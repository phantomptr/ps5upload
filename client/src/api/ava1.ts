// AVA1 routes the app calls on the engine directly: the pairing handshake and the
// PS5 to PS5 relay. Plain fetches against the engine origin, so the desktop app, the
// browser build and the Docker build all take the same path (no Tauri command, no
// `@tauri-apps` import: this file is shared with the browser build).

import { getEngineUrl } from "../state/engine";
import { consoleAddr } from "../lib/addr";

/** What the pairing dialog needs to know (engine `GET /api/ava1/pairing`).
 *  `code` is the six digits both screens show; `none` means no pairing is in progress. */
export type PairingView =
  | { state: "none" }
  | { state: "accepted" }
  | { state: "closed" }
  | { state: "code"; code: string; consoleName: string };

interface PairingWire {
  state?: string;
  code?: string;
  console_name?: string;
  error?: string;
}

function toView(w: PairingWire): PairingView {
  if (w.state === "code" && typeof w.code === "string") {
    return { state: "code", code: w.code, consoleName: w.console_name ?? "" };
  }
  if (w.state === "accepted" || w.state === "closed") return { state: w.state };
  return { state: "none" };
}

async function readWire(res: Response): Promise<PairingWire> {
  const body = (await res.json().catch(() => ({}))) as PairingWire;
  if (!res.ok) {
    throw new Error(body.error || `pairing request failed (${res.status})`);
  }
  return body;
}

/** Starts (or re-reads) the pairing handshake with the console. Asking again while the
 *  dialog is open returns the same code. Throws when the console cannot be reached. */
export async function pairingStatus(host: string): Promise<PairingView> {
  const res = await fetch(
    `${getEngineUrl()}/api/ava1/pairing?addr=${encodeURIComponent(consoleAddr(host))}`,
    { signal: AbortSignal.timeout(15_000) },
  );
  return toView(await readWire(res));
}

/** The user saw matching codes. Resolves `accepted`, or `closed` when the console's
 *  pairing window shut before the confirm arrived. */
export async function pairingConfirm(host: string): Promise<PairingView> {
  const res = await fetch(`${getEngineUrl()}/api/ava1/pairing/confirm`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ addr: consoleAddr(host) }),
    signal: AbortSignal.timeout(15_000),
  });
  return toView(await readWire(res));
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

/** Replaces an older helper through the engine (`POST /api/ps5/helper/replace`): its shutdown
 *  request, the stamped helper, a wait for the AVA1 port. Resolves `{replaced}`; throws an
 *  Error whose message carries the engine's token (`legacy_helper_wedged`, `helper_not_running`). */
export async function replaceHelper(host: string): Promise<{ replaced: boolean }> {
  const res = await fetch(`${getEngineUrl()}/api/ps5/helper/replace`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ host: consoleAddr(host) }),
  });
  const body = (await res.json().catch(() => ({}))) as {
    replaced?: boolean;
    error?: string;
  };
  if (!res.ok) throw new Error(body.error || `helper replace failed (${res.status})`);
  return { replaced: !!body.replaced };
}

/** The engine's view of the console's helper (`GET /api/ps5/helper/state`): `ava1`,
 *  `helper_old`, `starting`, `ava1_failed` or `not_running`. Null when the engine could not
 *  say (it never throws: a probe must not turn a failure into another one). */
export async function helperState(host: string): Promise<string | null> {
  try {
    const res = await fetch(
      `${getEngineUrl()}/api/ps5/helper/state?host=${encodeURIComponent(consoleAddr(host))}`,
      { signal: AbortSignal.timeout(8_000) },
    );
    if (!res.ok) return null;
    const body = (await res.json().catch(() => ({}))) as { state?: string };
    return typeof body.state === "string" ? body.state : null;
  } catch {
    return null;
  }
}
