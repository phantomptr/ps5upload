import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { startKeepScreenVideo, stopKeepScreenVideo } from "./keepScreenVideo";

function fakeVideo(play: () => Promise<void>) {
  return {
    style: {} as { cssText?: string },
    muted: false,
    loop: false,
    playsInline: false,
    src: "",
    play: vi.fn(play),
    pause: vi.fn(),
    remove: vi.fn(),
    setAttribute: vi.fn(),
    removeAttribute: vi.fn(),
  };
}

describe("the video that keeps a phone's screen on over plain http", () => {
  let video: ReturnType<typeof fakeVideo>;
  const touches = new Set<() => void>();

  function stub(play: () => Promise<void>) {
    video = fakeVideo(play);
    vi.stubGlobal("document", {
      createElement: () => video,
      body: { appendChild: vi.fn() },
    });
    vi.stubGlobal("window", {
      addEventListener: (_t: string, fn: () => void) => void touches.add(fn),
      removeEventListener: (_t: string, fn: () => void) => void touches.delete(fn),
    });
  }

  beforeEach(() => touches.clear());
  afterEach(() => {
    stopKeepScreenVideo();
    vi.unstubAllGlobals();
  });

  it("plays muted and inline, so a phone browser allows it", () => {
    stub(() => Promise.resolve());
    startKeepScreenVideo();
    expect(video.muted).toBe(true);
    expect(video.loop).toBe(true);
    expect(video.playsInline).toBe(true);
    expect(video.src).toMatch(/^data:video\/mp4;base64,/);
    expect(video.play).toHaveBeenCalledOnce();
  });

  it("waits for a touch when the browser wants a gesture first", async () => {
    stub(() => Promise.reject(new Error("NotAllowedError")));
    startKeepScreenVideo();
    await vi.waitFor(() => expect(touches.size).toBe(1));
    touches.forEach((f) => f());
    expect(video.play).toHaveBeenCalledTimes(2);
    expect(touches.size).toBe(0);
  });

  it("goes away with the last reason", () => {
    stub(() => Promise.resolve());
    startKeepScreenVideo();
    const v = video;
    stopKeepScreenVideo();
    expect(v.pause).toHaveBeenCalled();
    expect(v.remove).toHaveBeenCalled();
  });
});
