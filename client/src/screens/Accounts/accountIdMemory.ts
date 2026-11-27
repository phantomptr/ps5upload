// The account IDs this app set, per console and user, kept on this device (#971).
//
// An account ID set here can be gone after the PS5 restarts (seen after a restart that followed
// the offline sign-in on 13.60). Kept here, Play › Set up can say so and put the same ID back in
// one press: the same ID matters, saves made under it are tied to it.

import { safeGetItem, safeSetItem } from "../../lib/safeStorage";
import { accountIdHexDigits } from "./accountId";

const KEY = "ps5upload.account_ids.v1";

export interface KeptAccountId {
  /** 0x-prefixed hex, as written. */
  id: string;
  /** The user's name when it was written (said back to the user). */
  name: string;
}

type Kept = Record<string, KeptAccountId>;

function keyOf(host: string, uid: number): string {
  return `${host.trim()}|${uid >>> 0}`;
}

function load(): Kept {
  try {
    const v = JSON.parse(safeGetItem(KEY) ?? "{}") as unknown;
    if (!v || typeof v !== "object") return {};
    const out: Kept = {};
    for (const [k, x] of Object.entries(v as Record<string, unknown>)) {
      const e = x as Partial<KeptAccountId> | null;
      if (e && typeof e.id === "string" && accountIdHexDigits(e.id) !== null) {
        out[k] = { id: e.id, name: typeof e.name === "string" ? e.name : "" };
      }
    }
    return out;
  } catch {
    return {};
  }
}

/** Remember the account ID just written (and read back) for this console's user. */
export function keepAccountId(host: string, uid: number, id: string, name: string): void {
  if (!host.trim() || !uid || accountIdHexDigits(id) === null) return;
  const all = load();
  all[keyOf(host, uid)] = { id, name };
  safeSetItem(KEY, JSON.stringify(all));
}

/** The account ID this app last set for this console's user, or null. */
export function keptAccountId(host: string, uid: number): KeptAccountId | null {
  if (!host.trim() || !uid) return null;
  return load()[keyOf(host, uid)] ?? null;
}

/** The same 8 bytes, whatever the spelling ("0x00ab" and "0xAB", hex or decimal). An absent or
 *  zero id is the same as no id. */
export function sameAccountId(a: string | null | undefined, b: string | null | undefined): boolean {
  const norm = (x: string | null | undefined) => (accountIdHexDigits(x) ?? "").replace(/^0+/, "");
  return norm(a) === norm(b);
}

/** The ID this app set for the user is no longer on the console: the slot reads another one, or
 *  none. */
export function keptIdLost(kept: KeptAccountId | null, current: string | null | undefined): boolean {
  return !!kept && !sameAccountId(kept.id, current);
}
