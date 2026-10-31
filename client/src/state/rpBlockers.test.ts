import { beforeEach, describe, expect, it, vi } from "vitest";

// One fake blocker on a fake console, and a Remote Play store reduced to what the blockers
// watch: whether a stream runs, and for which console.
const h = vi.hoisted(() => {
  const calls: string[] = [];
  const state = { on: true };
  const fake = {
    id: "hush",
    name: "Hush",
    reasonKey: "rp_blocker_hush_reason",
    reason: "Hush is on.",
    detect: async () => state.on,
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
  detectBlockers: async () => ((await h.fake.detect()) ? [h.fake] : []),
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
  h.state.on = true;
  go.mockClear();
  useRpBlockers.setState({ autoPause: false, held: null, pausing: null, paused: {}, onAtStart: {} });
  rp.setState({ stream: null, target: null } as never);
});

describe("Remote Play blockers around a stream", () => {
  it("starts at once when nothing blocks", async () => {
    h.state.on = false;
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
});
