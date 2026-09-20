import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";

// pkgLibrary pulls in the Tauri invoke bridge + the ps5 api at module load;
// stub both so importing the store doesn't touch a real backend. Same
// pattern as pkgLibrary.test.ts.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));
vi.mock("../api/ps5", () => ({
  fsListDir: vi.fn(async () => []),
  fsDelete: vi.fn(async () => {}),
  fsMkdir: vi.fn(async () => {}),
  fsCopy: vi.fn(async () => {}),
  fsOpStatus: vi.fn(async () => ({ total_bytes: 0, bytes_copied: 0 })),
  pkgMetadataConsole: vi.fn(async () => null),
  toastPush: vi.fn(async () => ({ ok: true })),
  installFreeBytes: vi.fn(async () => 1_000_000_000_000),
  consoleReadiness: vi.fn(async () => true),
  pkgInstalledInventory: vi.fn(async () => []),
  pkgInstallPreflight: vi.fn(async () => null),
}));
vi.mock("../lib/ps5Transfers", () => ({ transferScreenBusy: () => false }));

import { invoke } from "@tauri-apps/api/core";
import { pkgInstalledInventory } from "../api/ps5";
import { installReverifyDelaysMs, runPkgInstall } from "./pkgLibrary";
import { useTaskStore } from "./tasks";

/* The re-verify must be cheap and must not give up early. A 100 GiB install on
 * a slow internal drive is the case this exists for: the engine's grace window
 * ends in minutes, the console keeps writing for far longer. */
describe("installReverifyDelaysMs", () => {
  it("starts quickly", () => {
    expect(installReverifyDelaysMs(0)).toBe(30_000);
  });

  it("backs off but stays bounded", () => {
    expect(installReverifyDelaysMs(1)).toBe(60_000);
    expect(installReverifyDelaysMs(2)).toBe(120_000);
    expect(installReverifyDelaysMs(3)).toBe(300_000);
  });

  it("caps at five minutes however many attempts have passed", () => {
    expect(installReverifyDelaysMs(4)).toBe(300_000);
    expect(installReverifyDelaysMs(99)).toBe(300_000);
  });

  it("never returns a non-positive delay", () => {
    for (let i = 0; i < 50; i++) {
      expect(installReverifyDelaysMs(i)).toBeGreaterThan(0);
    }
  });
});

// Regression: uploadQueue.ts calls runPkgInstall(..., null, ...) unconditionally
// (it has no PARAM.SFO category to send), so `packageType === null` is a live
// mainline case, not an edge case. The background re-verify must use the
// engine's OWN resolved package_type (authoritative — it inspects the staged
// pkg), never fall back to "" (which verifyDpiInstalledArtifact treats as the
// base-game category "gd" and so filters out the real DLC/patch artifact,
// stranding an accepted DLC/patch install in "awaiting" forever).
describe("runPkgInstall — hands the ENGINE's resolved package_type to the background re-verify", () => {
  const mockedInvoke = vi.mocked(invoke);
  const mockedInventory = vi.mocked(pkgInstalledInventory);
  const host = "192.168.1.50";
  const contentId = "IV0000-CUSA07842_00-0000000000000001";
  const localPath = "/user/data/dlc.pkg";
  const fingerprint = "f".repeat(64);

  beforeEach(() => {
    vi.useFakeTimers();
    useTaskStore.setState({ tasks: [] });
    mockedInventory.mockReset();
    mockedInvoke.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("matches the DLC artifact via the engine's package_type, not the caller's null", async () => {
    // The caller (mirroring uploadQueue.ts) sends packageType=null. The engine
    // resolves it from the staged pkg's own PARAM.SFO and reports it back as
    // "AC". A response with no session_id is accepted-but-unverified
    // immediately (see the "forwards deleteStaging" tests in
    // pkgLibrary.test.ts for the same fast path).
    mockedInvoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "pkg_install_start") {
        return {
          err_code: 0,
          register_path: "shellui-rpc",
          package_type: "AC",
        };
      }
      return {};
    });
    // Only a "dlc"-kind artifact is present — never a "base". If the reverify
    // used the caller's null (⇒ "" ⇒ category "gd" ⇒ kind "base"), it would
    // filter this artifact out entirely and never match.
    mockedInventory.mockResolvedValue([
      {
        kind: "dlc",
        path: `/user/addcont/CUSA07842/CUSA07842-AC0001/ac.pkg`,
        size: 12_345,
        fingerprint,
        contentId,
      },
    ]);

    const result = await runPkgInstall(
      host,
      localPath,
      contentId,
      null,
      true,
      undefined,
      undefined,
      { fingerprint },
    );

    expect(result.acceptedUnverified).toBe(true);
    expect(result.resolvedPackageType).toBe("AC");

    const taskId = useTaskStore.getState().tasks[0]?.id;
    expect(taskId).toBeTruthy();
    expect(useTaskStore.getState().tasks[0]?.status).toBe("awaiting");

    // Drive the background re-verify's first tick (installReverifyDelaysMs(0)).
    await vi.advanceTimersByTimeAsync(30_000);

    expect(useTaskStore.getState().getTask(taskId!)?.status).toBe("done");
  });
});
