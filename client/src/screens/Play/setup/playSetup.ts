// Setting up Play (10.0.0): the steps from a fresh console to a picture in the app, and which of
// them are done, from what the app can read. Pure, so the checklist, Home's Play card and the
// tests agree on every state.

import type { NpState, RemotePlayReadiness } from "../../../api/ps5";

export type SetupKey = "connect" | "user" | "account" | "signin" | "service" | "rest" | "pair" | "play";

/** ok: done. todo: the next thing to do here. wait: can't be checked or done until an earlier
 *  step is. manual: only the console can show it; the user ticks it. */
export type SetupState = "ok" | "todo" | "wait" | "manual";

export const SETUP_ORDER: readonly SetupKey[] = [
  "connect",
  "user",
  "account",
  "signin",
  "service",
  "rest",
  "pair",
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
}

export function setupStates(i: SetupInput): Record<SetupKey, SetupState> {
  const r = i.connected ? i.readiness : null;
  // A step is judged once the one before it is done; until then it waits (its facts may read
  // as done, but a user signed in on no activated account is not a step done).
  const after = (prev: SetupState, ok: boolean | undefined): SetupState =>
    prev !== "ok" || ok === undefined ? "wait" : ok ? "ok" : "todo";

  const connect: SetupState = i.connected ? "ok" : "todo";
  const user = after(connect, r ? r.account_uid > 0 : undefined);
  const account = after(user, r ? r.account_id_raw !== 0 && r.account_type === "np" : undefined);
  // Signed in: np-fake-signin (or PSN) left an online id and its auth file; a console whose
  // Remote Play service is already on got past this step some other way.
  const npSigned = !!(i.np && i.np.online_id.trim() && i.np.auth_dat);
  const signin = after(account, r ? npSigned || r.service_enabled !== 0 : undefined);
  const userOn = r && r.has_per_user !== 0 ? r.user_enabled !== 0 : true;
  const service = after(signin, r ? r.service_enabled !== 0 && userOn : undefined);
  const rest: SetupState = i.restOk ? "ok" : "manual";
  // Pairing needs the service on; keys kept from before still count when the console is away.
  const pair: SetupState = i.paired ? "ok" : service === "ok" ? "todo" : "wait";
  const play: SetupState = i.played ? "ok" : pair === "ok" ? "todo" : "wait";
  return { connect, user, account, signin, service, rest, pair, play };
}

/** The step to open: the first one that is not done (a manual one counts as not done). */
export function nextStep(s: Record<SetupKey, SetupState>): SetupKey | null {
  return SETUP_ORDER.find((k) => s[k] !== "ok") ?? null;
}

export function setupProgress(s: Record<SetupKey, SetupState>): { done: number; total: number } {
  return { done: SETUP_ORDER.filter((k) => s[k] === "ok").length, total: SETUP_ORDER.length };
}

/** Ready to play: paired (the rest of the list is how you get there, and keys kept from before
 *  still work). */
export function playReady(s: Record<SetupKey, SetupState>): boolean {
  return s.pair === "ok";
}
