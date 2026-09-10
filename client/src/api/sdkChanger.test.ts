import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { sdkScan, sdkPatch, sdkRestore } from "./ps5";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));

const mockedInvoke = vi.mocked(invoke);

describe("SDK Changer API", () => {
  beforeEach(() => {
    mockedInvoke.mockReset();
  });

  it("sdkScan sends addr", async () => {
    mockedInvoke.mockResolvedValueOnce({ titles: [] });
    await sdkScan("192.168.1.50:9021");
    expect(mockedInvoke).toHaveBeenCalledWith("sdk_scan", {
      req: { addr: "192.168.1.50:9021" },
    });
  });

  it("sdkScan defaults addr to null", async () => {
    mockedInvoke.mockResolvedValueOnce({ titles: [] });
    await sdkScan();
    expect(mockedInvoke).toHaveBeenCalledWith("sdk_scan", {
      req: { addr: null },
    });
  });

  it("sdkPatch sends title_id + target_sdk", async () => {
    mockedInvoke.mockResolvedValueOnce({ ok: true });
    await sdkPatch("CUSA00001_00", "0x09060000", "192.168.1.50:9021");
    expect(mockedInvoke).toHaveBeenCalledWith("sdk_patch", {
      req: {
        addr: "192.168.1.50:9021",
        title_id: "CUSA00001_00",
        target_sdk: "0x09060000",
        patch_libc: false,
      },
    });
  });

  it("sdkPatch carries the opt-in libc choice", async () => {
    mockedInvoke.mockResolvedValueOnce({ ok: true });
    await sdkPatch("PPSA25411", "0x04000031", "192.168.1.50:9021", true);
    expect(mockedInvoke).toHaveBeenCalledWith("sdk_patch", {
      req: {
        addr: "192.168.1.50:9021",
        title_id: "PPSA25411",
        target_sdk: "0x04000031",
        patch_libc: true,
      },
    });
  });

  it("sdkRestore sends title_id only", async () => {
    mockedInvoke.mockResolvedValueOnce({ ok: true, restored: 3 });
    await sdkRestore("CUSA00001_00", "192.168.1.50:9021");
    expect(mockedInvoke).toHaveBeenCalledWith("sdk_restore", {
      req: {
        addr: "192.168.1.50:9021",
        title_id: "CUSA00001_00",
      },
    });
  });

  it("sdkRestore defaults addr to null", async () => {
    mockedInvoke.mockResolvedValueOnce({ ok: true, restored: 0 });
    await sdkRestore("CUSA00001_00");
    expect(mockedInvoke).toHaveBeenCalledWith("sdk_restore", {
      req: { addr: null, title_id: "CUSA00001_00" },
    });
  });

  it("sdkRestore handles no-backup response", async () => {
    mockedInvoke.mockResolvedValueOnce({
      ok: true,
      restored: 0,
      error: "no .bak files found",
    });
    const result = await sdkRestore("CUSA99999_00");
    expect(result.ok).toBe(true);
    expect(result.restored).toBe(0);
    expect(result.error).toBe("no .bak files found");
  });

  it("sdkRestore handles error response", async () => {
    mockedInvoke.mockResolvedValueOnce({
      ok: false,
      error: "title not found",
    });
    const result = await sdkRestore("CUSA99999_00");
    expect(result.ok).toBe(false);
    expect(result.error).toBe("title not found");
  });
});
