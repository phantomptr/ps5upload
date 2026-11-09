// AVA1 routes the app calls on the engine directly: the pairing handshake and the
// PS5 to PS5 relay. Plain fetches against the engine origin, so the desktop app, the
// browser build and the Docker build all take the same path (no Tauri command, no
// `@tauri-apps` import: this file is shared with the browser build).

import { getEngineUrl } from "../state/engine";
import { consoleAddr } from "../lib/addr";
import { fetchStart } from "../lib/startRequestTimeout";

/** What the pairing dialog needs to know (engine `GET /api/ava1/pairing`). Passkey entry:
 *  the console shows a six-digit code on its own screen and the user types it into the app,
 *  so no code is ever sent to the app. `wrong_code`: what was typed was not the console's
 *  (type it again; a console-side refusal also shows a new code). `wrong_console`: another
 *  PS5 answers at the address this app pinned. `none`: no pairing is in progress. */
export type PairingView =
  | { state: "none" }
  | { state: "accepted" }
  | { state: "closed" }
  | { state: "blocked" }
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
    w.state === "blocked" ||
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

/** Who answers at `host`: the console's AVA1 key (hex, the same console at every address) and the
 *  name it gives, read without pairing (engine `GET /api/ava1/console`). null when nothing answers
 *  or the engine predates the route. */
