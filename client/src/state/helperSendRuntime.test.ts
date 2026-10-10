import { beforeEach, describe, expect, it, vi } from "vitest";

const { runHelperSend } = vi.hoisted(() => ({ runHelperSend: vi.fn() }));
vi.mock("./helperSend", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./helperSend")>()),
  runHelperSend,
}));

import { useHelperSendStore } from "./helperSend";
import { loaderHint, sendHelperAndWait } from "./helperSendRuntime";
import type { Translator } from "./lang";

const tr = ((_k: string, a?: unknown, b?: string) =>
  typeof a === "string" ? a : (b ?? _k)) as Translator;

describe("loaderHint", () => {
  it("turns a refused loader port into what to do, keeping the original error", () => {
    const raw = "connect 192.168.1.128:9021: Connection refused (os error 111)";
    const msg = loaderHint(raw);
    expect(msg).toContain("loader (port 9021) isn't running");
    expect(msg).toContain(raw);
  });

  it("leaves any other error as it is", () => {
    expect(loaderHint("the engine has no helper to send")).toBe(
      "the engine has no helper to send",
    );
  });
});

describe("sendHelperAndWait", () => {
  const H = "192.168.0.9";
  beforeEach(() => {
    runHelperSend.mockReset();
    useHelperSendStore.setState({ byHost: {} });
  });

  it("goes through the shared send and resolves null when the helper answered", async () => {
    runHelperSend.mockResolvedValue("ok");
    expect(await sendHelperAndWait(` ${H} `, tr)).toBeNull();
    expect(runHelperSend).toHaveBeenCalledTimes(1);
    expect(runHelperSend.mock.calls[0][0]).toBe(H);
  });

  it("returns what the failed send left for this console", async () => {
    runHelperSend.mockImplementation(async (host: string) => {
      useHelperSendStore.getState().put(host, { state: "fail", phase: null, msg: "Pair with your PS5", startedAtMs: 0 });
      return "fail";
    });
    expect(await sendHelperAndWait(H, tr)).toBe("Pair with your PS5");
  });

  it("waits out a send already running instead of reporting busy", async () => {
    useHelperSendStore.getState().put(H, { state: "busy", phase: "waiting", msg: "…", startedAtMs: 0 });
    runHelperSend.mockResolvedValue("busy");
    let slept = 0;
    const out = await sendHelperAndWait(H, tr, async () => {
      slept += 1;
      // The running send ends well: its entry goes.
      useHelperSendStore.getState().put(H, null);
    });
    expect(slept).toBe(1);
    expect(out).toBeNull();
  });

  it("reports the joined send's failure", async () => {
    useHelperSendStore.getState().put(H, { state: "busy", phase: "waiting", msg: "…", startedAtMs: 0 });
    runHelperSend.mockResolvedValue("busy");
    const out = await sendHelperAndWait(H, tr, async () => {
      useHelperSendStore.getState().put(H, { state: "fail", phase: null, msg: "timed out", startedAtMs: 0 });
    });
    expect(out).toBe("timed out");
  });
});
