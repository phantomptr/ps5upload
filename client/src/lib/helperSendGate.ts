// One helper send per console at a time, for every place that sends one.
//
// The Send helper button (state/helperSendRuntime) and the automatic redeploy
// (lib/ensurePayloadCurrent: queues, recovery) each guarded only against themselves, so a
// click that met an automatic redeploy sent two helpers within milliseconds. Two helpers
// starting together fight over the takeover: the new one waits out the old one's ports, then
// kills it, and "Send helper" sat on "Waiting for payload to boot…" for 20-40 s while the
// console had shown the ELF arriving at once (the engine log showed the two loader checks
// 13 ms apart).
//
// 12.6.15: the cooldown is checked here, in the same step that starts the send (`minGapMs`), so a
// caller that read "nothing sent lately" a moment ago cannot send right behind another one; one
// window (RESEND_COOLDOWN_MS) for the buttons and the automatic sends alike. The engine checks it
// again under its loader lock, for every front end.

import { hostOf } from "./addr";

/** No second helper this soon after one was sent to the console: it is still starting. The same
 *  window as the engine's (helper_send.rs RESEND_COOLDOWN). */
export const RESEND_COOLDOWN_MS = 45_000;

interface Running {
  p: Promise<void>;
  /** Who started it: an unattended send or one someone asked for. */
  mode: "auto" | "explicit";
}

const inFlight = new Map<string, Running>();
const lastSentAt = new Map<string, number>();

/** The one key every per-console table of a helper send uses: the host, no port, lower case. */
export function consoleKey(host: string): string {
  return hostOf(host.trim()).toLowerCase();
}

export type SendOnceResult = "sent" | "joined" | "recent";

/** Sends with `send` unless a send to this console is already running: then waits for that
 *  one instead ("joined"). With `minGapMs`, a console sent a helper less than that long ago gets
 *  none ("recent"), decided in the same step as the send starts. A send that throws records
 *  nothing and rethrows. */
export async function sendHelperOnce(
  host: string,
  send: () => Promise<unknown> | unknown,
  opts: { minGapMs?: number; mode?: "auto" | "explicit"; now?: () => number } = {},
): Promise<SendOnceResult> {
  const key = consoleKey(host);
  const now = opts.now ?? Date.now;
  const running = inFlight.get(key);
  if (running) {
    await running.p.catch(() => {});
    return "joined";
  }
  const at = lastSentAt.get(key);
  if (opts.minGapMs !== undefined && at !== undefined && now() - at < opts.minGapMs) return "recent";
  const p = Promise.resolve()
    .then(send)
    .then(() => {
      lastSentAt.set(key, now());
    });
  inFlight.set(key, { p, mode: opts.mode ?? "explicit" });
  try {
    await p;
    return "sent";
  } finally {
    inFlight.delete(key);
  }
}

/** Records a send that broke after the loader took it (the console may run it): it counts for the
 *  cooldown like one that went. */
export function noteSentAnyway(host: string, now = Date.now()): void {
  lastSentAt.set(consoleKey(host), now);
}

/** How long ago this console was last sent a helper (ms), or null when it never was. */
export function sentAgoMs(host: string, now = Date.now()): number | null {
  const at = lastSentAt.get(consoleKey(host));
  return at === undefined ? null : now - at;
}

/** Whether a send to this console is running now. */
export function sendInFlight(host: string): boolean {
  return inFlight.has(consoleKey(host));
}

/** Who started the send running now, or null. */
export function sendInFlightMode(host: string): "auto" | "explicit" | null {
  return inFlight.get(consoleKey(host))?.mode ?? null;
}

/** Whether a failed send got past the connection to the loader, so the console may have (some of)
 *  the helper: the engine's (core payload_lifecycle) words for a stream that broke once the loader
 *  had taken it. Anything else (never reached, no helper to send, a refusal) is no send. */
export function sendFailedPastConnect(message: string): boolean {
  return /^(write |send timed out|flush |shutdown: |half-close |set write timeout)/i.test(message.trim());
}

/** Test seam. */
export function resetHelperSendGate(): void {
  inFlight.clear();
  lastSentAt.clear();
}
