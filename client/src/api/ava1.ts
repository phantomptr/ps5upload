// AVA1 routes the app calls on the engine directly: the pairing handshake and the
// PS5 to PS5 relay. Plain fetches against the engine origin, so the desktop app, the
// browser build and the Docker build all take the same path (no Tauri command, no
// `@tauri-apps` import: this file is shared with the browser build).

import { getEngineUrl } from "../state/engine";
import { consoleAddr } from "../lib/addr";

/** What the pairing dialog needs to know (engine `GET /api/ava1/pairing`). Passkey entry:
 *  the console shows a six-digit code on its own screen and the user types it into the app,
 *  so no code is ever sent to the app. `wrong_code`: what was typed was not the console's
 *  (type it again; a console-side refusal also shows a new code). `wrong_console`: another
 *  PS5 answers at the address this app pinned. `none`: no pairing is in progress. */
export type PairingView =
  | { state: "none" }
  | { state: "accepted" }
  | { state: "closed" }
  | { state: "wrong_console" }
  | { state: "code"; consoleName: string }
  | { state: "wrong_code"; consoleName: string };

interface PairingWire {
  state?: string;
  console_name?: string;
  error?: string;
}

function toView(w: PairingWire): PairingView {
  if (w.state === "code" || w.state === "wrong_code") {
    return { state: w.state, consoleName: w.console_name ?? "" };
  }
  if (
    w.state === "accepted" ||
    w.state === "closed" ||
    w.state === "wrong_console"
  ) {
    return { state: w.state };
  }
  return { state: "none" };
}

async function readWire(res: Response): Promise<PairingWire> {
  const body = (await res.json().catch(() => ({}))) as PairingWire;
  if (!res.ok) {
    throw new Error(body.error || `pairing request failed (${res.status})`);
  }
  return body;
}

/** Starts (or re-reads) the pairing handshake with the console (`POST /pairing/start`: it
 *  makes the console show a code, so it is not a GET). Asking again while the dialog is open
 *  returns the same handshake. Throws when the console cannot be reached. */
export async function pairingStatus(host: string): Promise<PairingView> {
  const res = await fetch(`${getEngineUrl()}/api/ava1/pairing/start`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ addr: consoleAddr(host) }),
    signal: AbortSignal.timeout(15_000),
  });
  return toView(await readWire(res));
}

/** The user typed the code the console shows (six digits, a string: leading zeros count).
 *  Resolves `accepted`; `wrong_code` when it was not the console's (the console shows a new
 *  code, or the same one for a typo); `closed` when five wrong codes shut its window. */
export async function pairingConfirm(
  host: string,
  code: string,
): Promise<PairingView> {
  const res = await fetch(`${getEngineUrl()}/api/ava1/pairing/confirm`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ addr: consoleAddr(host), code }),
    signal: AbortSignal.timeout(15_000),
  });
  return toView(await readWire(res));
}

/** The dialog was dismissed: the engine closes the pending handshake, which would
 *  otherwise hold one of the console's two unconfirmed places for a minute. */
export async function pairingCancel(host: string): Promise<void> {
  await fetch(`${getEngineUrl()}/api/ava1/pairing/cancel`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ addr: consoleAddr(host) }),
    signal: AbortSignal.timeout(5_000),
  }).catch(() => undefined);
}

/** From this (paired) app, open the console's pairing window for five minutes so another
 *  device can pair with a code. */
export async function pairingAllow(host: string): Promise<void> {
  const res = await fetch(`${getEngineUrl()}/api/ava1/pairing/allow`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ addr: consoleAddr(host) }),
    signal: AbortSignal.timeout(15_000),
  });
  await readWire(res);
}

/** "Forget the old console": removes the key pinned for this address so a different PS5
 *  there can be paired. */
export async function pairingForget(host: string): Promise<void> {
  const res = await fetch(`${getEngineUrl()}/api/ava1/pairing/forget`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ addr: consoleAddr(host) }),
    signal: AbortSignal.timeout(10_000),
  });
  await readWire(res);
}

/** Copy `src` on console `from` to `dest` on console `to` through this engine. Returns the
 *  job id; progress is the ordinary job snapshot every upload card already renders. */
export async function startPs5ToPs5(
  from: string,
  src: string,
  to: string,
  dest: string,
  txId?: string | null,
): Promise<string> {
  const res = await fetch(`${getEngineUrl()}/api/transfer/ps5-to-ps5`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      from: consoleAddr(from),
      src,
      to: consoleAddr(to),
      dest,
      tx_id: txId ?? null,
    }),
  });
  const body = (await res.json().catch(() => ({}))) as {
    job_id?: string;
    error?: string;
  };
  if (!res.ok || !body.job_id) {
    throw new Error(body.error || `PS5 to PS5 failed to start (${res.status})`);
  }
  return body.job_id;
}
