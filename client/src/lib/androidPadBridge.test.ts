import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { resetPadForwardForTest, setPadForwardReason } from "./androidPadBridge";

describe("telling the Android app when controllers matter", () => {
  const setActive = vi.fn();

  beforeEach(() => {
    setActive.mockReset();
    resetPadForwardForTest();
    vi.stubGlobal("window", { PS5UploadPad: { setActive } });
  });
  afterEach(() => vi.unstubAllGlobals());

  it("says so on the first reason and takes it back on the last", () => {
    setPadForwardReason("remote-play", true);
    expect(setActive.mock.calls).toEqual([[true]]);
    setPadForwardReason("controller-setup", true);
    setPadForwardReason("remote-play", false);
    expect(setActive.mock.calls).toEqual([[true]]);
    setPadForwardReason("controller-setup", false);
    expect(setActive.mock.calls).toEqual([[true], [false]]);
  });

  it("repeats nothing when a store says the same again", () => {
    setPadForwardReason("remote-play", false);
    setPadForwardReason("remote-play", true);
    setPadForwardReason("remote-play", true);
    expect(setActive.mock.calls).toEqual([[true]]);
  });

  it("does nothing without the bridge, and survives one that throws", () => {
    vi.stubGlobal("window", {});
    expect(() => setPadForwardReason("remote-play", true)).not.toThrow();
    resetPadForwardForTest();
    vi.stubGlobal("window", {
      PS5UploadPad: {
        setActive: () => {
          throw new Error("gone");
        },
      },
    });
    expect(() => setPadForwardReason("remote-play", true)).not.toThrow();
  });
});
