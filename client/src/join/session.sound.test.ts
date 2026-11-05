// The guest's sound (#657): no real audio plays here.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { JoinSession } from "./session";

beforeEach(() => {
  // A page with neither WebCodecs nor MSE: the players need only an element to make.
  const el = () => ({ dataset: {}, append() {}, addEventListener() {}, getContext: () => null, removeAttribute() {}, src: "" });
  vi.stubGlobal("document", { createElement: el });
  vi.stubGlobal("window", {});
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function session(): JoinSession {
  return new JoinSession({ url: "ws://192.0.2.1:19113/api/join/ws", invite: "k", name: "Sam", host: "Alex" });
}

describe("guest sound (#657)", () => {
  it("starts once: the first touch and the button both ask", () => {
    const s = session();
    const start = vi.spyOn(s.picture(), "startSound").mockImplementation(() => {});
    s.startSound();
    s.startSound();
    expect(start).toHaveBeenCalledTimes(1);
    expect(s.snapshot.sound).toBe(true);
  });

  it("says when this browser cannot play the sound", () => {
    const s = session();
    expect(s.snapshot.noSound).toBe(false);
    s.picture().onSoundFail?.();
    expect(s.snapshot.noSound).toBe(true);
  });
});
