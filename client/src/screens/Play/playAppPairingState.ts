/**
 * PS5Upload Play on the PS5: pairing it with this engine (engine play_app.rs). The app shows a
 * 6-character code on the TV; it is typed here once (POST /api/play/app/pair). The list is the
 * engine's paired consoles (GET /api/play/app/paired), each with Unpair.
 */
import { getEngineUrl } from "../../state/engine";

/** The letters a code may have (the engine's join codes: no I, L, O, 0, 1). */
const CODE_LETTERS = "ABCDEFGHJKMNPQRSTUVWXYZ23456789";
export const PAIR_CODE_LEN = 6;

export interface PairedApp {
  id: string;
  /** The console's address. */
  console: string;
  /** Its name on the console list when it paired, if it had one. */
  name: string | null;
  pairedUnix: number;
  lastSeenUnix: number | null;
  connected: boolean;
}

/** A PS5 whose TV shows a pairing code now: compare it and pair with one click. */
export interface WaitingPair {
  console: string;
  name: string | null;
  /** `K7M-2QX`, as the TV shows it; null from an engine that doesn't list it. */
  codeDisplay: string | null;
}

/** A paired PS5 that came back at a new address: PS5Upload confirms it is the same one. */
export interface MovedPair {
  id: string;
  name: string | null;
  addrOld: string;
  addrNew: string;
}

export interface PairingState {
  apps: PairedApp[];
  /** Consoles showing a code now. */
  waiting: WaitingPair[];
  /** Paired consoles at a new address, waiting for "same PS5?". */
  moves: MovedPair[];
  /** Pairing is locked (too many wrong codes) for this many seconds; null when it isn't. */
  lockedS: number | null;
}

/** What typing a code did. */
export type PairResult =
  | { kind: "ok"; console: string; name: string | null }
  | { kind: "wrong"; triesLeft: number | null }
  | { kind: "locked"; retryS: number | null }
  | { kind: "code" }
  | { kind: "app_gone" }
  | { kind: "not_waiting" }
  | { kind: "engine" };

/** A typed code as the engine takes it: capitals, no dashes or spaces, only its letters, 6 at most. */
export function normalizePairCode(input: string): string {
  return input
    .toUpperCase()
    .split("")
    .filter((c) => CODE_LETTERS.includes(c))
    .join("")
    .slice(0, PAIR_CODE_LEN);
}

/** `K7M2QX` as the TV shows it: `K7M-2QX`. */
export function displayPairCode(code: string): string {
  return code.length > 3 ? `${code.slice(0, 3)}-${code.slice(3)}` : code;
}

export function pairCodeReady(code: string): boolean {
  return normalizePairCode(code).length === PAIR_CODE_LEN;
}

const num = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);
const str = (v: unknown): string | null => (typeof v === "string" && v ? v : null);

/** GET /api/play/app/paired's answer; null for anything else (an engine from before it). */
export function readPairing(json: unknown): PairingState | null {
  if (!json || typeof json !== "object" || !Array.isArray((json as { apps?: unknown }).apps)) return null;
  const o = json as { apps: unknown[]; waiting?: unknown; moves?: unknown; locked_s?: unknown };
  const apps: PairedApp[] = [];
  for (const a of o.apps) {
    if (!a || typeof a !== "object") continue;
    const r = a as Record<string, unknown>;
    const id = str(r.id);
    const console = str(r.console);
    if (!id || !console) continue;
    apps.push({
      id,
      console,
      name: str(r.name),
      pairedUnix: num(r.paired_unix) ?? 0,
      lastSeenUnix: num(r.last_seen_unix),
      connected: r.connected === true,
    });
  }
  const waiting: WaitingPair[] = [];
  for (const w of Array.isArray(o.waiting) ? o.waiting : []) {
    const r = (w && typeof w === "object" ? w : {}) as Record<string, unknown>;
    const console = str(r.console);
    if (console) waiting.push({ console, name: str(r.name), codeDisplay: str(r.code_display) });
  }
  const moves: MovedPair[] = [];
  for (const m of Array.isArray(o.moves) ? o.moves : []) {
    const r = (m && typeof m === "object" ? m : {}) as Record<string, unknown>;
    const id = str(r.id);
    const addrOld = str(r.addr_old);
    const addrNew = str(r.addr_new);
    if (id && addrOld && addrNew) moves.push({ id, name: str(r.name), addrOld, addrNew });
  }
  const locked = num(o.locked_s);
  return { apps, waiting, moves, lockedS: locked && locked > 0 ? locked : null };
}

