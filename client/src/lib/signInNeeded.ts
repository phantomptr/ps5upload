import { create } from "zustand";

import { hostOf } from "./addr";

/**
 * "The PS5 is waiting at sign-in": what a feature that needs a signed-in user says when nobody is
 * (a game launch, the native keyboard & mouse, an avatar for the current user). One message with
 * a Sign in button that runs Wake & sign in (the passcode box when the user has one), shown by
 * SignInNeededHost over the app. Nothing changes while a user is signed in.
 */

/** The helper's / engine's ways of saying nobody is signed in. */
const NO_USER = [
  /launch_no_user/i,
  // Sony's launcher: "no foreground user set".
  /launch_sony_error_0x8094000f/i,
  /vinput_no_user/i,
  /rp_enable_no_user/i,
  /no foreground user/i,
];

export function isSignInNeeded(raw: string | null | undefined): boolean {
  return !!raw && NO_USER.some((re) => re.test(raw));
}

interface SignInNeededState {
  /** The console waiting at sign-in, while the message is up. */
  host: string | null;
  seq: number;
}

export const useSignInNeededStore = create<SignInNeededState>(() => ({ host: null, seq: 0 }));

/** Show the message for this console. */
export function askToSignIn(host: string): void {
  const h = hostOf(host) || host;
  if (!h) return;
  useSignInNeededStore.setState((s) => ({ host: h, seq: s.seq + 1 }));
}

export function dismissSignInNeeded(): void {
  useSignInNeededStore.setState({ host: null });
}
