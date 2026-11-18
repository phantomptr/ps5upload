// Setting up Play (10.0.0): the steps from a fresh console to a picture in the app, and which of
// them are done, from what the app can read. Pure, so the checklist, Home's Play card and the
// tests agree on every state.

import type { NpState, RemotePlayReadiness } from "../../../api/ps5";

export type SetupKey =
  | "connect"
  | "user"
  | "account"
  | "signin"
  | "service"
  | "rest"
  | "pair"
  | "blockers"
  | "play";

/** ok: done. todo: the next thing to do here. wait: can't be checked or done until an earlier
 *  step is. manual: only the console can show it; the user ticks it. optional: not needed to
 *  play (only the console can show it; the user may tick it). */
export type SetupState = "ok" | "todo" | "wait" | "manual" | "optional";

export const SETUP_ORDER: readonly SetupKey[] = [
  "connect",
  "user",
  "account",
  "signin",
  "service",
  "rest",
  "pair",
  "blockers",
  "play",
];

export interface SetupInput {
  /** The engine and the PS5's helper both answer. */
  connected: boolean;
  /** The console's Remote Play facts, or null while unread. */
  readiness: RemotePlayReadiness | null;
  /** The signed-in user's PSN state (np-fake-signin's result), or null when unread. */
  np: NpState | null;
  /** This app holds the console's pairing keys. */
  paired: boolean;
  /** The user ticked the rest-mode settings for this console. */
  restOk: boolean;
  /** A picture has come through from this console. */
  played: boolean;
  /** Payloads on the console that block Remote Play's picture (lib/rpBlockers.ts), or null
   *  while unread. */
  blockers?: readonly string[] | null;
}

export function setupStates(i: SetupInput): Record<SetupKey, SetupState> {
  const r = i.connected ? i.readiness : null;
  // A step is judged once the one before it is done; until then it waits (its facts may read
  // as done, but a user signed in on no activated account is not a step done).
  const after = (prev: SetupState, ok: boolean | undefined): SetupState =>
    prev !== "ok" || ok === undefined ? "wait" : ok ? "ok" : "todo";

  const connect: SetupState = i.connected ? "ok" : "todo";
  const user = after(connect, r ? userSignedIn(r) : undefined);
  const account = after(user, r ? r.account_id_raw !== 0 && r.account_type === "np" : undefined);
  // Signed in: np-fake-signin (or PSN) left an online id and its auth file; a console whose
  // Remote Play service is already on got past this step some other way.
  const npSigned = !!(i.np && i.np.online_id.trim() && i.np.auth_dat);
  const signin = after(account, r ? npSigned || r.service_enabled !== 0 : undefined);
  const userOn = r && r.has_per_user !== 0 ? r.user_enabled !== 0 : true;
  const service = after(signin, r ? r.service_enabled !== 0 && userOn : undefined);
  // Only for starting Play while the PS5 is in rest mode: someone who turns the console on
  // themselves never needs it, so it is never the step in the way.
  const rest: SetupState = i.restOk ? "ok" : "optional";
  // Pairing needs the service on; keys kept from before still count when the console is away.
  const pair: SetupState = i.paired ? "ok" : service === "ok" ? "todo" : "wait";
  // No payload in the way (Hush off or not loaded): read on its own, whatever the steps above.
  const blockers: SetupState = !i.connected || i.blockers == null ? "wait" : i.blockers.length ? "todo" : "ok";
  const play: SetupState = i.played ? "ok" : pair === "ok" ? "todo" : "wait";
  return { connect, user, account, signin, service, rest, pair, blockers, play };
}

/** A user is signed in on the console, with an account ID or not. `account_uid` is only the
 *  user pairing would use, one with an account ID: an offline user without one yet is signed
 *  in all the same, and giving it an ID is the next step (#864). `login_count` comes from a
 *  newer helper; with an older one, the foreground user says so. */
export function userSignedIn(r: RemotePlayReadiness): boolean {
  return r.account_uid > 0 || r.foreground_uid > 0 || (r.login_count ?? 0) > 0;
}

/** Why the sign-in step is not done, from the helper's answer: the console said nobody is
 *  signed in, or it answered the question with an error code (shown as hex). */
export type UserWait = { why: "none" } | { why: "error"; code: string };

export function userWait(r: RemotePlayReadiness | null): UserWait | null {
  if (!r || userSignedIn(r)) return null;
  // The signed-in list is the answer that counts; the foreground one only when there is no list.
  const rc = r.login_rc !== undefined ? r.login_rc : (r.fg_rc ?? 0);
  return rc ? { why: "error", code: hexCode(rc) } : { why: "none" };
}

/** The helper's raw answer about the console's user, for the screen and a bug report: never the
 *  account ID. */
export function userFacts(r: RemotePlayReadiness): string {
  const parts = [
    `foreground_uid=${hexCode(r.foreground_uid)}`,
    `account_uid=${hexCode(r.account_uid)}`,
    `account_via=${r.account_via || "?"}`,
    `user_slot=${r.user_slot}`,
  ];
  if (r.login_count !== undefined) parts.push(`login_count=${r.login_count}`);
  if (r.signed_in_slot !== undefined) parts.push(`signed_in_slot=${r.signed_in_slot}`);
  if (r.fg_rc !== undefined) parts.push(`fg_rc=${hexCode(r.fg_rc)}`);
  if (r.login_rc !== undefined) parts.push(`login_rc=${hexCode(r.login_rc)}`);
  return parts.join(" ");
}

function hexCode(n: number): string {
  return `0x${(n >>> 0).toString(16).padStart(8, "0")}`;
}

/** The step to open: the first one that is not done (a manual one counts as not done, an
 *  optional one is skipped). */
export function nextStep(s: Record<SetupKey, SetupState>): SetupKey | null {
  return SETUP_ORDER.find((k) => s[k] !== "ok" && s[k] !== "optional") ?? null;
}

/** Done out of the steps that count: an optional step not taken is left out. */
export function setupProgress(s: Record<SetupKey, SetupState>): { done: number; total: number } {
  const counted = SETUP_ORDER.filter((k) => s[k] !== "optional");
  return { done: counted.filter((k) => s[k] === "ok").length, total: counted.length };
}

/** Ready to play: paired (the rest of the list is how you get there, and keys kept from before
 *  still work). */
export function playReady(s: Record<SetupKey, SetupState>): boolean {
  return s.pair === "ok";
}
