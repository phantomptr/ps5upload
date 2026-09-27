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
import {
  installReverifyDelaysMs,
  retryInstallReverify,
} from "./pkgLibrary";
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

// Regression: the Recheck action (retryInstallReverify) rebuilds its args
// from task.payload alone. registerTask's payload is only
// { localPs5Path, contentId, packageType, deleteStaging, expected } — it
// never carried the engine's resolved package_type, so a Recheck on a task
// whose ORIGINAL schedule had packageType === null (the uploadQueue.ts
// mainline) would re-probe with "" instead of the engine's real type. A test
// that only drove the first schedule (above) would not catch this: it never
// exercises payload round-tripping through the store. This one does.
describe("retryInstallReverify — Recheck probes with the engine's resolved package_type", () => {
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

  it("uses resolvedPackageType from payload, not the original null packageType", async () => {
    // Only a "dlc"-kind artifact is present. If the Recheck fell back to the
    // caller's null (⇒ "" ⇒ category "gd" ⇒ kind "base"), this would never
    // match and the row would stay stuck in "awaiting" forever.
    mockedInventory.mockResolvedValue([
      {
        kind: "dlc",
        path: `/user/addcont/CUSA07842/CUSA07842-AC0001/ac.pkg`,
        size: 12_345,
        fingerprint,
        contentId,
      },
    ]);

    // Mirrors what runPkgInstall's registerTask + the `awaiting` transition
    // now persist: the caller's original (null) packageType alongside the
    // engine-resolved type from PkgInstallOutcome.
    const taskId = useTaskStore.getState().registerTask({
      kind: "pkg-install",
      origin: "pkg.install",
      label: "Installing dlc.pkg",
      detail: localPath,
      consoleId: host,
      status: "awaiting",
      payload: {
        localPs5Path: localPath,
        contentId,
        packageType: null,
        deleteStaging: true,
        expected: { fingerprint },
        resolvedPackageType: "AC",
        mayNotLaunch: false,
      },
    });
    const task = useTaskStore.getState().getTask(taskId)!;

    const started = retryInstallReverify({
      id: task.id,
      label: task.label,
      consoleId: task.consoleId,
      payload: task.payload,
    });
    expect(started).toBe(true);

    await vi.advanceTimersByTimeAsync(30_000);

    expect(useTaskStore.getState().getTask(taskId)?.status).toBe("done");
  });
});
