// The PS5 users a console is paired as, for Remote Play (11.5).
//
// A pairing (registration) is made for one PSN account, and a stream signs in the user it was made
// for. So a profile keeps one pairing per user in `rp_users`, and its own `wake_*` keys stay what
// every caller already uses (Start, Wake, Wake & sign in): they are always one of the list's, the
// chosen user's. Choosing a user copies that user's keys there, so the choice is remembered per
// console and Wake signs in the same user. A profile paired before the list has only the `wake_*`
// keys: they read as one user with no name, and join the list when another user is paired. The
// engine keeps the same list the same way (engine consoles_api.rs); keep the two in step.

import type { PS5Profile, RpUser } from "../state/roster";
import { isValidSessionKey } from "./wakeState";

type Keys = Pick<PS5Profile, "wake_credential" | "wake_regist_key" | "wake_rp_key" | "rp_users">;
export type RpUsersPatch = Pick<PS5Profile, "wake_credential" | "wake_regist_key" | "wake_rp_key" | "rp_users">;

const norm = (s: string | null | undefined) => (s ?? "").trim().toLowerCase();

function usable(u: RpUser): boolean {
  return isValidSessionKey(u.wake_regist_key ?? "") && isValidSessionKey(u.wake_rp_key ?? "");
}

/** The profile's paired users: its list (usable keys, each pairing once), with the chosen keys
 *  first as a user of their own when the list does not hold them. */
export function rpUsersOf(p: Keys | null | undefined): RpUser[] {
  if (!p) return [];
  const out: RpUser[] = [];
  for (const raw of Array.isArray(p.rp_users) ? p.rp_users : []) {
    if (!raw || typeof raw !== "object") continue;
    const u: RpUser = {
      account_id: (raw.account_id ?? "").trim(),
      name: (raw.name ?? "").trim(),
      wake_credential: (raw.wake_credential ?? "").trim() || undefined,
      wake_regist_key: norm(raw.wake_regist_key),
      wake_rp_key: norm(raw.wake_rp_key),
    };
    if (usable(u) && !out.some((o) => o.wake_regist_key === u.wake_regist_key)) out.push(u);
  }
  const r = norm(p.wake_regist_key);
  const k = norm(p.wake_rp_key);
  if (isValidSessionKey(r) && isValidSessionKey(k) && !out.some((u) => u.wake_regist_key === r)) {
    out.unshift({
      account_id: "",
      name: "",
      wake_credential: (p.wake_credential ?? "").trim() || undefined,
      wake_regist_key: r,
      wake_rp_key: k,
    });
  }
  return out;
}

/** The chosen user's regist key (lower case), or null when the console is not paired. */
export function chosenRpUser(p: Keys | null | undefined): string | null {
  const r = norm(p?.wake_regist_key);
  return isValidSessionKey(r) ? r : null;
}

function chooseKeys(u: RpUser | undefined): Pick<PS5Profile, "wake_credential" | "wake_regist_key" | "wake_rp_key"> {
  return {
    wake_regist_key: u?.wake_regist_key || null,
    wake_rp_key: u?.wake_rp_key || null,
    wake_credential: u?.wake_credential || null,
  };
}

/** A new pairing: it replaces the same account's (or the same keys'), else joins the list, and it
 *  is chosen, as a new pairing always was. */
export function withPairing(p: Keys, paired: RpUser): RpUsersPatch {
  const users = rpUsersOf(p);
  const u: RpUser = {
    account_id: (paired.account_id ?? "").trim(),
    name: (paired.name ?? "").trim(),
    wake_credential: (paired.wake_credential ?? "").trim() || undefined,
    wake_regist_key: norm(paired.wake_regist_key),
    wake_rp_key: norm(paired.wake_rp_key),
  };
  const i = users.findIndex(
    (o) => (!!u.account_id && o.account_id === u.account_id) || o.wake_regist_key === u.wake_regist_key,
  );
  if (i >= 0) {
    if (!u.name) u.name = users[i].name;
    users[i] = u;
  } else {
    users.push(u);
  }
  return { rp_users: users, ...chooseKeys(u) };
}

/** Start (and Wake) as this user from now on. Null when it is not one of the profile's. */
export function withChosen(p: Keys, registKey: string): RpUsersPatch | null {
  const users = rpUsersOf(p);
  const u = users.find((o) => o.wake_regist_key === norm(registKey));
  return u ? { rp_users: users, ...chooseKeys(u) } : null;
}

/** Forget one user's pairing. The chosen one gone, the first left is chosen; the last gone, the
 *  console is not paired any more. */
export function withoutRpUser(p: Keys, registKey: string): RpUsersPatch {
  const gone = norm(registKey);
  const users = rpUsersOf(p).filter((u) => u.wake_regist_key !== gone);
  const chosen = chosenRpUser(p);
  const keys = chosen && chosen !== gone ? chooseKeys(users.find((u) => u.wake_regist_key === chosen)) : chooseKeys(users[0]);
  return { rp_users: users.length ? users : null, ...keys };
}