export async function consoleIdentity(
  host: string,
): Promise<{ key: string; name: string } | null> {
  try {
    const q = new URLSearchParams({ addr: consoleAddr(host) });
    const res = await fetch(`${getEngineUrl()}/api/ava1/console?${q}`, {
      signal: AbortSignal.timeout(6_000),
    });
    if (!res.ok) return null;
    const body = (await res.json().catch(() => ({}))) as { key?: unknown; name?: unknown };
    return typeof body.key === "string" && /^[0-9a-f]{64}$/i.test(body.key)
      ? { key: body.key.toLowerCase(), name: typeof body.name === "string" ? body.name : "" }
      : null;
  } catch {
    return null;
  }
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
  /** #444: `auto` (absent), `direct` or `relay`, and what the sending console dials. */
  route?: { mode?: "auto" | "direct" | "relay"; directAddr?: string | null },
  /** #444, a send another device started (`txId` is its id): `take` ends it there and sends
   *  again from here, resuming; `watch` follows a direct send still running. */
  adopt?: "take" | "watch" | null,
): Promise<string> {
  const res = await fetchStart("the PS5 to PS5 copy", `${getEngineUrl()}/api/transfer/ps5-to-ps5`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      from: consoleAddr(from),
      src,
      to: consoleAddr(to),
      dest,
      tx_id: txId ?? null,
      route: route?.mode ?? "auto",
      direct_addr: route?.directAddr?.trim() || null,
      ...(adopt ? { adopt } : {}),
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

/** How a console-to-console send reads to this app (engine `GET .../ps5-to-ps5/remote`). */
export type RemoteSendClass = "mine" | "running_elsewhere" | "paused" | "interrupted";

/** One send into a console, from the shared record the driving device keeps there (#444). */
export interface RemoteSend {
  txId: string;
  from: string;
  to: string;
  src: string;
  dest: string;
  route: "auto" | "direct" | "relay";
  directAddr: string | null;
  ownerName: string;
  /** Started from this app's engine (another run of it, or before a restart). */
  mine: boolean;
  cls: RemoteSendClass;
  /** False when an older helper could not say whether it still runs. */
  confirmed: boolean;
  liveRoute: "direct" | "relay" | null;
  bytesDone: number;
  bytesTotal: number;
  updatedMs: number;
  error: string | null;
  canWatch: boolean;
  canTakeOver: boolean;
}

export interface RemoteSends {
  sends: RemoteSend[];
  /** The receiving console's helper shares transfers with every paired device; null = unknown. */
  sharedJobs: boolean | null;
}

const ROUTES = ["auto", "direct", "relay"] as const;
const CLASSES: readonly RemoteSendClass[] = ["mine", "running_elsewhere", "paused", "interrupted"];

/** The engine's answer, read defensively (an older engine has no such route). */
export function parseRemoteSends(raw: unknown): RemoteSends {
  const o = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
  const list = Array.isArray(o.sends) ? o.sends : [];
  const str = (v: unknown) => (typeof v === "string" ? v : "");
  const num = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? v : 0);
  const sends: RemoteSend[] = [];
  for (const item of list) {
    if (!item || typeof item !== "object") continue;
    const s = item as Record<string, unknown>;
    const cls = CLASSES.find((c) => c === s.class);
    if (!cls || !/^[0-9a-f]{32}$/.test(str(s.tx_id))) continue;
    const live = s.live_route === "direct" || s.live_route === "relay" ? s.live_route : null;
    sends.push({
      txId: str(s.tx_id),
      from: str(s.from),
      to: str(s.to),
      src: str(s.src),
      dest: str(s.dest),
      route: ROUTES.find((r) => r === s.route) ?? "auto",
      directAddr: str(s.direct_addr) || null,
      ownerName: str(s.owner_name),
      mine: s.mine === true,
      cls,
      confirmed: s.confirmed !== false,
      liveRoute: live,
      bytesDone: num(s.bytes_done),
      bytesTotal: num(s.bytes_total),
      updatedMs: num(s.updated_ms),
      error: str(s.error) || null,
      canWatch: s.can_watch === true,
      canTakeOver: s.can_take_over === true,
    });
  }
  return { sends, sharedJobs: typeof o.shared_jobs === "boolean" ? o.shared_jobs : null };
}

/** The sends into console `host` that this app could watch, take over or resume. */
export async function fetchRemoteSends(host: string): Promise<RemoteSends> {
  const res = await fetch(
    `${getEngineUrl()}/api/transfer/ps5-to-ps5/remote?addr=${encodeURIComponent(consoleAddr(host))}`,
    { signal: AbortSignal.timeout(20_000) },
  );
  const body = (await res.json().catch(() => ({}))) as { error?: string };
  if (!res.ok) throw new Error(body.error || `listing sends failed (${res.status})`);
  return parseRemoteSends(body);
}

/** One connect from the sending console, or one leg through this device. */
export interface ReachResult {
  /** `reachable` `refused` `timeout` `unreachable` `bad_address` `helper_too_old` `error`, or
   *  for a relay leg `ok` / the failure's reason. */
  verdict: string;
  ms: number | null;
  detail: string | null;
  target: string | null;
}

export interface ConnectionTest {
  direct: ReachResult | null;
  relay: { from: ReachResult; to: ReachResult } | null;
  deep: { ok: boolean; ms: number | null; detail: string | null } | null;
}

function reach(v: unknown): ReachResult {
  const o = (v && typeof v === "object" ? v : {}) as Record<string, unknown>;
  return {
    verdict: typeof o.verdict === "string" ? o.verdict : "error",
    ms: typeof o.ms === "number" ? o.ms : null,
    detail: typeof o.detail === "string" ? o.detail : null,
    target: typeof o.target === "string" ? o.target : null,
  };
}

export function parseConnectionTest(raw: unknown): ConnectionTest {
  const o = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
  const relay = o.relay && typeof o.relay === "object" ? (o.relay as Record<string, unknown>) : null;
  const deep = o.deep && typeof o.deep === "object" ? (o.deep as Record<string, unknown>) : null;
  return {
    direct: o.direct ? reach(o.direct) : null,
    relay: relay ? { from: reach(relay.from), to: reach(relay.to) } : null,
    deep: deep
      ? {
          ok: deep.ok === true,
          ms: typeof deep.ms === "number" ? deep.ms : null,
          detail: typeof deep.detail === "string" ? deep.detail : null,
        }
      : null,
  };
}

/** "Test connection" in Send to another console (#444): can `from` reach `to` the way a send
 *  would dial it, and (Auto, relay) can this device reach both? `deep` also sends 4 KiB. */
export async function testPs5Connection(
  from: string,
  to: string,
  route: { mode: "auto" | "direct" | "relay"; directAddr: string },
  deep = false,
): Promise<ConnectionTest> {
  const res = await fetch(`${getEngineUrl()}/api/transfer/ps5-to-ps5/test`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      from: consoleAddr(from),
      to: consoleAddr(to),
      route: route.mode,
      direct_addr: route.directAddr.trim() || null,
      deep,
    }),
    signal: AbortSignal.timeout(deep ? 60_000 : 30_000),
  });
  const body = (await res.json().catch(() => ({}))) as { error?: string };
  if (!res.ok) throw new Error(body.error || `the connection test failed (${res.status})`);
  return parseConnectionTest(body);
}

/** Whether the console at `newHost` is the one paired at `oldHost` (engine
 *  `POST /api/ava1/same-console`: it compares the console's key with the one pinned for the old
 *  address). Anything it cannot read, an engine without the route included, is `unknown`: only
 *  `same` lets the app offer the new address. */
export async function sameConsole(
  oldHost: string,
  newHost: string,
): Promise<"same" | "different" | "unknown"> {
  try {
    const res = await fetch(`${getEngineUrl()}/api/ava1/same-console`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ old: consoleAddr(oldHost), new: consoleAddr(newHost) }),
      signal: AbortSignal.timeout(15_000),
    });
    if (!res.ok) return "unknown";
    const body = (await res.json().catch(() => ({}))) as { result?: string };
    return body.result === "same" || body.result === "different" ? body.result : "unknown";
  } catch {
    return "unknown";
  }
}
