import {
  bundledHelperVersion,
  bundledPayloadPath,
  payloadCheck,
  type PayloadSendRefused,
  portCheck,
  sendPayload,
  type HelperBusy,
  type HelperSendOptions,
} from "../api/ps5";
import { hostOf, PAYLOAD_MANAGER_PORT, PS5_AVA1_PORT, PS5_LOADER_PORT } from "./addr";
import { getAppVersion } from "./appVersion";
import { isTauriEnv } from "./tauriEnv";
import { engineSendsHelper } from "./restoreMainPayload";
import { guardElfldr, LISTENER_LOADER_HINT, STUCK_LOADER_MESSAGE, waitForLoader } from "./elfldrGuard";
import {
  consoleKey,
  noteSentAnyway,
  RESEND_COOLDOWN_MS,
  resetHelperSendGate,
  sendFailedPastConnect,
  sendHelperOnce,
  sentAgoMs,
} from "./helperSendGate";
import { compareVersions } from "./semver";
import { helperAnsweredButUnusable } from "./autoRedeploy";
import { STATUS_SLOW_REASON } from "./statusSchedule";
import { justStarted, noteConsoleGone, noteHelperUp, noteLoader, resetLoaderSettle, unattendedSettle } from "./loaderSettle";
import { helperAtBootSettleMs, loadBootRecord } from "./helperAtBoot";
import { safeGetItem, safeSetItem } from "./safeStorage";
import { log } from "../state/logs";
import { clearHelperStopped, isHelperStopped } from "../state/helperStopped";
import { updateHelperCopiesAfterSend } from "../state/helperStoredCopiesRuntime";
import { helperSendFor, useHelperSendStore } from "../state/helperSend";
import { meshAnnounce } from "../api/mesh";
import { meshFor, othersBusy, useMeshStore } from "../state/mesh";
import { filesTransferBusy } from "./ps5Transfers";
import { staticTranslator, trStatic } from "./trStatic";

export { RESEND_COOLDOWN_MS } from "./helperSendGate";

export type EnsurePayloadResult =
  | "current"
  | "pushed"
  | "stale-ok"
  | "no-push"
  /** The send broke after the loader took it: the console may run it. Counted like a send. */
  | "sent-failed";

/** Why the last call for a console did not send (or what it did), for the screens and the
 *  reconnect loop's reasons. `why` is a hold reason or the engine's refusal code. */
export interface EnsureOutcome {
  result: EnsurePayloadResult;
  why?: string;
  /** When an unattended send may go again (a settle, the engine's hourly limit). */
  retryAt?: number | null;
}

const lastOutcome = new Map<string, EnsureOutcome>();

