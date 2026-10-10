import { bundledPayloadPath, payloadCheck, sendPayload } from "../api/ps5";
import { PS5_LOADER_PORT } from "../lib/addr";
import { isNotPairedError, reportIfNotPaired } from "../lib/consoleSession";
import { STUCK_LOADER_MESSAGE, waitForLoader } from "../lib/elfldrGuard";
import { useConnectionStore } from "./connection";
import {
  helperSendFor,
  runHelperSend,
  useHelperSendStore,
  type HelperSendResult,
} from "./helperSend";
import type { Translator } from "./lang";
import { invoke } from "../lib/invokeLogged";
import { sendHelperOnce, sendInFlight, sentAgoMs } from "../lib/helperSendGate";
import { isTauriEnv } from "../lib/tauriEnv";
import { trStatic } from "../lib/trStatic";

/** The web UI has no helper file to send and no socket to the console: the engine sends its
 *  own bundled helper, stamped with its key, so it is paired by the send (#415). */
/** "connect …:9021: Connection refused" says only that the send failed; say what to do. */
export function loaderHint(error: string): string {
  return /:9021\b.*(refused|reset|unreachable|timed out)/i.test(error)
    ? trStatic(
        "connection_loader_down",
        "The PS5's loader (port 9021) isn't running. Load elfldr on the PS5 (your jailbreak or autoloader does it), then send the helper again. ({error})",
      ).replace("{error}", error)
    : error;
}

async function engineSendsHelper(ip: string): Promise<void> {
  const r = (await invoke("payload_restore", { ip })) as { ok?: boolean; error?: string };
  if (!r?.ok) throw new Error(loaderHint(r?.error ?? "the engine has no helper to send"));
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
export function sendHelperTo(
  host: string,
  tr: Translator,
): Promise<HelperSendResult> {
  const target = host.trim();
  const conn = () => useConnectionStore.getState();
  return runHelperSend(
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
          isTauriEnv() ? sendPayload(ip, elf) : engineSendsHelper(ip),
        );
      },
      check: payloadCheck,
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
      stuck: tr("connection_elfldr_stuck", undefined, STUCK_LOADER_MESSAGE),
      sending: (elf) =>
        tr(
          "connection_sending_elf",
          { elf, host: target, port: PS5_LOADER_PORT },
          "Sending {elf} to {host}:{port}…",
        ),
      waiting: tr(
        "connection_waiting_boot",
        undefined,
        "Waiting for payload to boot…",
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
}

/** sendHelperTo for a flow that goes on once the helper is up (Quick bring-up, the setup
 *  wizard): a send to this console already running is waited out rather than reported as
 *  "busy". Resolves null when the helper answered, otherwise what the send ended with (an
 *  unpaired console has had pairing opened by then). */
export async function sendHelperAndWait(
  host: string,
  tr: Translator,
  sleep: (ms: number) => Promise<void> = (ms) => new Promise((r) => setTimeout(r, ms)),
): Promise<string | null> {
  const target = host.trim();
  const result = await sendHelperTo(target, tr);
  const entry = () => helperSendFor(useHelperSendStore.getState(), target);
  if (result === "busy") {
    while (entry()?.state === "busy") await sleep(500);
  }
  if (result === "ok") return null;
  const end = entry();
  return end?.state === "fail" ? end.msg : null;
}
