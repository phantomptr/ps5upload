import { bundledPayloadPath, payloadCheck, PayloadSendRefused, sendPayload, type HelperSendOptions } from "../api/ps5";
import { PS5_LOADER_PORT } from "../lib/addr";
import { isNotPairedError, reportIfNotPaired } from "../lib/consoleSession";
import { LISTENER_LOADER_HINT, STUCK_LOADER_MESSAGE, waitForLoader } from "../lib/elfldrGuard";
import { useConnectionStore } from "./connection";
import { updateHelperCopiesAfterSend } from "./helperStoredCopiesRuntime";
import { clearHelperStopped } from "./helperStopped";
import {
  helperSendFor,
  runHelperSend,
  useHelperSendStore,
  type HelperSendResult,
} from "./helperSend";
import type { Translator } from "./lang";
import { RESEND_COOLDOWN_MS, sendHelperOnce, sendInFlight, sentAgoMs } from "../lib/helperSendGate";
import { isTauriEnv } from "../lib/tauriEnv";
import { trStatic } from "../lib/trStatic";
import { getAppVersion } from "../lib/appVersion";
import { compareVersions } from "../lib/semver";
import { engineSendsHelper } from "../lib/restoreMainPayload";
import { justStarted } from "../lib/loaderSettle";
import { helperAtBootSettleMs, loadBootRecord } from "../lib/helperAtBoot";
import { payloadSendRefusalText } from "../lib/payloadSendRefusal";

/** "connect …:9021: Connection refused" says only that the send failed; say what to do. The
 *  errors a phone or a Mac gives when the app may not reach the local network, a name that does
 *  not look up and an IPv6 address get their own next step (12.6.15). */
export function loaderHint(error: string, host?: string): string {
  if (host && isIpv6Literal(host)) {
    return trStatic(
      "connection_ipv6_address",
      "This PS5 address is an IPv6 address. The PS5's loader and the helper listen on IPv4 only: use the console's IPv4 address (like 192.168.1.20, under Settings › Network › View Connection Status on the PS5). ({error})",
    ).replace("{error}", error);
  }
  if (/no route to host|EHOSTUNREACH|os error 65\b/i.test(error)) {
    return trStatic(
      "connection_no_route",
      "This device can't reach the PS5 (no route to host). On a Mac, iPhone or iPad, allow PS5Upload under Settings › Privacy & Security › Local Network. Otherwise check that both are on the same network. ({error})",
    ).replace("{error}", error);
  }
  if (/operation not permitted|EPERM|os error 1\b/i.test(error)) {
    return trStatic(
      "connection_not_permitted",
      "This device did not let the app connect to the PS5 (operation not permitted). On Android, allow PS5Upload to find and connect to devices on your local network (Settings › Apps › PS5Upload › Permissions), then try again. ({error})",
    ).replace("{error}", error);
  }
  if (/cannot resolve|failed to lookup|name lookup|nodename nor servname|Name or service not known|No such host/i.test(error)) {
    return trStatic(
      "connection_name_lookup",
      "The PS5's name could not be looked up. Use its IP address instead (on the PS5: Settings › Network › View Connection Status). ({error})",
    ).replace("{error}", error);
  }
  return /:9021\b.*(refused|reset|unreachable|timed out|timeout)/i.test(error)
    ? trStatic(
        "connection_loader_down",
        "The PS5's loader (port 9021) isn't running. Load elfldr on the PS5 (your jailbreak or autoloader does it), then send the helper again. ({error})",
      ).replace("{error}", error)
    : error;
}

/** An IPv6 literal (bracketed or bare, with or without a port). */
export function isIpv6Literal(host: string): boolean {
  const h = host.trim();
  if (h.startsWith("[")) return true;
  return (h.match(/:/g)?.length ?? 0) >= 2;
}

/** A refusal a person may override: thrown out of the send so the step shows "Send anyway". */
class Forceable extends Error {
  readonly forceable = true;
}

