import { beforeEach, describe, expect, it, vi } from "vitest";

// One fake blocker on a fake console, and a Remote Play store reduced to what the blockers
// watch: whether a stream runs, and for which console.
const h = vi.hoisted(() => {
  const calls: string[] = [];
  type Status =
    | { kind: "off" }
    | { kind: "on" }
    | { kind: "wait"; maxMs: number }
    | { kind: "stuck"; key: string; text: string };
  const state: { status: Status; waitOutcome: "clear" | "timeout"; waitGate: Promise<void> | null } = {
    status: { kind: "on" },
    waitOutcome: "clear",
    waitGate: null,
  };
  const fake = {
    id: "hush",
    name: "Hush",
    reasonKey: "rp_blocker_hush_reason",
    reason: "Hush is on.",
    status: async () => state.status,
    waitClear: async (_ctx: unknown, maxMs: number, opts?: { signal?: AbortSignal }) => {
      calls.push(`wait ${maxMs}`);
      if (state.waitGate) await state.waitGate;
      return opts?.signal?.aborted ? ("timeout" as const) : state.waitOutcome;
    },
    pause: async () => {
      calls.push("pause");
      return "restored" as const;
    },
    resume: async () => {
      calls.push("resume");
      return "running" as const;
    },
  };
  return { calls, state, fake };
});
vi.mock("../lib/rpBlockers", () => ({
  RP_BLOCKERS: [h.fake],
  blockerById: (id: string) => (id === "hush" ? h.fake : null),
  blockerCtx: (host: string) => ({ host, addr: host, procs: async () => [] }),
  checkBlockers: async () => {
    const status = await h.fake.status();
    return status.kind === "off" ? [] : [{ blocker: h.fake, status }];
  },
}));
vi.mock("./logs", () => ({ log: { info: vi.fn(), warn: vi.fn(), error: vi.fn() } }));
vi.mock("./notifications", () => ({ pushNotification: vi.fn() }));
vi.mock("./remotePlay", async () => {
  const { create } = await import("zustand");
  return {
    useRemotePlayStore: create<{ stream: object | null; target: { host: string } | null }>(() => ({
      stream: null,
      target: null,
    })),
  };
});

import { useRemotePlayStore as rp } from "./remotePlay";
import {
  beforeRemotePlay,
  dropHeld,
  pauseHeldAndStart,
  startHeldAnyway,
  useRpBlockers,
} from "./rpBlockers";

const calls = h.calls;
const HOST = "192.168.1.99";

/** The stream start: a stream begins on the console. */
const go = vi.fn(async () => {
  calls.push("start");
  rp.setState({ stream: {}, target: { host: HOST } } as never);
});

beforeEach(() => {
  calls.length = 0;
  h.state.status = { kind: "on" };
  h.state.waitOutcome = "clear";
  h.state.waitGate = null;
  go.mockClear();
  useRpBlockers.setState({ autoPause: false, held: null, pausing: null, waiting: null, paused: {}, onAtStart: {} });
  rp.setState({ stream: null, target: null } as never);
});

