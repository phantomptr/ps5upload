import { beforeEach, describe, expect, it, vi } from "vitest";

const { runHelperSend } = vi.hoisted(() => ({ runHelperSend: vi.fn() }));
vi.mock("./helperSend", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./helperSend")>()),
  runHelperSend,
}));

const { updateHelperCopiesAfterSend } = vi.hoisted(() => ({
  updateHelperCopiesAfterSend: vi.fn(),
}));
vi.mock("./helperStoredCopiesRuntime", () => ({ updateHelperCopiesAfterSend }));

vi.mock("../lib/appVersion", () => ({ getAppVersion: async () => "5.37.1" }));

import { useConnectionStore } from "./connection";
import { useHelperSendStore } from "./helperSend";
import { loaderHint, sendHelperAndWait, sendHelperTo } from "./helperSendRuntime";
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

  it("reads the desktop send's own timeout wording too", () => {
    expect(loaderHint("connect 1.2.3.4:9021: timeout")).toContain("isn't running");
  });

  it("leaves any other error as it is", () => {
    expect(loaderHint("the engine has no helper to send")).toBe(
      "the engine has no helper to send",
    );
  });
});

describe("sendHelperTo", () => {
  it("says a silent :9021 may be a jailbreak's own listener that needs elfldr first", async () => {
    runHelperSend.mockResolvedValue("fail");
    await sendHelperTo("192.168.0.9", tr);
    const text = runHelperSend.mock.calls[0][2] as { stuck: string };
    expect(text.stuck).toContain("stuck");
    expect(text.stuck).toContain("elfldr-ps5.elf");
  });
});

describe("sendHelperTo and the copies the PS5 starts at boot", () => {
  beforeEach(() => {
    runHelperSend.mockReset();
    updateHelperCopiesAfterSend.mockReset();
  });

  it("updates them once the new helper answered", async () => {
    runHelperSend.mockResolvedValue("ok");
    updateHelperCopiesAfterSend.mockResolvedValue([]);
    expect(await sendHelperTo("192.168.0.9", tr)).toBe("ok");
    expect(updateHelperCopiesAfterSend).toHaveBeenCalledTimes(1);
    expect(updateHelperCopiesAfterSend.mock.calls[0][0]).toBe("192.168.0.9");
  });

  it("leaves them alone when the send failed", async () => {
    runHelperSend.mockResolvedValue("fail");
    expect(await sendHelperTo("192.168.0.9", tr)).toBe("fail");
    expect(updateHelperCopiesAfterSend).not.toHaveBeenCalled();
  });

  it("a failure there does not fail the send", async () => {
    runHelperSend.mockResolvedValue("ok");
    updateHelperCopiesAfterSend.mockRejectedValue(new Error("console went away"));
    expect(await sendHelperTo("192.168.0.9", tr)).toBe("ok");
  });
});

describe("sendHelperAndWait", () => {
  const H = "192.168.0.9";
  beforeEach(() => {
    runHelperSend.mockReset();
    useHelperSendStore.setState({ byHost: {} });
  });

  // "Run again" and "Send only ps5upload" restarted a current helper and stopped uploads.
  it("leaves a helper of this app's version running, and says it is up", async () => {
    const out = await sendHelperAndWait(H, tr, undefined, async () => ({
      reachable: true,
      payloadVersion: "5.37.1",
    }));
    expect(out).toBeNull();
    expect(runHelperSend).not.toHaveBeenCalled();
    expect(useConnectionStore.getState().runtimeByHost[H]?.payloadStatus).toBe("up");
  });

  it("does not put this app's helper over a newer one", async () => {
    const guard = vi.fn(async () => true);
    const out = await sendHelperAndWait(
      H,
      tr,
      undefined,
      async () => ({ reachable: true, payloadVersion: "99.0.0" }),
      guard,
    );
    expect(out).toBeNull();
    expect(guard).not.toHaveBeenCalled();
    expect(runHelperSend).not.toHaveBeenCalled();
  });

  it("asks before replacing an older helper, and stops if told no", async () => {
    const out = await sendHelperAndWait(
      H,
      tr,
      undefined,
      async () => ({ reachable: true, payloadVersion: "1.0.0" }),
      async () => false,
    );
    expect(out).toContain("left as it is");
    expect(runHelperSend).not.toHaveBeenCalled();
  });

  it("replaces an older helper once allowed", async () => {
    runHelperSend.mockResolvedValue("ok");
    const guard = vi.fn(async () => true);
    const out = await sendHelperAndWait(
      H,
      tr,
      undefined,
      async () => ({ reachable: true, payloadVersion: "1.0.0" }),
      guard,
    );
    expect(out).toBeNull();
    expect(guard).toHaveBeenCalledWith(H);
    expect(runHelperSend).toHaveBeenCalledTimes(1);
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
    const out = await sendHelperAndWait(
      H,
      tr,
      async () => {
        slept += 1;
        // The running send ends well: its entry goes.
        useHelperSendStore.getState().put(H, null);
      },
      async () => ({ reachable: true }),
    );
    expect(slept).toBe(1);
    expect(out).toBeNull();
  });

  it("does not count a vanished record as success when the helper does not answer", async () => {
    useHelperSendStore.getState().put(H, { state: "busy", phase: "waiting", msg: "…", startedAtMs: 0 });
    runHelperSend.mockResolvedValue("busy");
    const out = await sendHelperAndWait(
      H,
      tr,
      async () => {
        // The joined send failed and its record was dismissed.
        useHelperSendStore.getState().put(H, null);
      },
      async () => ({ reachable: false, error: "connection refused" }),
    );
    expect(out).toContain("helper didn't start");
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

describe("an explicit send after Stop helper (#471)", () => {
  it("clears the console's 'stopped by you' mark so the reconnect loop looks after it again", async () => {
    const { markHelperStopped, isHelperStopped } = await import("./helperStopped");
    const { sendHelperTo } = await import("./helperSendRuntime");
    runHelperSend.mockReset();
    runHelperSend.mockResolvedValue("ok");
    markHelperStopped("192.168.0.9");
    markHelperStopped("192.168.0.10");
    await sendHelperTo(" 192.168.0.9 ", tr);
    expect(isHelperStopped("192.168.0.9")).toBe(false);
    // Only the console sent to.
    expect(isHelperStopped("192.168.0.10")).toBe(true);
  });
});