function withLoaderHint(host: string) {
  return (e: unknown): never => {
    if (e instanceof PayloadSendRefused) {
      const text = payloadSendRefusalText(e);
      throw e.forceable ? new Forceable(text) : new Error(text);
    }
    throw new Error(loaderHint(e instanceof Error ? e.message : String(e), host));
  };
}

// Says what to do first; the probe's technical detail ({tail}) goes last, where it
// doesn't push the advice out of view.
const TIMEOUT_EN =
  "The helper has not answered yet. It may still be starting: wait a minute before sending it again. If it does not come up, load elfldr and kstuff first, then send the helper.{tail}";

/** Polls for the new helper: about two minutes (a slow console takes over, patches and arms its
 *  listeners well past 20 s, and an early "send it again" started a second one over it). */
const NEW_HELPER_POLLS = 110;

/** Why a send someone asked for should wait, with the "Send anyway" that overrides it: a helper
 *  was sent moments ago and may still be starting, or the console just started and its
 *  jailbreak is still loading. null when nothing stands in the way. */
export function sendNowHold(
  host: string,
  tr: Translator,
  now = Date.now(),
): string | null {
  const ago = sentAgoMs(host, now);
  if (!sendInFlight(host) && ago !== null && ago < RESEND_COOLDOWN_MS) {
    return tr(
      "helper_send_recent",
      { seconds: Math.max(1, Math.round(ago / 1000)) },
      "A helper was sent to this PS5 {seconds} s ago and may still be starting. Wait a moment, or send it anyway.",
    );
  }
  const started = justStarted(host, helperAtBootSettleMs(loadBootRecord(host)), now);
  if (started) {
    return tr(
      "helper_send_just_started",
      { seconds: Math.max(1, Math.round((started.sendAt - now) / 1000)) },
      "The PS5 just started, and its jailbreak may still be loading: a helper sent now can crash it. Wait about {seconds} s, or send it anyway.",
    );
  }
  return null;
}

/** The person confirmed sending over work the console reported (useHelperReplaceGuard): the next
 *  send to that console within a few seconds goes as "Send anyway", so the engine does not ask
 *  again. */
const forcedAt = new Map<string, number>();
const FORCE_FOR_MS = 15_000;

export function allowForceOnce(host: string, now = Date.now()): void {
  forcedAt.set(host.trim().toLowerCase(), now);
}

function takeForce(host: string, now = Date.now()): boolean {
  const k = host.trim().toLowerCase();
  const at = forcedAt.get(k);
  forcedAt.delete(k);
  return at !== undefined && now - at < FORCE_FOR_MS;
}

/** Sends the bundled helper to `host` and waits for it to answer (see state/helperSend), wired
 *  to the real console. One implementation for every place that offers "Send helper".
 *
 *  The connection store describes the selected console only, so a send that ends after the
 *  user moved to another console never labels that one.
 *
 *  `force`: the person pressed "Send anyway" (past a helper still starting, a console that just
 *  started or one busy with work a restart stops). */
