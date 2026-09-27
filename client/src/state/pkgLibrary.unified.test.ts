import { describe, expect, it, vi } from "vitest";

// pkgLibrary pulls in the Tauri invoke bridge + the ps5 api at module load;
// stub both so importing the store doesn't touch a real backend. Same pattern
// as pkgLibrary.test.ts.
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

import {
  PKG_MAY_NOT_LAUNCH_MESSAGE,
  PKG_PATCH_DID_NOT_APPLY_HINT,
  PKG_PATCH_REGRESSED_HINT,
  sampleFromStatus,
  statusToOutcome,
} from "./pkgLibrary";
import type { InstallStatus } from "../api/ps5";

/** A minimal terminal status; the tests override the fields under test. */
function status(over: Partial<InstallStatus>): InstallStatus {
  return {
    job: "j1",
    ps5_addr: "192.168.1.50:9114",
    content_id: "CID",
    title_id: null,
    phase: "done",
    route: "loopback",
    verdict: "installed",
    code: 0,
    hint: null,
    reason: null,
    metrics: {
      total_bytes: 0,
      served_bytes: 0,
      throughput_mbps: 0,
      phase_ms: {},
      retries: 0,
      sony_rc: 0,
    },
    app_ver_before: null,
    app_ver_after: null,
    patch_verdict: null,
    shortened: false,
    started_at: 0,
    updated_at: 0,
    ...over,
  };
}

describe("statusToOutcome — maps the unified verdict to the UI outcome", () => {
  it("verdict=installed → installed, launchable", () => {
    const o = statusToOutcome(status({ verdict: "installed" }));
    expect(o.installed).toBe(true);
    expect(o.mayNotLaunch).toBe(false);
    expect(o.errMessage).toBe("");
  });

  it("verdict=may_not_launch → installed but cautioned", () => {
    const o = statusToOutcome(status({ verdict: "may_not_launch" }));
    // may_not_launch means the artifact IS on disk (deletable staging), but the
    // title may not start — so it is a success WITH a warning, never a failure.
    expect(o.installed).toBe(true);
    expect(o.mayNotLaunch).toBe(true);
  });

  it("verdict=failed → not installed, carries the engine hint", () => {
    const o = statusToOutcome(
      status({ phase: "failed", verdict: "failed", hint: "Sony refused it" }),
    );
    expect(o.installed).toBe(false);
    expect(o.errMessage).toBe("Sony refused it");
  });

  it("a regressed patch gets the update-specific copy, not a raw error", () => {
    const o = statusToOutcome(
      status({ phase: "failed", verdict: "failed", patch_verdict: "regressed" }),
    );
    expect(o.installed).toBe(false);
    expect(o.errMessage).toBe(PKG_PATCH_REGRESSED_HINT);
  });

  it("a patch that did not apply gets its own guidance", () => {
    const o = statusToOutcome(
      status({
        phase: "failed",
        verdict: "failed",
        patch_verdict: "did_not_apply",
      }),
    );
    expect(o.errMessage).toBe(PKG_PATCH_DID_NOT_APPLY_HINT);
  });

  it("falls back to the hex code when the engine gave no hint", () => {
    const o = statusToOutcome(
      status({ phase: "failed", verdict: "failed", hint: null, code: 0x80b2116f }),
    );
    expect(o.errMessage).toBe("0x80b2116f");
  });
});

describe("sampleFromStatus — drives the live progress bar from metrics", () => {
  it("uses served/total bytes for progress", () => {
    const s = sampleFromStatus(
      status({
        phase: "install",
        metrics: {
          total_bytes: 1000,
          served_bytes: 400,
          throughput_mbps: 10,
          phase_ms: {},
          retries: 0,
          sony_rc: 0,
        },
      }),
    );
    expect(s.total).toBe(1000);
    expect(Math.max(s.transferBytes, s.installedBytes)).toBe(400);
  });

  it("is never stalled/unverified — those states are gone", () => {
    const s = sampleFromStatus(status({ phase: "deliver" }));
    expect(s.stalled).toBe(false);
    expect(s.acceptedUnverified).toBe(false);
  });
});

// Keep a reference to the imported message so an unused-import lint can't fire
// if a future refactor drops one of the assertions above.
expect(typeof PKG_MAY_NOT_LAUNCH_MESSAGE).toBe("string");