/** What the last call for `host` ended with, or null. */
export function lastEnsureOutcome(host: string): EnsureOutcome | null {
  return lastOutcome.get(consoleKey(host)) ?? null;
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

/** How long a new helper is waited for after a send: the loader unpacks it, it takes over from the
 *  old one (up to 15 s), the kernel patches, its listeners. 30 s was too short on a slow console,
 *  and the next caller sent another one over it. */
export const NEW_HELPER_WAIT_MS = 120_000;

export interface EnsureOptions {
  /** The caller asked the console itself what a restart would stop, right before (the automatic
   *  update's checks): a helper too old to report it may be replaced unattended. */
  consoleChecked?: boolean;
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
 * can bail out of the poll promptly instead of running it to completion
 * in the background. Returns "no-push" when cancelled mid-poll. The ELF may
 * already have been sent by then — that's fine, it's idempotent.
 *
 * `force` (kept for the callers): a recovery's "the transfer broke, the helper is suspect". Since
 * 12.6.15 it changes nothing on its own: transfers and management share one port, so a helper
 * that answers status is not replaced mid-run (the retry adopts its job), and one that does not
 * answer is sent anyway.
 *
 * `mode` says who asked (#471). `"explicit"` is an action the user started that needs the
 * helper (starting an upload): it sends as before and clears a "stopped by you" mark.
 * `"auto"` (the default) is everything unattended — the reconnect loop, a queue's mid-run
 * recovery, a run nobody clicked — and leaves a console whose helper the user stopped alone,
 * returning "no-push" without touching it. Unattended sends also wait for a console that just
 * started (lib/loaderSettle), never touch a helper that answers in any way unless it is an older
 * one the caller means to replace, and go through the engine's stricter rules (an hourly limit,
 * kept across restarts: engine helper_send.rs).
 */
export function ensurePayloadCurrent(
  host: string,
  shouldCancel?: () => boolean,
  force = false,
  mode: "auto" | "explicit" = "auto",
  options: EnsureOptions = {},
): Promise<EnsurePayloadResult> {
  if (mode === "explicit") clearHelperStopped(host);
  else if (isHelperStopped(host)) {
    log.info(
      "payload",
      `not sending the helper to ${host}: you stopped it there; send it from Connection when you need it`,
    );
    record(host, { result: "no-push", why: "stopped" });
    return Promise.resolve("no-push");
  }
  // One at a time per console. The redeploy loop, both queues and their
  // recovery paths each call this on their own schedule, and two sends that
  // land together start two helpers: the Phat's startup log has pairs of
  // ENTER_MAIN 82 ms and 180 ms apart, each followed by two mgmt threads and a
  // takeover fight. A caller that arrives while a check is running shares its
  // result instead of sending again, unless it asks for more than the running one (a person's
  // request joining an unattended check that held back): it then runs after it, as itself.
  const key = consoleKey(host);
  const running = inFlight.get(key);
  if (running) {
    const stronger = mode === "explicit" && running.mode === "auto";
    if (!stronger) return running.p;
    const after = running.p.then((r) =>
      r === "no-push" ? ensurePayloadCurrentOnce(host, shouldCancel, force, mode, options) : r,
    );
    const chained = after.finally(() => {
      if (inFlight.get(key)?.p === chained) inFlight.delete(key);
    });
    inFlight.set(key, { p: chained, mode });
    return chained;
  }
  const p = ensurePayloadCurrentOnce(host, shouldCancel, force, mode, options).finally(() => {
    if (inFlight.get(key)?.p === p) inFlight.delete(key);
  });
  inFlight.set(key, { p, mode });
  return p;
}

const inFlight = new Map<string, { p: Promise<EnsurePayloadResult>; mode: "auto" | "explicit" }>();

/** The wait before an unattended send, the same on every run of this device (0 to 15 s): two apps
 *  that find a console without its helper at the same moment do not send together. */
const JITTER_KEY = "ps5upload.helper_send_jitter_ms";
function deviceJitterMs(): number {
  const kept = Number(safeGetItem(JITTER_KEY));
  if (Number.isFinite(kept) && kept >= 0 && kept <= 15_000 && safeGetItem(JITTER_KEY) !== null) return kept;
  const ms = Math.floor(Math.random() * 15_000);
  safeSetItem(JITTER_KEY, String(ms));
  return ms;
}
let jitterMs: () => number = deviceJitterMs;

let bundledMemo: Promise<string | null> | null = null;
/** The version of the helper this app sends (null: it does not say). */
function bundledVersion(): Promise<string | null> {
  bundledMemo ??= (async () => bundledHelperVersion())().catch(() => {
    bundledMemo = null;
    return null;
  });
  return bundledMemo;
}

let staleLogged = false;

/** Test seam. */
export function resetEnsurePayloadState(): void {
  inFlight.clear();
  lastOutcome.clear();
  resetHelperSendGate();
  resetLoaderSettle();
  bundledMemo = null;
  staleLogged = false;
  jitterMs = () => 0;
}

/** Test seam: the unattended wait before a send. */
export function setUnattendedJitterForTests(fn: () => number): void {
  jitterMs = fn;
}

function record(host: string, o: EnsureOutcome): void {
  lastOutcome.set(consoleKey(host), o);
}

/** A TCP probe of one port; null when it could not be asked. */
async function probePort(host: string, port: number): Promise<boolean | null> {
  try {
    return await portCheck(hostOf(host) || host, port);
  } catch {
    return null;
  }
}

/** Whether something can take a helper: the loader, or Payload Manager. null when not known. */
async function loaderAnswers(host: string): Promise<boolean | null> {
  const loader = await probePort(host, PS5_LOADER_PORT);
  if (loader === null) return null;
  if (loader) return true;
  return (await probePort(host, PAYLOAD_MANAGER_PORT)) === true;
}

/** Why a running helper must not be replaced unasked, or null. `busy`: what the helper said a
 *  restart would stop (null: it cannot say, an older helper). */
export function replaceHold(input: {
  running: string | null;
  want: string;
  othersBusy: boolean;
  filesBusy: boolean;
  busy: HelperBusy | null;
  helperAnswers: boolean;
  mode: "auto" | "explicit";
  consoleChecked: boolean;
}): { why: string; text: string } | null {
  if (input.running && compareVersions(input.running, input.want) === 1) {
    return { why: "newer", text: `it runs v${input.running}, newer than this app's v${input.want}` };
  }
  if (input.othersBusy) return { why: "others-busy", text: "other devices are using this console" };
  // Not this app's queue: its rows are the reason this runs, and a recovery runs under them.
  if (input.filesBusy) return { why: "files-busy", text: "the Files screen is moving data on this console" };
  if (input.busy && !input.busy.idle) {
    return { why: "console-busy", text: "the PS5 reports work a restart would stop" };
  }
  // A helper that answers and cannot say whether it is busy (one before 12.6.15) is replaced
  // unattended only after the caller asked the console itself (the automatic update).
  if (input.helperAnswers && !input.busy && input.mode === "auto" && !input.consoleChecked) {
    return { why: "console-unknown", text: "it cannot say whether the PS5 is busy" };
  }
  return null;
}

async function ensurePayloadCurrentOnce(
  host: string,
  shouldCancel: (() => boolean) | undefined,
  force: boolean,
  mode: "auto" | "explicit",
  options: EnsureOptions,
): Promise<EnsurePayloadResult> {
  const hold = (why: string, text?: string, retryAt?: number | null): EnsurePayloadResult => {
    if (text) log.info("payload", `not sending the helper to ${host}: ${text}`);
    record(host, { result: "no-push", why, retryAt: retryAt ?? null });
    return "no-push";
  };
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
    return hold("no-version");
  }
  // The helper this app sends says its version; the running one is compared with THAT. One of
  // another version than the app (a stale build) is never sent unattended.
  const bundled = await bundledVersion();
  const want = bundled ?? appVersion;
  if (bundled && bundled !== appVersion) {
    if (!staleLogged) {
      staleLogged = true;
      log.error(
        "payload",
        `this app (v${appVersion}) carries helper v${bundled}: a stale build. It is never sent by itself; reinstall the app`,
      );
    }
    if (mode === "auto") return hold("stale-build");
  }
  // Probe what's running.
  let running: string | null = null;
  let helperAnswers = false;
  let probe: Awaited<ReturnType<typeof payloadCheck>> | null = null;
  try {
    probe = await payloadCheck(host);
  } catch {
    // payloadCheck threw — fall through to push attempt.
  }
  if (probe?.reachable) {
    helperAnswers = true;
    running = probe.payloadVersion;
    noteHelperUp(host);
  } else if (probe) {
    if (probe.session === "needs_pairing") {
      // A helper IS running and only wants pairing, which the pairing banner asks for. Replacing
      // it would stop it for every device that uses it, unasked.
      return hold("pairing", "one is running there, waiting to be paired");
    }
    if (probe.reason === STATUS_SLOW_REASON) {
      // Alive and talking, only slow to answer (`status_slow`): a weak link, usually Wi-Fi. A new
      // helper replaces this one and drops every connection to it, which on a weak link makes the
      // console drop in and out. Even a forced recovery leaves it: the retry adopts its job.
      return hold("slow", "one is running there, only slow to answer");
    }
    // It answered the engine in a way this app cannot use (another app version's, too new, too
    // old): it is running. Only a person replaces it; starting an upload counts for a too-old one,
    // which cannot serve this app at all.
    if (helperAnsweredButUnusable(probe) && !(mode === "explicit" && probe.helperTooOld)) {
      return hold("answering", "one is running there that this app cannot use; Send helper replaces it");
    }
    if (probe.power === "rest") noteConsoleGone(host);
  }
  if (helperAnswers) {
    const cmp = running ? compareVersions(running, want) : null;
    if (cmp === 0) {
      // The helper is current, whatever `force` says: transfers and management share its one
      // port, so one that answers is not replaced mid-run. The install daemon is not pre-armed
      // here (the engine brings it up per install); keep the patched elfldr in place, in the
      // background (see lib/elfldrGuard.ts).
      void guardElfldr(host, false);
      record(host, { result: "current" });
      return "current";
    }
    // A version that does not read: never replaced on a guess, by nobody's request.
    if (cmp === null && mode === "auto") {
      return hold("version-unknown", `its version "${running ?? ""}" does not read as one`);
    }
  }
  // Replacing a helper that answers, or whose port still takes connections, restarts it for
  // every device on the console, and nobody here asked: only the Send helper buttons ask
  // (useHelperReplaceGuard). So it is left alone when this would take something away from someone.
  const portOpen = helperAnswers || probe?.helperTooOld === true || (await probePort(host, PS5_AVA1_PORT)) === true;
  if (helperAnswers || portOpen) {
    const h = replaceHold({
      running,
      want,
      othersBusy: othersBusy(meshFor(useMeshStore.getState(), host).state).length > 0,
      filesBusy: filesTransferBusy(host),
      busy: probe?.busy ?? null,
      helperAnswers,
      mode,
      consoleChecked: options.consoleChecked === true,
    });
    if (h) return hold(h.why, h.text);
  }
  // Need to push — unless we already did, moments ago, and that helper is still starting. The
  // Send helper button counts too (lib/helperSendGate); checked again as the send starts.
  const sentAgo = sentAgoMs(host) ?? Infinity;
  if (sentAgo < RESEND_COOLDOWN_MS) {
    return hold(
      "recent",
      `one was sent ${Math.round(sentAgo / 1000)}s ago and may still be starting`,
      Date.now() - sentAgo + RESEND_COOLDOWN_MS,
    );
  }
  // A console that just started: its jailbreak is still loading behind the loader, and a helper
  // landing in that gap crashed an FW 11.40 console every time. Unattended sends wait for it;
  // starting an upload waits too (the person did not ask for a helper send in particular).
  const settleMs = helperAtBootSettleMs(loadBootRecord(host));
  const loaderOpen = await loaderAnswers(host);
  if (loaderOpen !== null) {
    if (mode === "auto") {
      const s = unattendedSettle(host, loaderOpen, settleMs);
      if (s.state === "closed") return hold("loader-closed", "nothing answers on its loader or Payload Manager");
      if (s.state === "settle") {
        return hold(
          "settling",
          `its loader came up moments ago; waiting ${Math.round((s.sendAt - Date.now()) / 1000)}s for the console to finish starting`,
          s.sendAt,
        );
      }
    } else {
      noteLoader(host, loaderOpen);
      const started = justStarted(host, settleMs);
      if (started) {
        log.info("payload", `${host} just started: waiting until its jailbreak has loaded before sending the helper`);
        while (Date.now() < started.sendAt) {
          if (shouldCancel?.()) return "no-push";
          await sleep(Math.min(1000, started.sendAt - Date.now()));
        }
      }
    }
  }
  // A stuck elfldr takes the connection and never answers: sending would only add one more
  // it ignores. A patched one clears within 15 s; a stock one stays stuck until reloaded.
  if ((await waitForLoader(host)) === "stuck") {
    log.error("payload", `not sending the helper to ${host}: ${STUCK_LOADER_MESSAGE} ${LISTENER_LOADER_HINT}`);
    return hold("stuck-loader");
  }
  // Two apps that found the console without its helper at the same moment do not send together:
  // each waits its own few seconds, then looks again right before the send.
  if (mode === "auto") {
    const wait = jitterMs();
    if (wait > 0) {
      await sleep(wait);
      if (shouldCancel?.()) return "no-push";
      const again = await payloadCheck(host).catch(() => null);
      if (again?.reachable) {
        const c = again.payloadVersion ? compareVersions(again.payloadVersion, want) : null;
        if (c !== null && c >= 0) {
          record(host, { result: "current" });
          return "current";
        }
        if (!helperAnswers) return hold("answering", "a helper came up meanwhile");
      } else if (again && helperAnsweredButUnusable(again)) {
        return hold("answering", "a helper came up meanwhile");
      }
    }
  }
  // Locate the bundled ELF + send it.
  log.info(
    "payload",
    `(re)deploying helper to ${host} (running=${running ?? "none"}, want=${want}, ${mode === "auto" ? "unattended" : "asked"}${force ? ", recovery" : ""})`,
  );
  // Best effort: the other devices hear it on their next poll, before the helper goes.
  if (helperAnswers && meshFor(useMeshStore.getState(), host).state) {
    await meshAnnounce(host, "restart", `sending helper v${want}`).catch(() => false);
  }
  const opts: HelperSendOptions = { mode, replace: helperAnswers, appVersion };
  const unpublish = publishStarting(host);
  try {
    let sent: Awaited<ReturnType<typeof sendHelperOnce>>;
    try {
      sent = await sendHelperOnce(host, () => sendBundled(host, opts), { minGapMs: RESEND_COOLDOWN_MS, mode });
    } catch (e) {
      if (isRefusal(e)) {
        log.info("payload", `not sending the helper to ${host}: the engine refused (${e.code}${e.detail ? `: ${e.detail}` : ""})`);
        return hold(e.code, undefined, e.retryAfterMs !== null ? Date.now() + e.retryAfterMs : null);
      }
      const msg = e instanceof Error ? e.message : String(e);
      log.warn("payload", `payload send to ${host} failed: ${msg}`);
      if (sendFailedPastConnect(msg)) {
        // The loader took it: the console may run it, or have crashed on it. Counted as a send.
        noteSentAnyway(host);
        record(host, { result: "sent-failed", why: "send-broke" });
        return "sent-failed";
      }
      return hold("send-failed");
    }
    if (sent === "recent") return hold("recent", "one was sent moments ago and may still be starting");
    return await waitForNewHelper(host, want, probe?.reachable ? (probe.instanceId ?? null) : null, shouldCancel);
  } finally {
    unpublish();
  }
}

/** An engine refusal (`PayloadSendRefused`), told by its name so a caught value of any kind is
 *  safe to ask about. */
function isRefusal(e: unknown): e is PayloadSendRefused {
  return e instanceof Error && e.name === "PayloadSendRefused";
}

/** The send itself: the engine sends (the browser build: its own helper; the desktop app: the
 *  bundled one, from memory). */
async function sendBundled(host: string, opts: HelperSendOptions): Promise<void> {
  if (!isTauriEnv()) {
    // A browser has neither the ELF bytes nor a socket to the loader, so the
    // engine does the send. A refused send throws (engineSendsHelper), so the gate records no send.
    await engineSendsHelper(host, opts);
    return;
  }
  const elfPath = await bundledPayloadPath();
  await sendPayload(host, elfPath, undefined, opts);
}

/** Every Send helper button for this console shows the unattended send as running, and the
 *  screens say "Starting the helper…", until it ends. Returns the undo. */
function publishStarting(host: string): () => void {
  const store = useHelperSendStore.getState();
  if (helperSendFor(store, host)) return () => {};
  store.put(host, {
    state: "busy",
    phase: "sending",
    msg: trStatic("helper_starting", "Starting the helper…"),
    startedAtMs: Date.now(),
  });
  return () => {
    const cur = helperSendFor(useHelperSendStore.getState(), host);
    if (cur?.state === "busy" && cur.phase === "sending") useHelperSendStore.getState().put(host, null);
  };
}

/** Waits for the helper just sent to answer: a new run of it (another instance id, so a
 *  same-version replace is seen too), or, from an older engine without ids, this app's version. */
async function waitForNewHelper(
  host: string,
  want: string,
  instanceBefore: number | null,
  shouldCancel: (() => boolean) | undefined,
): Promise<EnsurePayloadResult> {
  const until = Date.now() + NEW_HELPER_WAIT_MS;
  await sleep(1500);
  for (let i = 0; Date.now() < until; i++) {
    if (shouldCancel?.()) return "no-push";
    try {
      const probe = await payloadCheck(host);
      const fresh =
        probe.reachable &&
        (instanceBefore != null && probe.instanceId != null
          ? probe.instanceId !== instanceBefore
          : !!probe.payloadVersion && compareVersions(probe.payloadVersion, want) === 0);
      if (fresh && probe.payloadVersion && compareVersions(probe.payloadVersion, want) === 0) {
        noteHelperUp(host);
        // A fresh helper usually means a reboot or a wake, which brings the stock elfldr back.
        void guardElfldr(host, true);
        // The send replaced the helper in memory only. Without this, an autoloader's old copy
        // comes back at every boot and the reconnect loop or the next queue sends again.
        void updateHelperCopiesAfterSend(host, staticTranslator).catch(() => {});
        record(host, { result: "pushed" });
        return "pushed";
      }
      if (fresh) {
        // A new helper, but not this app's version (someone sideloaded another meanwhile).
        record(host, { result: "stale-ok" });
        return "stale-ok";
      }
    } catch {
      // ignore; keep polling
    }
    // Every second for the first half minute, then every three.
    await sleep(i < 28 ? 1000 : 3000);
  }
  // Push went through but the new version never showed up in the poll window. It may still be
  // starting: nothing is sent again before the cooldown, and the caller carries on.
  record(host, { result: "stale-ok", why: "not-up-yet" });
  return "stale-ok";
}
