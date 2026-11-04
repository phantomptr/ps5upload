import { describe, expect, it, vi } from "vitest";

vi.mock("../api/ps5", () => ({
  ddpStatus: vi.fn(),
  payloadCheck: vi.fn(),
  portProbe: vi.fn(),
}));

import { checkConsole, closedPortText, type ConsoleCheckDeps } from "./consoleCheck";
import type { Translator } from "../state/lang";

type Helper = Awaited<ReturnType<ConsoleCheckDeps["helper"]>>;

const asleep = 620;
const awake = 200;

function deps(over: {
  open?: number[];
  helper?: Partial<Helper> | Error;
  ddp?: number;
  probeThrows?: boolean;
}): ConsoleCheckDeps & { probed: number[] } {
  const probed: number[] = [];
  return {
    probed,
    probe: async (_h, port) => {
      probed.push(port);
      if (over.probeThrows) throw new Error("engine is away");
      return (over.open ?? []).includes(port)
        ? { open: true, error: null }
        : { open: false, error: "Connection refused" };
    },
    helper: async () => {
      if (over.helper instanceof Error) throw over.helper;
      return { reachable: false, engineReachable: true, ...over.helper } as Helper;
    },
    ddpCode: async () => over.ddp ?? awake,
  };
}

const tr = ((key: string, a?: unknown, b?: unknown) =>
  (typeof a === "string" ? a : typeof b === "string" ? b : key)) as Translator;

describe("checkConsole", () => {
  // Pressing Check on a connected console used to reset step 2 and never read the helper.
  it("reads the helper even when the loader answers", async () => {
    const r = await checkConsole("h", deps({ open: [9021], helper: { reachable: true } }));
    expect(r).toMatchObject({ kind: "loader", helperRunning: true });
  });

  it("reports an open loader with no helper as just that", async () => {
    const r = await checkConsole("h", deps({ open: [9021] }));
    expect(r).toMatchObject({ kind: "loader", helperRunning: false });
  });

  it("does not call a running helper with a closed loader a failure", async () => {
    const r = await checkConsole("h", deps({ helper: { reachable: true } }));
    expect(r.kind).toBe("helper");
  });

  it("counts a helper that wants pairing as running", async () => {
    const r = await checkConsole("h", deps({ helper: { session: "needs_pairing" } as Partial<Helper> }));
    expect(r.kind).toBe("helper");
  });

  it("says rest mode, and asks nothing else of a resting console", async () => {
    const d = deps({ helper: { power: "rest" } as Partial<Helper> });
    expect((await checkConsole("h", d)).kind).toBe("rest");
    expect(d.probed).toEqual([9021]);
  });

  it("asks DDP when the helper's reply did not say", async () => {
    expect((await checkConsole("h", deps({ ddp: asleep }))).kind).toBe("rest");
  });

  // #344: with :9021 dead and Payload Manager up, the helper can still be sent.
  it("treats Payload Manager as a usable loader on a console that is on", async () => {
    const r = await checkConsole(
      "h",
      deps({ open: [8084], helper: { power: "awake" } as Partial<Helper> }),
    );
    expect(r.kind).toBe("payload-manager");
  });

  it("names the missing loader on a console that is on, not the network", async () => {
    const r = await checkConsole("h", deps({ helper: { power: "awake" } as Partial<Helper> }));
    expect(r.kind).toBe("no-loader");
    const text = closedPortText(tr, "h", r as Extract<typeof r, { kind: "no-loader" }>, "");
    expect(text).toContain("loader");
    expect(text).not.toContain("VPN");
  });

  it("takes a refused helper port as a console that is on", async () => {
    const r = await checkConsole(
      "h",
      deps({ ddp: 0, helper: { power: "unknown", reason: "helper_not_ava1" } as unknown as Partial<Helper> }),
    );
    expect(r.kind).toBe("no-loader");
  });

  it("leaves a console that does not answer to the network hints, and probes nothing more", async () => {
    const d = deps({ ddp: 0, helper: { power: "unreachable" } as Partial<Helper> });
    const r = await checkConsole("h", d);
    expect(r.kind).toBe("unreachable");
    expect(d.probed).toEqual([9021]);
    const text = closedPortText(tr, "h", r as Extract<typeof r, { kind: "unreachable" }>, "");
    expect(text).toContain("VPN");
  });

  // The web UI's probe goes through the engine: a throw used to leave Check spinning.
  it("answers when the probe itself throws", async () => {
    const r = await checkConsole("h", deps({ probeThrows: true, helper: new Error("away"), ddp: 0 }));
    expect(r).toEqual({ kind: "unreachable", error: "engine is away" });
  });
});
