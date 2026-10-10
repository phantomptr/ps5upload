import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api/ps5", () => ({
  appKill: vi.fn(),
  processKill: vi.fn(),
  processList: vi.fn(),
}));

import { appKill, processKill, processList, type ProcessInfo } from "../api/ps5";
import { killGame } from "./killGame";

const appKillM = vi.mocked(appKill);
const processKillM = vi.mocked(processKill);
const processListM = vi.mocked(processList);

function proc(p: Partial<ProcessInfo>): ProcessInfo {
  return {
    pid: 0,
    name: "",
    comm: "",
    title_id: "",
    app_id: 0,
    memory_mib: 0,
    threads: 1,
    kind: "app",
    ...p,
  };
}

describe("killGame", () => {
  beforeEach(() => {
    appKillM.mockReset();
    processKillM.mockReset();
    processListM.mockReset();
  });

  it("stops at Sony's app-kill when it works", async () => {
    appKillM.mockResolvedValue({ ok: true });
    expect(await killGame("a", { appId: 7, pid: 70 })).toBe(true);
    expect(processKillM).not.toHaveBeenCalled();
  });

  it("falls back to SIGKILL when app-kill throws (FW 12.20)", async () => {
    appKillM.mockRejectedValue(new Error("app_kill rejected"));
    processKillM.mockResolvedValue({ ok: true, pid: 0 });
    expect(await killGame("a", { appId: 7, pid: 70 })).toBe(true);
    expect(processKillM).toHaveBeenCalledWith("a", 70);
  });

  it("looks the pid up by app id when the caller has none", async () => {
    appKillM.mockResolvedValue({ ok: false });
    processListM.mockResolvedValue({
      truncated: false,
      processes: [
        proc({ pid: 5, app_id: 7, kind: "payload", is_self: true }),
        proc({ pid: 9, app_id: 7, kind: "app" }),
      ],
    });
    processKillM.mockResolvedValue({ ok: true, pid: 0 });
    expect(await killGame("a", { appId: 7 })).toBe(true);
    expect(processKillM).toHaveBeenCalledWith("a", 9);
  });

  it("reports failure when no process carries the app id", async () => {
    appKillM.mockRejectedValue(new Error("no"));
    processListM.mockResolvedValue({ truncated: false, processes: [proc({ pid: 9, app_id: 8 })] });
    expect(await killGame("a", { appId: 7 })).toBe(false);
    expect(processKillM).not.toHaveBeenCalled();
  });
});
