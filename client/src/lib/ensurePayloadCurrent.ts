import { getVersion } from "@tauri-apps/api/app";

import { bundledPayloadPath, payloadCheck, sendPayload } from "../api/ps5";
import { compareVersions } from "./semver";
import { log } from "../state/logs";

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
 */
export async function ensurePayloadCurrent(
  host: string,
  shouldCancel?: () => boolean,
): Promise<EnsurePayloadResult> {
  if (shouldCancel?.()) return "no-push";
  let appVersion: string;
  try {
    appVersion = await getVersion();
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
  if (running && compareVersions(running, appVersion) === 0) {
    return "current";
  }
  // Need to push. Locate the bundled ELF + send it.
  log.info(
    "payload",
    `(re)deploying helper to ${host} (running=${running ?? "none"}, want=${appVersion})`,
  );
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