/** The pair route's answer as a result. */
export function readPairResult(ok: boolean, json: unknown): PairResult {
  const o = (json && typeof json === "object" ? json : {}) as Record<string, unknown>;
  if (ok && o.ok === true) return { kind: "ok", console: str(o.console) ?? "", name: str(o.name) };
  switch (o.error) {
    case "wrong_code":
      return { kind: "wrong", triesLeft: num(o.tries_left) };
    case "locked":
      return { kind: "locked", retryS: num(o.retry_s) };
    case "code":
      return { kind: "code" };
    case "app_gone":
      return { kind: "app_gone" };
    case "not_waiting":
      return { kind: "not_waiting" };
    default:
      return { kind: "engine" };
  }
}

type Tr = (key: string, vars: Record<string, string | number> | undefined, fallback: string) => string;

/** What a pairing result says, in words. */
export function pairResultText(tr: Tr, r: PairResult): string {
  switch (r.kind) {
    case "ok": {
      const name = r.name ?? r.console;
      return tr("pa_paired", { name }, `Paired with ${name}.`);
    }
    case "wrong":
      return r.triesLeft !== null
        ? tr("pa_wrong_left", { n: r.triesLeft }, `That code doesn't match the one on the TV. ${r.triesLeft} tries left.`)
        : tr("pa_wrong", undefined, "That code doesn't match the one on the TV.");
    case "locked": {
      const min = Math.max(1, Math.ceil((r.retryS ?? 600) / 60));
      return tr("pa_locked", { min }, `Too many wrong codes. Try again in ${min} min, with a new code from the TV.`);
    }
    case "code":
      return tr("pa_bad_code", undefined, "Type the 6 letters and numbers the TV shows.");
    case "app_gone":
      return tr("pa_app_gone", undefined, "PS5Upload Play closed before it was paired. Open it on the PS5 and get a new code.");
    case "not_waiting":
      return tr("pa_not_waiting", undefined, "That TV isn't showing a code any more. Choose Pair in PS5Upload Play again.");
    default:
      return tr("pa_engine", undefined, "Couldn't pair. Check that the engine is up to date.");
  }
}

async function call(path: string, body?: object): Promise<{ ok: boolean; json: unknown }> {
  const r = await fetch(`${getEngineUrl()}${path}`, {
    method: body ? "POST" : "GET",
    headers: body ? { "Content-Type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  const json: unknown = await r.json().catch(() => null);
  return { ok: r.ok, json };
}

export const playAppApi = {
  /** The paired consoles; null when the engine doesn't answer or has no PS5Upload Play pairing. */
  list: async (): Promise<PairingState | null> => {
    try {
      const r = await call("/api/play/app/paired");
      return r.ok ? readPairing(r.json) : null;
    } catch {
      return null;
    }
  },
  pair: async (code: string): Promise<PairResult> => {
    try {
      const r = await call("/api/play/app/pair", { code: normalizePairCode(code) });
      return readPairResult(r.ok, r.json);
    } catch {
      return { kind: "engine" };
    }
  },
  /** One click: the code on that TV is the one shown here (POST /api/play/app/pair_confirm). */
  confirm: async (console: string): Promise<PairResult> => {
    try {
      const r = await call("/api/play/app/pair_confirm", { console });
      return readPairResult(r.ok, r.json);
    } catch {
      return { kind: "engine" };
    }
  },
  /** A paired PS5 at a new address: the same one (ok) or not (POST /api/play/app/moved). */
  moved: async (m: MovedPair, ok: boolean): Promise<boolean> => {
    try {
      const r = await call("/api/play/app/moved", { console_old: m.addrOld, addr: m.addrNew, ok });
      return r.ok;
    } catch {
      return false;
    }
  },
  unpair: async (id: string): Promise<boolean> => {
    try {
      const r = await call("/api/play/app/unpair", { id });
      return r.ok && (r.json as { ok?: unknown } | null)?.ok === true;
    } catch {
      return false;
    }
  },
};