export async function sendHelperTo(
  host: string,
  tr: Translator,
  /** The version of the helper this send replaces, when the caller already cleared it from the
   *  store; by default the store's. Until the new helper answers, that one may still answer. */
  replacing?: string | null,
  opts: { force?: boolean } = {},
): Promise<HelperSendResult> {
  const target = host.trim();
  // "Send anyway" pressed on this step skips its own holds too; a confirmation in the replace
  // guard (busy work) only tells the engine not to ask again.
  const insisted = opts.force === true;
  const force = insisted || takeForce(target);
  const conn = () => useConnectionStore.getState();
  const old =
    replacing !== undefined ? replacing : conn().payloadStatusHost === target ? conn().payloadVersion : null;
  const app = await getAppVersion().catch(() => null);
  // Sending is the explicit "I want the helper": a "stopped by you" mark goes, and the
  // reconnect loop looks after this console again (#471).
  clearHelperStopped(target);
  // A helper sent moments ago, or a console that just started: say so first.
  const store = useHelperSendStore.getState();
  if (!insisted && helperSendFor(store, target)?.state !== "busy") {
    const hold = sendNowHold(target, tr);
    if (hold) {
      store.put(target, { state: "fail", phase: null, msg: hold, startedAtMs: Date.now(), forceable: true });
      return "fail";
    }
  }
  // Which run of the helper answered before the send: the new one is another (12.6.15).
  let instanceBefore: number | null = null;
  let unstamped = false;
  const sendOpts: HelperSendOptions = {
    mode: "explicit",
    force,
    ...(app ? { appVersion: app } : {}),
  };
  const result = await runHelperSend(
    target,
    {
      // A send to this console already under way (the automatic redeploy, or this button a
      // moment ago) is not raced: this one waits for that helper to come up instead of
      // starting a second, which made the two fight over the takeover for 20-40 s.
      waitForLoader: (h) => (sendInFlight(h) ? Promise.resolve("healthy" as const) : waitForLoader(h)),
      bundledPath: isTauriEnv() ? bundledPayloadPath : async () => "ps5upload.elf",
      send: async (ip, elf) => {
        if (sendInFlight(ip)) {
          // Wait out the send still running; it is the one to wait for.
          await sendHelperOnce(ip, async () => {}).catch(() => {});
          return;
        }
        const before = await payloadCheck(ip).catch(() => null);
        instanceBefore = before?.reachable ? (before.instanceId ?? null) : null;
        await sendHelperOnce(
          ip,
          async () => {
            if (isTauriEnv()) {
              const sent = await sendPayload(ip, elf, undefined, sendOpts).catch(withLoaderHint(ip));
              unstamped = sent.unstamped;
            } else {
              await engineSendsHelper(ip, sendOpts).catch(withLoaderHint(ip));
            }
          },
          { mode: "explicit" },
        );
      },
      check: payloadCheck,
      // The old helper answers until the new one takes over: a new run of the helper (another
      // instance id) is the new one, a same-version replace included. Without ids (an older
      // engine): only a different version, or this app's own.
      isNew: (p) => {
        if (instanceBefore !== null && p.instanceId != null) return p.instanceId !== instanceBefore;
        return !old || old === app || !p.payloadVersion || p.payloadVersion !== old;
      },
      isNotPaired: isNotPairedError,
      sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
      maxAttempts: NEW_HELPER_POLLS,
      setProbing: (h, on) => {
        if (conn().host === h) conn().setStatus({ payloadProbing: on });
      },
      onUp: (h, status) => {
        // The console's own record, mirrored to the screens while it is the selected one: the
        // flat fields alone were put back to the record's old "down" by a switch to another
        // tab and back, hiding the new helper until the next status poll.
        conn().setHostStatus(h, {
          payloadStatus: "up",
          payloadVersion: status.payloadVersion,
          ps5Kernel: status.ps5Kernel,
          ucredElevated: status.ucredElevated,
          priorInstance: status.priorInstance,
          power: "awake",
        });
        if (conn().host === h) conn().setStatus({ payloadProbing: false });
      },
      onOk: (h, msg) => {
        if (conn().host === h) conn().setStep2("ok", msg);
      },
      onNotPaired: (h, error) => reportIfNotPaired(error, h),
    },
    {
      checkingLoader: tr(
        "connection_checking_loader",
        undefined,
        "Checking the PS5's elfldr…",
      ),
      stuck: `${tr("connection_elfldr_stuck", undefined, STUCK_LOADER_MESSAGE)}\n\n${tr(
        "connection_loader_listener_hint",
        undefined,
        LISTENER_LOADER_HINT,
      )}`,
      sending: (elf) =>
        tr(
          "connection_sending_elf",
          { elf, host: target, port: PS5_LOADER_PORT },
          "Sending {elf} to {host}:{port}…",
        ),
      waiting: tr("helper_starting", undefined, "Starting the helper…"),
      running: tr(
        "connection_payload_running",
        { host: target },
        `Helper is running on ${target}`,
      ),
      timeout: (tail) => tr("connection_payload_timeout_v3", { tail }, TIMEOUT_EN),
      engineUnreachable: (error) =>
        tr(
          "connection_payload_engine_unreachable",
          { error },
          "The app could not reach its own engine on this computer, so it cannot tell whether the helper started on the PS5. This is not a PS5 problem: the notice at the top of the window says why and can restart the engine. Then press Send helper again. ({error})",
        ),
      notPaired: (error) => {
        if (error.toLowerCase().includes("different device")) {
          return tr("pairing_wrong_console_title", undefined, "A different PS5 answered at this address");
        }
        // Sent while the app's engine was down: the helper went without this app's key, which is
        // why the PS5 asks to pair.
        return unstamped
          ? tr(
              "helper_sent_unstamped",
              undefined,
              "The app's engine was not running when the helper was sent, so the helper does not know this app yet: the PS5 shows a pairing code. Pair once, and it is remembered.",
            )
          : tr("pairing_title", undefined, "Pair with your PS5");
      },
    },
  );
  // The send replaced the helper in memory only; an autoloader starts its stored copy at the
  // next boot. Update those copies in the background: whatever happens there, this send
  // stands (#540, #541). Not after a downgrade (a newer helper was replaced by this app's older
  // one, which the person confirmed): the copies the PS5 starts at boot stay the newer ones.
  const downgrade = !!old && !!app && compareVersions(old, app) === 1;
  if (result === "ok" && !downgrade) {
    void updateHelperCopiesAfterSend(target, tr).catch(() => {});
  }
  return result;
}

