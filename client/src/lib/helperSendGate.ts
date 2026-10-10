// One helper send per console at a time, for every place that sends one.
//
// The Send helper button (state/helperSendRuntime) and the automatic redeploy
// (lib/ensurePayloadCurrent: queues, recovery) each guarded only against themselves, so a
// click that met an automatic redeploy sent two helpers within milliseconds. Two helpers
// starting together fight over the takeover: the new one waits out the old one's ports, then
// kills it, and "Send helper" sat on "Waiting for payload to boot…" for 20-40 s while the
// console had shown the ELF arriving at once (the engine log showed the two loader checks
// 13 ms apart).

import { hostOf } from "./addr";

const inFlight = new Map<string, Promise<void>>();
const lastSentAt = new Map<string, number>();

const keyOf = (host: string) => hostOf(host.trim()).toLowerCase();

/** Sends with `send` unless a send to this console is already running: then waits for that
 *  one instead ("joined"). A send that throws records nothing and rethrows. */
export async function sendHelperOnce(
  host: string,
  send: () => Promise<unknown> | unknown,
): Promise<"sent" | "joined"> {
  const key = keyOf(host);
  const running = inFlight.get(key);
  if (running) {
    await running.catch(() => {});
    return "joined";
  }
  const p = Promise.resolve()
    .then(send)
    .then(() => {
      lastSentAt.set(key, Date.now());
    });
  inFlight.set(key, p);
  try {
    await p;
    return "sent";
  } finally {
    inFlight.delete(key);
  }
}

/** How long ago this console was last sent a helper (ms), or null when it never was. */
export function sentAgoMs(host: string, now = Date.now()): number | null {
  const at = lastSentAt.get(keyOf(host));
  return at === undefined ? null : now - at;
}

/** Whether a send to this console is running now. */
export function sendInFlight(host: string): boolean {
  return inFlight.has(keyOf(host));
}

/** Test seam. */
export function resetHelperSendGate(): void {
  inFlight.clear();
  lastSentAt.clear();
}
