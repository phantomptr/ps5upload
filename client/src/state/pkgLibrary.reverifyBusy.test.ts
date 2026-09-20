import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Same module-load stubs as pkgLibrary.reverify.test.ts, except that this
// console is permanently busy with a transfer — the case the give-up
// ordering exists for.
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
vi.mock("../lib/ps5Transfers", () => ({ transferScreenBusy: () => true }));

import { pkgInstalledInventory } from "../api/ps5";
import {
  INSTALL_REVERIFY_MAX_MS,
  scheduleInstallReverify,
} from "./pkgLibrary";
import { useTaskStore } from "./tasks";

describe("scheduleInstallReverify — a console that is never free", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useTaskStore.setState({ tasks: [] });
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  /* The busy gate postpones a tick instead of consuming it. If the expiry
   * check sat below that gate, a console whose transfer never ends would
   * re-arm the chain every 30s forever and the row would never become the
   * user's to resolve. */
  it("hands the row its Recheck control once the window closes", async () => {
    const taskId = useTaskStore.getState().registerTask({
      kind: "pkg-install",
      origin: "test",
      label: "Busy Console Game",
      consoleId: "10.0.0.5:9114",
      status: "awaiting",
    });

    scheduleInstallReverify({
      taskId,
      host: "10.0.0.5:9114",
      name: "Busy Console Game",
      contentId: "UP9000-PPSA03016_00-MARVELSSPIDERMAN2",
      packageType: "app",
      expected: { size: 101_000_000_000 },
    });

    // Well past the window, with the console busy for every single tick.
    await vi.advanceTimersByTimeAsync(INSTALL_REVERIFY_MAX_MS + 60_000);

    const task = useTaskStore.getState().getTask(taskId);
    // Still `awaiting` — the install may genuinely still be running — but now
    // carrying the control that lets the user resolve it.
    expect(task?.status).toBe("awaiting");
    expect(task?.control).toEqual({ owner: "pkg-install", taskId });
    // The busy gate really did hold: we never probed the console.
    expect(vi.mocked(pkgInstalledInventory)).not.toHaveBeenCalled();
  });

  /* Nothing was stopped, so the row must not say "Cancelled". */
  it("closes an unprobeable install as `unverified`, not `cancelled`", () => {
    const taskId = useTaskStore.getState().registerTask({
      kind: "pkg-install",
      origin: "test",
      label: "No Identity Game",
      consoleId: "10.0.0.5:9114",
      status: "awaiting",
    });

    scheduleInstallReverify({
      taskId,
      host: "10.0.0.5:9114",
      name: "No Identity Game",
      contentId: null,
      packageType: "app",
      expected: undefined,
    });

    expect(useTaskStore.getState().getTask(taskId)?.status).toBe("unverified");
  });
});
