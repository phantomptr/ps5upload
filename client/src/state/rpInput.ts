// The stream's live input (held keys, touch, the on-screen pad, motion) and who holds it back.
// A module of its own so the friend's page (join/session.ts) and the phone's (pad/PadApp.tsx) can
// read it without importing the host's whole Remote Play store (state/remotePlay.ts re-exports both).

import type { MotionReading } from "../lib/motion";
import { IDLE_PAD, type PadState } from "../lib/remotePlayStream";

/** Who holds the game's input back now (Settings, the passcode box, the keyboard box). */
const inputPauses = new Set<string>();

/** While any `owner` has it on, the console gets an idle pad. Each lets go only of its own: the
 *  keyboard box closing must not hand the pad back under open Settings. */
export function pauseRpInput(owner: "settings" | "passcode" | "keyboard", on: boolean): void {
  if (on) inputPauses.add(owner);
  else inputPauses.delete(owner);
}

/** Keys held on the picture and a finger on the touchpad: read by the stream every frame. */
export const rpInput = {
  held: new Set<string>(),
  touch: null as [number, number] | null,
  /** The on-screen controller. */
  touchPad: IDLE_PAD as PadState,
  /** A controller is being mapped: its presses are answers, not play. */
  mapping: false,
  /** Remote Play's settings or a box are open over the stream: keys, controllers and touch are
   *  theirs, and the console gets an idle pad until they close ([`pauseRpInput`]). */
  get paused(): boolean {
    return inputPauses.size > 0;
  },
  /** The phone's latest motion reading, while motion controls are on. */
  motion: null as MotionReading | null,
};
