import { create } from "zustand";

import {
  answerWakePasscode,
  cancelWakePasscode,
  wakeAndSignIn,
  WakeLoginError,
  type WakeLoginResult,
} from "../api/ps5";
import { audit } from "../state/auditLog";
import { pushNotification } from "../state/notifications";
import { withConsolePrefix } from "../state/roster";
import { trStatic } from "./trStatic";

/**
 * Wake & sign in, asking for the PS5 user's login passcode when the console wants it (#608).
 *
 * The engine holds the sign-in open while the user types (see engine wake_login_api.rs). The ask
 * lives here, not in a component, so the box stays up when the screen that pressed Wake changes
 * (the rest-mode notice goes away as soon as the console is awake). One box at a time, over the
 * app (WakePasscodeHost); a second console asking waits in line behind the first. The passcode is sent to the engine and never kept.
 */

export interface WakePasscodeAsk {
  host: string;
  /** The engine's token for the held sign-in. */
  id: string;
  /** The last passcode was wrong. */
  retry: boolean;
  /** Changes with every ask, so the box starts empty each time. */
  seq: number;
  /** An answer is on its way. */
  sending: boolean;
}

interface WakeSignInState {
  /** The box shown now: the first of `asks`. */
  ask: WakePasscodeAsk | null;
  /** Every console waiting for its passcode, one box at a time in the order they asked. */
  asks: WakePasscodeAsk[];
  /** Hosts with a Wake & sign in running (including while the box is up). */
  busy: Record<string, boolean>;
  /** The last Wake & sign in error per host. */
  error: Record<string, string | null>;
}

export const useWakeSignInStore = create<WakeSignInState>(() => ({
  ask: null,
  asks: [],
  busy: {},
  error: {},
}));

let seq = 0;

/** Puts `asks` in the store, the first one shown. */
function setAsks(asks: WakePasscodeAsk[]) {
  useWakeSignInStore.setState({ asks, ask: asks[0] ?? null });
}

/** `host`'s ask is now `ask` (a new one goes last in line; a retry keeps its place), or gone. */
function putAsk(host: string, ask: WakePasscodeAsk | null) {
  const asks = useWakeSignInStore.getState().asks;
  const at = asks.findIndex((a) => a.host === host);
  if (!ask) {
    if (at >= 0) setAsks(asks.filter((_, i) => i !== at));
    return;
  }
  setAsks(at >= 0 ? asks.map((a, i) => (i === at ? ask : a)) : [...asks, ask]);
}

function setBusy(host: string, busy: boolean) {
  useWakeSignInStore.setState((s) => ({ busy: { ...s.busy, [host]: busy } }));
}

function setError(host: string, error: string | null) {
  useWakeSignInStore.setState((s) => ({ error: { ...s.error, [host]: error } }));
}

/** What the user reads when a Wake & sign in fails. */
export function wakeErrorText(e: unknown): string {
  const code = e instanceof WakeLoginError ? e.code : undefined;
  switch (code) {
    case "passcode_too_many":
      return trStatic("rp_err_passcode_too_many", "Too many wrong passcodes; enter it on the PS5.");
    case "passcode_timeout":
    case "passcode_expired":
      return trStatic(
        "wake_passcode_timeout",
        "The PS5 stopped waiting for the passcode. Press Wake & sign in again.",
      );
  }
  return e instanceof Error ? e.message : String(e);
}

/** Act on one step of the sign-in. Returns true when it is over. */
function settle(host: string, r: WakeLoginResult): boolean {
  if (r.state === "passcode") {
    putAsk(host, { host, id: r.id, retry: r.retry, seq: ++seq, sending: false });
    return false;
  }
  putAsk(host, null);
  if (r.state === "signed_in") {
    pushNotification("success", trStatic("power_wake_signedin", "Woken and signed in"), {
      body: trStatic("power_wake_signedin_body", "The console is on and signed in to your user."),
    });
    audit("system_wake", withConsolePrefix(host, "woken and signed in"));
  } else {
    // Not asked for here (only automatic wakes skip the passcode), but say what happened.
    pushNotification("info", trStatic("wake_at_sign_in", "The PS5 is on, at its sign-in screen."));
    audit("system_wake", withConsolePrefix(host, "woken, waiting at sign-in"));
  }
  return true;
}

function failed(host: string, e: unknown) {
  putAsk(host, null);
  if (e instanceof WakeLoginError && e.code === "passcode_cancelled") {
    // The user chose this: not an error.
    pushNotification(
      "info",
      trStatic("wake_passcode_cancelled", "The PS5 stays at its sign-in screen."),
    );
    audit("system_wake", withConsolePrefix(host, "woken, sign-in cancelled at the passcode"));
    return;
  }
  setError(host, wakeErrorText(e));
}

async function run(host: string, step: Promise<WakeLoginResult>) {
  try {
    if (!settle(host, await step)) return;
  } catch (e) {
    failed(host, e);
  }
  // Every way out but a box still up for this console ends its busy.
  setBusy(host, false);
}

/** Wake the console and sign its user in, asking for the passcode if the PS5 wants one.
 *  Resolves when the sign-in ended or the box is up. */
export async function wakeAndSignInAsking(
  host: string,
  creds: { credential: string; registKey: string; rpKey: string },
): Promise<void> {
  if (useWakeSignInStore.getState().busy[host]) return;
  setBusy(host, true);
  setError(host, null);
  await run(
    host,
    wakeAndSignIn(host, creds.credential, creds.registKey, creds.rpKey, { askPasscode: true }),
  );
}

/** The passcode typed into the box. */
export async function submitWakePasscode(digits: string): Promise<void> {
  const ask = useWakeSignInStore.getState().ask;
  if (!ask || ask.sending) return;
  putAsk(ask.host, { ...ask, sending: true });
  await run(ask.host, answerWakePasscode(ask.id, digits));
}

/** Cancel: the console stays at its sign-in screen. */
export async function cancelWakeSignIn(): Promise<void> {
  const ask = useWakeSignInStore.getState().ask;
  if (!ask) return;
  // The next console's box (if any) shows now.
  putAsk(ask.host, null);
  await run(ask.host, cancelWakePasscode(ask.id));
}

/** Test seam. */
export function resetWakeSignIn(): void {
  useWakeSignInStore.setState({ ask: null, asks: [], busy: {}, error: {} });
}
