import { beforeEach, describe, expect, it } from "vitest";

import { askToSignIn, dismissSignInNeeded, isSignInNeeded, useSignInNeededStore } from "./signInNeeded";

describe("waiting at sign-in", () => {
  beforeEach(() => dismissSignInNeeded());

  it("recognises every way the helper and engine say nobody is signed in", () => {
    expect(isSignInNeeded("payload rejected APP_LAUNCH: launch_no_user: rc=0x80940005")).toBe(true);
    expect(isSignInNeeded("launch_sony_error_0x8094000F")).toBe(true);
    expect(isSignInNeeded("vinput_no_user")).toBe(true);
    expect(isSignInNeeded("payload rejected REMOTE_PLAY_ENABLE: rp_enable_no_user")).toBe(true);
    expect(isSignInNeeded("no foreground user on the console — sign in to a profile first")).toBe(true);
  });

  it("leaves every other failure alone", () => {
    expect(isSignInNeeded("launch_sony_error_0x80940033")).toBe(false);
    expect(isSignInNeeded("vinput_failed")).toBe(false);
    expect(isSignInNeeded("")).toBe(false);
    expect(isSignInNeeded(null)).toBe(false);
  });

  it("shows one message for the console, by its host", () => {
    askToSignIn("192.168.1.100:9113");
    expect(useSignInNeededStore.getState().host).toBe("192.168.1.100");
    dismissSignInNeeded();
    expect(useSignInNeededStore.getState().host).toBeNull();
  });
});
