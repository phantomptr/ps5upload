// TV mode (#976): the app as a TV app for playing, a console as one large card with a big Play.
// It turns on by itself on a TV (the Android app on Android TV or Google TV, lib/tvNav), and
// Settings › App has a switch for any device. "Full app" leaves it for now; the app opens in TV
// mode again next time, and a bar in the full app goes back to it.
//
// Kept on this device only (not in lib/sharedPrefs): a TV's choice is not a phone's.

import { useSyncExternalStore } from "react";
import { create } from "zustand";

import { safeGetItem, safeRemoveItem, safeSetItem } from "../lib/safeStorage";
import { isTvDevice } from "../lib/tvNav";

const KEY = "ps5upload.tv_mode.v1";

/** The screens TV mode shows in its own plain frame (no sidebar, no tab bar). Anything else is
 *  the full app's, reached from them (a link in Play's setup, say), and shows as it always does. */
export const TV_ROUTES: readonly string[] = ["/tv", "/play", "/play/setup"];

/** Whether `pathname` is one of TV mode's own screens. */
export function isTvRoute(pathname: string): boolean {
  return TV_ROUTES.includes(pathname.replace(/\/+$/, "") || "/");
}

/** The saved choice: on, off, or null for "on by itself on a TV". */
export function readTvPref(): boolean | null {
  const v = safeGetItem(KEY);
  return v === "on" ? true : v === "off" ? false : null;
}

/** Whether this device wants TV mode: the choice made in Settings, else whether it is a TV. */
export function tvModeWanted(pref: boolean | null, tv: boolean): boolean {
  return pref ?? tv;
}

/** Whether TV mode is on now: wanted, and "Full app" not chosen since the app opened. */
export function tvModeOn(pref: boolean | null, tv: boolean, fullApp: boolean): boolean {
  return tvModeWanted(pref, tv) && !fullApp;
}

interface TvModeState {
  /** Settings' switch: on, off, or null (on by itself on a TV). */
  pref: boolean | null;
  /** "Full app" was chosen: TV mode rests until "Back to TV mode" or the next start. */
  fullApp: boolean;
  setPref: (on: boolean | null) => void;
  setFullApp: (on: boolean) => void;
}

export const useTvModeStore = create<TvModeState>((set) => ({
  pref: readTvPref(),
  fullApp: false,
  setPref: (on) => {
    if (on === null) safeRemoveItem(KEY);
    else safeSetItem(KEY, on ? "on" : "off");
    // Turned on in Settings: on now, not after the next start.
    set({ pref: on, fullApp: false });
  },
  setFullApp: (on) => set({ fullApp: on }),
}));

// Whether this is a TV does not change while the app runs (MainActivity's bridge), so it is
// read once per render without a subscription.
const noSubscribe = () => () => {};
const tvNow = () => isTvDevice();

/** Whether this device is a TV (stable for the app's life). */
export function useIsTv(): boolean {
  return useSyncExternalStore(noSubscribe, tvNow, () => false);
}

/** Whether TV mode is on now. */
export function useTvMode(): boolean {
  const tv = useIsTv();
  return useTvModeStore((s) => tvModeOn(s.pref, tv, s.fullApp));
}

/** Whether this device wants TV mode (on now, or rested with "Full app"). */
export function useTvModeWanted(): boolean {
  const tv = useIsTv();
  return useTvModeStore((s) => tvModeWanted(s.pref, tv));
}

/** Outside React (the landing redirect's first render reads the hook; this is for plain code). */
export function tvModeOnNow(): boolean {
  const s = useTvModeStore.getState();
  return tvModeOn(s.pref, isTvDevice(), s.fullApp);
}
