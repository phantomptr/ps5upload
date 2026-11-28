import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { memoryStorage } from "../test-utils/storage";
import { isTvRoute, readTvPref, tvModeOn, tvModeOnNow, tvModeWanted, useTvModeStore } from "./tvMode";

const KEY = "ps5upload.tv_mode.v1";

/** `window` as the app sees it: storage, and MainActivity's bridge when `tv` says so. */
function device(tv: boolean | null, seed: Record<string, string> = {}): Storage {
  const localStorage = memoryStorage(seed);
  vi.stubGlobal("window", {
    localStorage,
    ...(tv === null ? {} : { PS5UploadTv: { isTv: () => tv } }),
  });
  return localStorage;
}

describe("TV mode: which devices get it (#976)", () => {
  beforeEach(() => useTvModeStore.setState({ pref: null, fullApp: false }));
  afterEach(() => vi.unstubAllGlobals());

  it("is on by itself on a TV and off elsewhere, until the switch says otherwise", () => {
    expect(tvModeWanted(null, true)).toBe(true);
    expect(tvModeWanted(null, false)).toBe(false);
    expect(tvModeWanted(false, true)).toBe(false);
    expect(tvModeWanted(true, false)).toBe(true);
  });

  it("rests while Full app is chosen, and stays wanted", () => {
    expect(tvModeOn(null, true, true)).toBe(false);
    expect(tvModeWanted(null, true)).toBe(true);
    expect(tvModeOn(true, false, false)).toBe(true);
  });

  it("reads the device: the Android app on a TV, a phone's app, a browser", () => {
    device(true);
    expect(tvModeOnNow()).toBe(true);
    device(false);
    expect(tvModeOnNow()).toBe(false);
    device(null);
    expect(tvModeOnNow()).toBe(false);
  });

  it("keeps the switch on this device, and clears it back to 'by itself'", () => {
    const ls = device(false);
    useTvModeStore.getState().setPref(true);
    expect(ls.getItem(KEY)).toBe("on");
    expect(readTvPref()).toBe(true);
    expect(tvModeOnNow()).toBe(true);
    useTvModeStore.getState().setPref(false);
    expect(ls.getItem(KEY)).toBe("off");
    expect(tvModeOnNow()).toBe(false);
    useTvModeStore.getState().setPref(null);
    expect(ls.getItem(KEY)).toBeNull();
    expect(readTvPref()).toBeNull();
  });

  it("an unknown saved value is 'by itself'", () => {
    device(true, { [KEY]: "maybe" });
    expect(readTvPref()).toBeNull();
  });

  it("Full app rests it on a TV; Back to TV mode and turning it on bring it back", () => {
    device(true);
    useTvModeStore.getState().setFullApp(true);
    expect(tvModeOnNow()).toBe(false);
    useTvModeStore.getState().setFullApp(false);
    expect(tvModeOnNow()).toBe(true);
    useTvModeStore.getState().setFullApp(true);
    useTvModeStore.getState().setPref(true);
    expect(useTvModeStore.getState().fullApp).toBe(false);
    expect(tvModeOnNow()).toBe(true);
  });
});

describe("TV mode's own screens", () => {
  it("are its home, Play and Play's setup; everything else is the full app's", () => {
    expect(isTvRoute("/tv")).toBe(true);
    expect(isTvRoute("/tv/")).toBe(true);
    expect(isTvRoute("/play")).toBe(true);
    expect(isTvRoute("/play/setup")).toBe(true);
    expect(isTvRoute("/play/pad")).toBe(false);
    expect(isTvRoute("/home")).toBe(false);
    expect(isTvRoute("/files")).toBe(false);
    expect(isTvRoute("/settings")).toBe(false);
  });
});
