import { bundledPayloadPath, payloadCheck, sendPayload } from "../api/ps5";
import { getAppVersion } from "./appVersion";
import { isTauriEnv } from "./tauriEnv";
import { engineSendsHelper } from "./restoreMainPayload";
import { guardElfldr, LISTENER_LOADER_HINT, STUCK_LOADER_MESSAGE, waitForLoader } from "./elfldrGuard";
import { resetHelperSendGate, sendHelperOnce, sentAgoMs } from "./helperSendGate";
import { compareVersions } from "./semver";
import { log } from "../state/logs";
import { clearHelperStopped, isHelperStopped } from "../state/helperStopped";
import { updateHelperCopiesAfterSend } from "../state/helperStoredCopiesRuntime";
import { meshAnnounce } from "../api/mesh";
import { meshFor, othersBusy, useMeshStore } from "../state/mesh";
import { filesTransferBusy } from "./ps5Transfers";
import { staticTranslator } from "./trStatic";

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
 * the version check can answer from a half-alive helper, but the transfer listener
 * can die while management survives — or the whole payload can be wedged —
 * so trusting "version matches → current" leaves a dead transfer port in
 * place and the resume retry just fails again on a refused connection (the
 * "I had to re-send the ELF manually" symptom). On a connection-class
 * failure the payload is already suspect, so force a clean redeploy.
 *
 * `mode` says who asked (#471). `"explicit"` is an action the user started that needs the
 * helper (starting an upload): it sends as before and clears a "stopped by you" mark.
 * `"auto"` (the default) is everything unattended — the reconnect loop, a queue's mid-run
 * recovery, a run nobody clicked — and leaves a console whose helper the user stopped alone,
 * returning "no-push" without touching it.
 */
export function ensurePayloadCurrent(
  host: string,
  shouldCancel?: () => boolean,
  force = false,
  mode: "auto" | "explicit" = "auto",
): Promise<EnsurePayloadResult> {
  if (mode === "explicit") clearHelperStopped(host);
  else if (isHelperStopped(host)) {
    log.info(
      "payload",
      `not sending the helper to ${host}: you stopped it there; send it from Connection when you need it`,
    );
    return Promise.resolve("no-push");
  }
  // One at a time per console. The redeploy loop, both queues and their
  // recovery paths each call this on their own schedule, and two sends that
  // land together start two helpers: the Phat's startup log has pairs of
  // ENTER_MAIN 82 ms and 180 ms apart, each followed by two mgmt threads and a
  // takeover fight. A caller that arrives while a check is running shares its
  // result instead of sending again.
  const key = host.trim().toLowerCase();
  const running = inFlight.get(key);
  if (running) return running;
  const p = ensurePayloadCurrentOnce(host, shouldCancel, force).finally(() => {
    inFlight.delete(key);
  });
  inFlight.set(key, p);
  return p;
}

const inFlight = new Map<string, Promise<EnsurePayloadResult>>();

/** A helper just sent is still starting — taking over, arming listeners — for
 *  several seconds, and answers nothing meanwhile. A second send inside this
 *  window is what starts a duplicate instance, so it waits for the first
 *  instead. Longer than the ~30 s boot poll below, so a slow boot that the
 *  poll gave up on is not immediately doubled either. */
export const RESEND_COOLDOWN_MS = 45_000;

/** Test seam. */
export function resetEnsurePayloadState(): void {
  inFlight.clear();
  resetHelperSendGate();
}

/** Why a helper that answers must not be replaced unasked, or null when nothing stands in the way. */
function replaceHold(host: string, running: string | null, appVersion: string): string | null {
  if (running && compareVersions(running, appVersion) === 1) {
    return `it runs v${running}, newer than this app's v${appVersion}`;
  }
  const busy = othersBusy(meshFor(useMeshStore.getState(), host).state);
  if (busy.length > 0) return "other devices are using this console";
  // Not this app's queue: its rows are the reason this runs, and a recovery runs under them.
  if (filesTransferBusy(host)) return "the Files screen is moving data on this console";
  return null;
}

async function ensurePayloadCurrentOnce(
  host: string,
  shouldCancel: (() => boolean) | undefined,
  force: boolean,
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
  let helperAnswers = false;
  try {
    const probe = await payloadCheck(host);
    if (probe.reachable) {
      helperAnswers = true;
      running = probe.payloadVersion;
    } else if (probe.session === "needs_pairing") {
      // A helper IS running and only wants pairing, which the pairing banner asks for. Replacing
      // it would stop it for every device that uses it, unasked.
      log.info("payload", `not sending the helper to ${host}: one is running there, waiting to be paired`);
      return "no-push";
    }
  } catch {
    // payloadCheck threw — fall through to push attempt.
  }
  if (!force && running && compareVersions(running, appVersion) === 0) {
    // The helper is current. The install daemon is no longer pre-armed here —
    // the engine brings it up per-install during `POST /api/pkg/install` — so
    // a healthy connect does no extra work — beyond keeping the patched elfldr in place, in the
    // background (see lib/elfldrGuard.ts).
    void guardElfldr(host, false);
    return "current";
  }
  // Replacing a helper that answers restarts it for every device on the console, and nobody
  // here asked: only the Send helper buttons ask (useHelperReplaceGuard). So a running helper is
  // left alone when this would take something away from someone. `force` does not change that:
  // a recovery must not put an older helper over a newer one, or stop another device's work.
  if (helperAnswers) {
    const hold = replaceHold(host, running, appVersion);
    if (hold) {
      log.info("payload", `not replacing the helper on ${host}: ${hold}`);
      return "no-push";
    }
  }
  // Need to push — unless we already did, moments ago, and that helper is
  // still starting.
  // The Send helper button counts too (lib/helperSendGate): one send per console at a time.
  const sentAgo = sentAgoMs(host) ?? Infinity;
  if (sentAgo < RESEND_COOLDOWN_MS) {
    log.info(
      "payload",
      `not re-sending the helper to ${host}: one was sent ${Math.round(sentAgo / 1000)}s ago and may still be starting`,
    );
    return "no-push";
  }
  // A stuck elfldr takes the connection and never answers: sending would only add one more
  // it ignores. A patched one clears within 15 s; a stock one stays stuck until reloaded.
  if ((await waitForLoader(host)) === "stuck") {
    log.error("payload", `not sending the helper to ${host}: ${STUCK_LOADER_MESSAGE} ${LISTENER_LOADER_HINT}`);
    return "no-push";
  }
  // Locate the bundled ELF + send it.
  log.info(
    "payload",
    `(re)deploying helper to ${host} (running=${running ?? "none"}, want=${appVersion})`,
  );
  // Best effort: the other devices hear it on their next poll, before the helper goes.
  if (helperAnswers && meshFor(useMeshStore.getState(), host).state) {
    await meshAnnounce(host, "restart", `sending helper v${appVersion}`).catch(() => false);
  }
  if (!isTauriEnv()) {
    // A browser has neither the ELF bytes nor a socket to the loader, so the
    // engine does the send. `bundledPayloadPath`/`sendPayload` are
    // desktop-only commands and throw here — which is why the web UI could
    // never redeploy a helper it had just found stale or dead.
    // A refused send throws (engineSendsHelper), so the gate records no send:
    // the never-throwing restoreMainPayload here stamped a send that did not
    // happen, polled 30 s for it, and held every caller to "no-push" for the
    // cooldown.
    try {
      await sendHelperOnce(host, () => engineSendsHelper(host));
    } catch (e) {
      log.warn("payload", `payload send to ${host} failed: ${e instanceof Error ? e.message : String(e)}`);
      return "no-push";
    }
  } else {
    let elfPath: string;
    try {
      elfPath = await bundledPayloadPath();
    } catch (e) {
      log.warn("payload", `cannot locate bundled payload ELF: ${e instanceof Error ? e.message : String(e)}`);
      return "no-push";
    }
    try {
      await sendHelperOnce(host, () => sendPayload(host, elfPath));
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
        // A fresh helper usually means a reboot or a wake, which brings the stock elfldr back.
        void guardElfldr(host, true);
        // The send replaced the helper in memory only. Without this, an autoloader's old copy
        // comes back at every boot and the reconnect loop or the next queue sends again.
        void updateHelperCopiesAfterSend(host, staticTranslator).catch(() => {});
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
