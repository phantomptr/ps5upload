// A friend's game this app is in, for the app shell's banner. Apart from state/joinSession, which
// brings the whole player with it.

import { create } from "zustand";

interface JoinPresence {
  /** The friend's name; null when no game is joined. */
  host: string | null;
  leave: (() => void) | null;
  /** The game's own view is on show: the keyboard plays only then. */
  onScreen: boolean;
}

export const useJoinPresence = create<JoinPresence>(() => ({ host: null, leave: null, onScreen: false }));
