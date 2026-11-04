import { bundledPayloadPath, payloadCheck, sendPayload } from "../api/ps5";
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
import { sendHelperOnce, sendInFlight, sentAgoMs } from "../lib/helperSendGate";
import { isTauriEnv } from "../lib/tauriEnv";
import { trStatic } from "../lib/trStatic";
import { getAppVersion } from "../lib/appVersion";
import { compareVersions } from "../lib/semver";
import { engineSendsHelper } from "../lib/restoreMainPayload";

/** The web UI has no helper file to send and no socket to the console: the engine sends its
 *  own bundled helper, stamped with its key, so it is paired by the send (#415). */
/** "connect …:9021: Connection refused" says only that the send failed; say what to do. */
export function loaderHint(error: string): string {
  return /:9021\b.*(refused|reset|unreachable|timed out|timeout)/i.test(error)
    ? trStatic(
        "connection_loader_down",
        "The PS5's loader (port 9021) isn't running. Load elfldr on the PS5 (your jailbreak or autoloader does it), then send the helper again. ({error})",
      ).replace("{error}", error)
    : error;
}

function withLoaderHint(e: unknown): never {
  throw new Error(loaderHint(e instanceof Error ? e.message : String(e)));
}

/** A helper sent to this console moments ago is still starting: a button press in this window
 *  waits for it rather than sending another (a retry after a failed start, later, sends). */
const JOIN_RECENT_MS = 15_000;

function joinsRecentSend(host: string): boolean {
  const ago = sentAgoMs(host);
  return sendInFlight(host) || (ago !== null && ago < JOIN_RECENT_MS);
}

// Says what to do first; the probe's technical detail ({tail}) goes last, where it
// doesn't push the advice out of view.
const TIMEOUT_EN =
  "The helper didn't start within 20 seconds. Send it again. If it fails again, load elfldr and kstuff first, then send the helper.{tail}";

/** Sends the bundled helper to `host` and waits for it to answer (see state/helperSend), wired
 *  to the real console. One implementation for every place that offers "Send helper".
 *
 *  The connection store describes the selected console only, so a send that ends after the
 *  user moved to another console never labels that one. */
export async function sendHelperTo(
  host: string,
  tr: Translator,
  /** The version of the helper this send replaces, when the caller already cleared it from the
   *  store; by default the store's. Until the new helper answers, that one may still answer. */
  replacing?: string | null,
): Promise<HelperSendResult> {
  const target = host.trim();
  const conn = () => useConnectionStore.getState();
  const old =
    replacing !== undefined ? replacing : conn().payloadStatusHost === target ? conn().payloadVersion : null;
  const app = await getAppVersion().catch(() => null);
  // Sending is the explicit "I want the helper": a "stopped by you" mark goes, and the
  // reconnect loop looks after this console again (#471).
  clearHelperStopped(target);
  const result = await runHelperSend(
    target,
    {
      // A send to this console already under way (the automatic redeploy, or this button a
      // moment ago) is not raced: this one waits for that helper to come up instead of
      // starting a second, which made the two fight over the takeover for 20-40 s.
      waitForLoader: (h) =>
        joinsRecentSend(h) ? Promise.resolve("healthy" as const) : waitForLoader(h),
      bundledPath: isTauriEnv() ? bundledPayloadPath : async () => "ps5upload.elf",
      send: async (ip, elf) => {
        if (joinsRecentSend(ip)) {
          // Wait out the send still running, if any; one already done needs nothing more.
          if (sendInFlight(ip)) await sendHelperOnce(ip, async () => {}).catch(() => {});
          return;
        }
        await sendHelperOnce(ip, () =>
          (isTauriEnv() ? sendPayload(ip, elf) : engineSendsHelper(ip)).catch(withLoaderHint),
        );
      },
      check: payloadCheck,
      // The old helper answers until the new one takes over: only a different version (or this
      // app's own) is the new one. Same version as before: nothing tells them apart, any answer.
      isNew: (p) => !old || old === app || !p.payloadVersion || p.payloadVersion !== old,
      isNotPaired: isNotPairedError,
      sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
      setProbing: (h, on) => {
        if (conn().host === h) conn().setStatus({ payloadProbing: on });
      },
      onUp: (h, status) => {
        if (conn().host !== h) return;
        conn().setStatus({
          payloadStatus: "up",
          payloadStatusHost: h,
          payloadVersion: status.payloadVersion,
          ps5Kernel: status.ps5Kernel,
          ucredElevated: status.ucredElevated,
          priorInstance: status.priorInstance,
          power: "awake",
          payloadProbing: false,
        });
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
      waiting: tr(
        "connection_waiting_boot",
        undefined,
        "Waiting for the helper to start…",
      ),
      running: tr(
        "connection_payload_running",
        { host: target },
        `Helper is running on ${target}`,
      ),
      timeout: (tail) => tr("connection_payload_timeout_v2", { tail }, TIMEOUT_EN),
      engineUnreachable: (error) =>
        tr(
          "connection_payload_engine_unreachable",
          { error },
          "The app could not reach its own engine on this computer, so it cannot tell whether the helper started on the PS5. This is not a PS5 problem: the notice at the top of the window says why and can restart the engine. Then press Send helper again. ({error})",
        ),
      notPaired: (error) =>
        error.toLowerCase().includes("different device")
          ? tr(
              "pairing_wrong_console_title",
              undefined,
              "A different PS5 answered at this address",
            )
          : tr("pairing_title", undefined, "Pair with your PS5"),
    },
  );
  // The send replaced the helper in memory only; an autoloader starts its stored copy at the
  // next boot. Update those copies in the background: whatever happens there, this send
  // stands (#540, #541).
  if (result === "ok") {
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
  return tr("connection_payload_timeout_v2", { tail }, TIMEOUT_EN);
}
