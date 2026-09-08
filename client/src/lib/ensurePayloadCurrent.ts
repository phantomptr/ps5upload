import { bundledPayloadPath, payloadCheck, sendPayload } from "../api/ps5";
import { getAppVersion } from "./appVersion";
import { isTauriEnv } from "./tauriEnv";
import { restoreMainPayload } from "./restoreMainPayload";
import { compareVersions } from "./semver";
import { log } from "../state/logs";
import { prearmDpiDaemon } from "./prearmDpi";

export type EnsurePayloadResult =
  | "current"
  | "pushed"
  | "stale-ok"
  | "no-push";

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

/**
 * Make sure the PS5 is running the payload that matches THIS app build,
 * pushing the bundled ELF if it's missing or a different version.
 *
 * Why both queues need this: each payload release carries server-side
 * hardening (e.g. v2.23.1 widened the mgmt accept backlog 8→128 and added
 * the reconcile connection-storm mitigations). Those only take effect once
 * the *payload itself* is redeployed. The install queue already did this; the
 * upload queue did not — so a queue run could hammer an old, fragile payload
 * with per-directory mgmt connections during reconcile and crash it after the
 * first job. Routing both queues through one check closes that gap.
 *
 * Never throws — on any failure it returns "no-push" / "stale-ok" and lets
 * the caller proceed with whatever payload is loaded.
 *
 * `shouldCancel` (optional) is polled between the boot-wait sleeps so a caller
 * that has been torn down (e.g. the upload queue's Stop during a recovery)
 * can bail out of the ~30 s poll promptly instead of running it to completion
 * in the background. Returns "no-push" when cancelled mid-poll. The ELF may
 * already have been sent by then — that's fine, it's idempotent.
 *
 * `force` (optional) re-sends the ELF even when the mgmt port reports a
 * matching version. This is REQUIRED for recovery from a mid-transfer drop:
 * the version check probes the mgmt port (:9114), but the transfer listener
 * (:9113) can die while mgmt survives — or the whole payload can be wedged —
 * so trusting "version matches → current" leaves a dead transfer port in
 * place and the resume retry just fails again on a refused :9113 (the
 * "I had to re-send the ELF manually" symptom). On a connection-class
 * failure the payload is already suspect, so force a clean redeploy.
 */
export async function ensurePayloadCurrent(
  host: string,
  shouldCancel?: () => boolean,
  force = false,
): Promise<EnsurePayloadResult> {
  if (shouldCancel?.()) return "no-push";
  let appVersion: string;
  try {
    // `getAppVersion`, not Tauri's `getVersion`: the latter needs
    // `__TAURI_INTERNALS__` and THROWS in a browser, which made this whole
    // function a no-op in the self-hosted web UI — the queue's
    // payload-is-current check and its auto-recovery redeploy both did
    // nothing there. In a browser the engine binary is the app, so its
    // `/api/version` is the equivalent.
    appVersion = await getAppVersion();
  } catch {
    // Can't read our own version — abort the auto-push entirely so we don't
    // accidentally push the wrong file. Proceed with the running payload.
    return "no-push";
  }
  // Probe what's running.
  let running: string | null = null;
  try {
    const probe = await payloadCheck(host);
    if (probe.reachable) {
      running = probe.payloadVersion;
    }
  } catch {
    // payloadCheck threw — fall through to push attempt.
  }
  if (!force && running && compareVersions(running, appVersion) === 0) {
    // The helper is current, which also means the console answered us just
    // now. Take the opportunity to arm the update installer (see prearmDpi):
    // it is one probe when the daemon is already up, and once per console per
    // session otherwise. Deliberately not awaited — nothing here depends on
    // it, and an unreachable loader must not slow down a healthy connect.
    void prearmDpiDaemon(host);
    return "current";
  }
  // Need to push. Locate the bundled ELF + send it.
  log.info(
    "payload",
    `(re)deploying helper to ${host} (running=${running ?? "none"}, want=${appVersion})`,
  );
  if (!isTauriEnv()) {
    // A browser has neither the ELF bytes nor a socket to the loader, so the
    // engine does the send. `bundledPayloadPath`/`sendPayload` are
    // desktop-only commands and throw here — which is why the web UI could
    // never redeploy a helper it had just found stale or dead.
    await restoreMainPayload(host);
  } else {
    let elfPath: string;
    try {
      elfPath = await bundledPayloadPath();
    } catch (e) {
      log.warn("payload", `cannot locate bundled payload ELF: ${e instanceof Error ? e.message : String(e)}`);
      return "no-push";
    }
    try {
      await sendPayload(host, elfPath);
    } catch (e) {
      log.warn("payload", `payload send to ${host} failed: ${e instanceof Error ? e.message : String(e)}`);
      return "no-push";
    }
  }
  // Poll up to ~30 s for the new payload to come up + report matching
  // version. ps5-payload-sdk's loader takes a few seconds to gunzip +
  // execute; the elevateUcred step takes a few more.
  await sleep(1500);
  for (let i = 0; i < 28; i++) {
    if (shouldCancel?.()) return "no-push";
    try {
      const probe = await payloadCheck(host);
      if (
        probe.reachable &&
        probe.payloadVersion &&
        compareVersions(probe.payloadVersion, appVersion) === 0
      ) {
        // The loader just took an ELF, so we know it is alive THIS second.
        // That is the whole point of arming here: a loader that works now can
        // be gone by the time an update install needs it, and then the
        // installer can never be delivered at all.
        void prearmDpiDaemon(host);
        return "pushed";
      }
    } catch {
      // ignore; keep polling
    }
    await sleep(1000);
  }
  // Push went through but the new version never showed up in the poll
  // window — continue anyway; it may be running but reporting an unexpected
  // version (e.g. user sideloaded a different build during the wait).
  return "stale-ok";
}
