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
  pkgLibraryStore,
  PKG_MAY_NOT_LAUNCH_MESSAGE,
  PKG_PATCH_DID_NOT_APPLY_HINT,
  PKG_PATCH_REGRESSED_HINT,
  sampleFromStatus,
  statusToOutcome,
} from "./pkgLibrary";
import type { InstallStatus } from "../api/ps5";
import { registerInstallEnqueuer } from "./consoleQueueBridge";
import type { InstallRequest } from "./consoleQueueBridge";

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

  it("a plain refusal keeps Sony's code, the only thing that tells one from another (#399)", () => {
    const o = statusToOutcome(
      status({ phase: "failed", verdict: "failed", reason: "sony_refused", code: 0x80b21106 }),
    );
    expect(o.errMessage).toBe("The PS5 declined the install. (0x80b21106)");
  });

  it("a stream the PS5 never reached gets the app's guidance and the code, not the engine's English", () => {
    const o = statusToOutcome(
      status({
        phase: "failed",
        verdict: "failed",
        reason: "stream_unreachable",
        code: 0x80431064,
        hint: "The PS5 never reached this computer at http://x (0x80431064)…",
      }),
    );
    expect(o.errMessage).toContain("firewall");
    expect(o.errMessage).toContain("Upload & install");
    expect(o.errMessage).toContain("0x80431064");
    expect(o.errMessage).not.toContain("http://x");
  });

  it("the engine's host-IP guidance (container / VPN address) is never replaced by the generic text", () => {
    // U1: a Docker engine advertising 172.17.x gave a Sony code AND a hint
    // naming PS5UPLOAD_PKG_HOST_IP; the app's static firewall wording used to
    // replace it, hiding the one thing that fixes it.
    const hint =
      "The PS5 never reached the engine at http://172.17.0.2:19113 to fetch the package (0x80431064). The engine is running in a container… set PS5UPLOAD_PKG_HOST_IP to the Docker host's LAN IP";
    const o = statusToOutcome(
      status({
        phase: "failed",
        verdict: "failed",
        reason: "stream_unreachable",
        code: 0x80431064,
        hint,
      }),
    );
    expect(o.errMessage).toBe(hint);
    expect(o.errMessage).toContain("PS5UPLOAD_PKG_HOST_IP");
  });

  it("the engine's own reach-check finding is shown as is", () => {
    // #342: the console could not connect back to the PC. No Sony code — the
    // engine's reach check found it and names the likely cause.
    const hint =
      "The PS5 cannot connect to this computer at http://192.168.137.1:19113 (timed out after 4000 ms)…";
    const o = statusToOutcome(
      status({ phase: "failed", verdict: "failed", reason: "stream_unreachable", code: 0, hint }),
    );
    expect(o.errMessage).toBe(hint);
  });

  it("a proxy refusal says to turn the PS5's proxy off", () => {
    const o = statusToOutcome(
      status({ phase: "failed", verdict: "failed", reason: "stream_proxy", code: 0x80431084 }),
    );
    expect(o.errMessage).toMatch(/Proxy Server/);
    expect(o.errMessage).toContain("0x80431084");
  });

  it("a staged refusal sends the user to Stream, not \"the PS5 declined\"", () => {
    // A phone can only stage; every install a user made from one failed with
    // this code while the same package streamed from a PC installed.
    const o = statusToOutcome(
      status({ phase: "failed", verdict: "failed", reason: "staged_refused", code: 0x80b2116f }),
    );
    expect(o.errMessage).toContain("Stream & install");
    expect(o.errMessage).toContain("0x80b2116f");
    expect(o.errMessage).not.toContain("declined");
  });

  it("a stalled delivery uses the app's own words", () => {
    const o = statusToOutcome(
      status({ phase: "failed", verdict: "failed", reason: "stalled", hint: "the console stopped fetching" }),
    );
    expect(o.errMessage).toMatch(/^The PS5 stopped fetching/);
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

describe("Retry with Stream (review 015 #04 §3)", () => {
  it("the outcome carries the retry only when the engine offers it on a failure", () => {
    const failed = status({
      phase: "failed",
      verdict: "failed",
      reason: "staged_refused",
      code: 0x80b2116f,
      retry_with_stream: true,
    });
    expect(statusToOutcome(failed).retryWithStream).toBe(true);
    // An unrelated refusal: the engine does not set it, so no action.
    const other = status({ phase: "failed", verdict: "failed", reason: "sony_refused", code: 0x80b21401 });
    expect(statusToOutcome(other).retryWithStream).toBeUndefined();
    // A success never offers it, whatever the flag says.
    expect(statusToOutcome(status({ retry_with_stream: true })).retryWithStream).toBeUndefined();
  });
});

describe("retryWithStream store action", () => {
  const HOST = "10.9.9.9";
  const entry = (path: string, status: "idle" | "installing" | "queued") =>
    ({ path, status, title: path, lastResult: { ok: false, message: "x", retryWithStream: true } }) as never;

  it("queues the same package once, as a forced stream, and never while it is already queued or running", async () => {
    const seen: InstallRequest[] = [];
    registerInstallEnqueuer((input) => {
      seen.push(input.request);
      return { id: "q", done: Promise.resolve({ ok: true }) };
    });
    const store = pkgLibraryStore(HOST);
    store.setState({ entries: [entry("/a.pkg", "idle"), entry("/b.pkg", "installing"), entry("/c.pkg", "queued")] });
    expect((await store.getState().retryWithStream("/a.pkg", HOST)).ok).toBe(true);
    expect(seen).toEqual([{ via: "library", path: "/a.pkg", forceStream: true }]);
    // The failure that offered the button is cleared as the retry starts.
    expect(store.getState().entries.find((e) => e.path === "/a.pkg")?.lastResult).toBeUndefined();
    for (const p of ["/b.pkg", "/c.pkg"]) {
      const r = await store.getState().retryWithStream(p, HOST);
      expect(r.ok).toBe(false);
    }
    expect(seen).toHaveLength(1);
  });
});

describe("after one staged refusal (Discord 2026-10-04: F3.3)", () => {
  const HOST = "10.9.9.10";

  it("the outcome says the console refused its own storage, and keeps the engine's hint when Windows named the cause", () => {
    const refused = statusToOutcome(
      status({
        phase: "failed",
        verdict: "failed",
        reason: "staged_refused",
        code: 0x80b2116f,
        retry_with_stream: true,
      }),
    );
    expect(refused.stagedRefused).toBe(true);
    // not for other refusals, and never on success
    expect(
      statusToOutcome(status({ phase: "failed", verdict: "failed", reason: "sony_refused" })).stagedRefused,
    ).toBeUndefined();
    expect(statusToOutcome(status({ reason: "staged_refused" })).stagedRefused).toBeUndefined();

    const hint = "The PS5 never reached this computer. “Ethernet 3” (192.168.88.1) is on a Public network.";
    const diag = {
      adapter: "Ethernet 3",
      local_ip: "192.168.88.1",
      category: "public" as const,
      firewall_enabled: true,
      allowed_by_rule: false,
    };
    const o = statusToOutcome(
      status({
        phase: "failed",
        verdict: "failed",
        reason: "stream_unreachable",
        code: 0x80431068,
        hint,
        net_diag: diag,
      }),
    );
    // the engine's sentence (adapter and category), not the generic firewall paragraph
    expect(o.errMessage).toBe(hint);
    expect(o.netDiag).toEqual(diag);
  });

  it("Retry with Stream still goes out when the engine did not offer it", async () => {
    const seen: InstallRequest[] = [];
    registerInstallEnqueuer((input) => {
      seen.push(input.request);
      return { id: "q", done: Promise.resolve({ ok: true }) };
    });
    const store = pkgLibraryStore(HOST);
    store.setState({
      entries: [
        { path: "/patch.pkg", status: "idle", title: "p", category: "gp", lastResult: { ok: false, message: "x", stagedRefused: true } },
      ] as never,
    });
    const r = await store.getState().retryWithStream("/patch.pkg", HOST);
    expect(r.ok).toBe(true);
    expect(seen).toHaveLength(1);
  });
});