describe("Remote Play blockers around a stream", () => {
  it("starts at once when nothing blocks", async () => {
    h.state.status = { kind: "off" };
    await beforeRemotePlay(HOST, go);
    expect(calls).toEqual(["start"]);
    expect(useRpBlockers.getState().onAtStart[HOST]).toEqual([]);
  });

  it("holds the start and asks when Hush is on", async () => {
    await beforeRemotePlay(HOST, go);
    expect(go).not.toHaveBeenCalled();
    expect(useRpBlockers.getState().held?.ids).toEqual(["hush"]);
    expect(useRpBlockers.getState().onAtStart[HOST]).toEqual(["hush"]);
    dropHeld();
    expect(useRpBlockers.getState().held).toBeNull();
  });

  it("starts anyway without touching Hush, and leaves it alone when the stream ends", async () => {
    await beforeRemotePlay(HOST, go);
    await startHeldAnyway();
    rp.setState({ stream: null } as never);
    await Promise.resolve();
    expect(calls).toEqual(["start"]);
  });

  it("pauses, starts, and turns Hush back on when the stream ends", async () => {
    await beforeRemotePlay(HOST, go);
    await pauseHeldAndStart();
    expect(calls).toEqual(["pause", "start"]);
    expect(useRpBlockers.getState().paused[HOST]).toEqual(["hush"]);
    rp.setState({ stream: null } as never);
    await vi.waitFor(() => expect(calls).toEqual(["pause", "start", "resume"]));
    expect(useRpBlockers.getState().paused[HOST]).toBeUndefined();
  });

  it("pauses without asking when Always pause is on", async () => {
    useRpBlockers.getState().setAutoPause(true);
    await beforeRemotePlay(HOST, go);
    expect(calls).toEqual(["pause", "start"]);
    expect(useRpBlockers.getState().held).toBeNull();
  });

  it("turns Hush back on at once when the start did not become a stream", async () => {
    useRpBlockers.getState().setAutoPause(true);
    await beforeRemotePlay(HOST, async () => {
      calls.push("start");
    });
    expect(calls).toEqual(["pause", "start", "resume"]);
  });

  it("waits out a hold, then starts without pausing Hush", async () => {
    h.state.status = { kind: "wait", maxMs: 50_000 };
    let open!: () => void;
    h.state.waitGate = new Promise<void>((r) => (open = r));
    const started = beforeRemotePlay(HOST, go);
    await vi.waitFor(() => expect(useRpBlockers.getState().waiting).toMatchObject({ ids: ["hush"], maxS: 50 }));
    // A second press while waiting does not start a second wait.
    await beforeRemotePlay(HOST, go);
    open();
    await started;
    expect(calls).toEqual(["wait 50000", "start"]);
    expect(useRpBlockers.getState().waiting).toBeNull();
    expect(useRpBlockers.getState().onAtStart[HOST]).toEqual([]);
    expect(useRpBlockers.getState().paused[HOST]).toBeUndefined();
  });

  it("treats a hold that outlasts its bound as on: asks, as before", async () => {
    h.state.status = { kind: "wait", maxMs: 50_000 };
    h.state.waitOutcome = "timeout";
    await beforeRemotePlay(HOST, go);
    expect(go).not.toHaveBeenCalled();
    expect(useRpBlockers.getState().held?.ids).toEqual(["hush"]);
    await pauseHeldAndStart();
    expect(calls).toEqual(["wait 50000", "pause", "start"]);
  });

  it("does not start when the wait is cancelled", async () => {
    h.state.status = { kind: "wait", maxMs: 50_000 };
    let open!: () => void;
    h.state.waitGate = new Promise<void>((r) => (open = r));
    const started = beforeRemotePlay(HOST, go);
    await vi.waitFor(() => expect(useRpBlockers.getState().waiting).not.toBeNull());
    useRpBlockers.getState().waiting!.cancel();
    open();
    await started;
    expect(calls).toEqual(["wait 50000"]);
    expect(useRpBlockers.getState().waiting).toBeNull();
    expect(useRpBlockers.getState().held).toBeNull();
  });

  it("holds the start for a stuck Hush even with Always pause on, and never pauses it", async () => {
    useRpBlockers.getState().setAutoPause(true);
    h.state.status = { kind: "stuck", key: "rp_blocker_hush_leftover_restart", text: "Restart the PS5 to clear it." };
    await beforeRemotePlay(HOST, go);
    const held = useRpBlockers.getState().held;
    expect(held?.ids).toEqual([]);
    expect(held?.stuck).toEqual([{ id: "hush", key: "rp_blocker_hush_leftover_restart", text: "Restart the PS5 to clear it." }]);
    expect(useRpBlockers.getState().onAtStart[HOST]).toEqual(["hush"]);
    await startHeldAnyway();
    expect(calls).toEqual(["start"]);
  });
});