/** sendHelperTo for a flow that goes on once the helper is up (Quick bring-up, the setup
 *  wizard): a send to this console already running is waited out rather than reported as
 *  "busy". Resolves null when the helper answered, otherwise what the send ended with (an
 *  unpaired console has had pairing opened by then).
 *
 *  These flows want a helper running, not a new one: a helper of this app's version or newer
 *  is left alone ("Run again" restarted a current helper and stopped uploads), and `guard`
 *  (useHelperReplaceGuard) is asked before an older one is replaced. */
export async function sendHelperAndWait(
  host: string,
  tr: Translator,
  sleep: (ms: number) => Promise<void> = (ms) => new Promise((r) => setTimeout(r, ms)),
  check: (host: string) => Promise<{
    reachable: boolean;
    error?: string | null;
    payloadVersion?: string | null;
  }> = payloadCheck,
  guard?: (host: string) => Promise<boolean>,
): Promise<string | null> {
  const target = host.trim();
  // A send already under way is joined below, whatever the helper it replaces says now.
  if (!sendInFlight(target) && helperSendFor(useHelperSendStore.getState(), target)?.state !== "busy") {
    const running = await check(target).catch(() => null);
    const version = running?.reachable ? running.payloadVersion : null;
    const app = await getAppVersion().catch(() => null);
    if (version && app && (compareVersions(version, app) ?? -1) >= 0) {
      useConnectionStore.getState().setHostStatus(target, {
        payloadStatus: "up",
        payloadVersion: version,
      });
      return null;
    }
    if (guard && !(await guard(target))) {
      return tr(
        "helper_send_left_running",
        undefined,
        "The helper already running was left as it is.",
      );
    }
  }
  const result = await sendHelperTo(target, tr);
  const entry = () => helperSendFor(useHelperSendStore.getState(), target);
  if (result === "busy") {
    while (entry()?.state === "busy") await sleep(500);
  }
  if (result === "ok") return null;
  const end = entry();
  if (end?.state === "fail") return end.msg;
  // No record left: a joined send that ended well clears it, but so does a
  // failure the user dismissed. Only the helper itself can say which.
  const probe = await check(target).catch((e: unknown) => ({
    reachable: false,
    error: e instanceof Error ? e.message : String(e),
  }));
  if (probe.reachable) return null;
  const tail = probe.error ? ` (${probe.error})` : "";
  return tr("connection_payload_timeout_v3", { tail }, TIMEOUT_EN);
}
