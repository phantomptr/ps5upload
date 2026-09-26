import { beforeEach, describe, expect, it } from "vitest";
import {
  isDirectTransportBlocked,
  noteDirectTransportBlocked,
  resetImageTransport,
} from "./imageTransport";

describe("imageTransport", () => {
  beforeEach(() => resetImageTransport());

  it("starts optimistic, so a healthy webview never pays for the fallback", () => {
    // The direct URL is the cheaper transport — no base64 inflation, and the
    // browser caches it. A session must not give it up until it has actual
    // evidence against it.
    expect(isDirectTransportBlocked()).toBe(false);
  });

  it("latches once the direct transport is shown to be blocked", () => {
    noteDirectTransportBlocked();
    expect(isDirectTransportBlocked()).toBe(true);
  });

  it("stays latched, so the verdict is reached once per session and not per image", () => {
    // This is the whole point: before it existed, every cover rediscovered
    // the blocked webview by failing twice and waiting out a 1.2-2.4s
    // stagger, on every mount of every screen.
    noteDirectTransportBlocked();
    for (let i = 0; i < 50; i++) {
      expect(isDirectTransportBlocked()).toBe(true);
    }
  });

  it("is reset only explicitly, so tests cannot leak the verdict into each other", () => {
    noteDirectTransportBlocked();
    resetImageTransport();
    expect(isDirectTransportBlocked()).toBe(false);
  });
});
