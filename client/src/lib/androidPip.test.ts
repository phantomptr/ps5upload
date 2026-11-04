import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// The remote-play store, without the stream machinery behind it (vitest runs in node).
vi.mock("../state/remotePlay", async () => {
  const { create } = await import("zustand");
  return {
    useRemotePlayStore: create(() => ({
      stream: null as { canvas: unknown } | null,
      phase: null as string | null,
      prefs: { systemPip: true },
    })),
  };
});

import { useRemotePlayStore } from "../state/remotePlay";
import {
  PIP_EVENT,
  androidPipAvailable,
  aspectOf,
  installAndroidPip,
  pipRequest,
  useAndroidPipStore,
} from "./androidPip";

/** A canvas as the stream draws it: only its --rp-shape is read. */
const canvas = (shape?: number) =>
  ({
    style: { getPropertyValue: (k: string) => (k === "--rp-shape" && shape !== undefined ? String(shape) : "") },
  }) as unknown as HTMLCanvasElement;

const setRp = (s: Record<string, unknown>) =>
  (useRemotePlayStore as unknown as { setState: (p: Record<string, unknown>) => void }).setState(s);

describe("aspectOf", () => {
  it("is 16:9 for the usual picture and when the shape is unknown", () => {
    expect(aspectOf(16 / 9)).toEqual([16, 9]);
    expect(aspectOf(NaN)).toEqual([16, 9]);
    expect(aspectOf(0)).toEqual([16, 9]);
  });
  it("keeps other shapes, within what Android allows", () => {
    expect(aspectOf(21 / 9)).toEqual([2333, 1000]);
    expect(aspectOf(4)).toEqual([2390, 1000]);
    expect(aspectOf(0.1)).toEqual([418, 1000]);
  });
});

describe("pipRequest", () => {
  it("asks for nothing with the picture off on this device", () => {
    const stream = { canvas: { width: 1920, height: 1080, style: { getPropertyValue: () => "" } } } as never;
    expect(pipRequest({ stream, phase: "playing", prefs: { systemPip: true }, pictureHere: false }).active).toBe(false);
  });
  it("asks for picture in picture only while a stream plays and the setting is on", () => {
    const stream = { canvas: canvas(16 / 9) };
    expect(pipRequest({ stream, phase: "playing", prefs: { systemPip: true } })).toEqual({ active: true, w: 16, h: 9 });
    expect(pipRequest({ stream, phase: "connecting", prefs: { systemPip: true } }).active).toBe(false);
    expect(pipRequest({ stream: null, phase: "playing", prefs: { systemPip: true } }).active).toBe(false);
    expect(pipRequest({ stream, phase: "playing", prefs: { systemPip: false } }).active).toBe(false);
  });
});

describe("installAndroidPip", () => {
  let calls: Array<[boolean, number, number]>;
  let inPip = false;
  beforeEach(() => {
    calls = [];
    inPip = false;
    const target = new EventTarget();
    vi.stubGlobal("window", {
      addEventListener: target.addEventListener.bind(target),
      removeEventListener: target.removeEventListener.bind(target),
      dispatchEvent: target.dispatchEvent.bind(target),
      PS5UploadPip: {
        setActive: (a: boolean, w: number, h: number) => calls.push([a, w, h]),
        inPip: () => inPip,
        supported: () => true,
      },
    });
    setRp({ stream: null, phase: null, prefs: { systemPip: true } });
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("does nothing without the Android app's bridge", () => {
    vi.stubGlobal("window", { addEventListener: () => {}, removeEventListener: () => {} });
    expect(androidPipAvailable()).toBe(false);
    const undo = installAndroidPip();
    setRp({ stream: { canvas: canvas(16 / 9) }, phase: "playing" });
    undo();
    expect(calls).toEqual([]);
  });

  it("tells the activity when a stream starts and stops playing, once per change", () => {
    expect(androidPipAvailable()).toBe(true);
    const undo = installAndroidPip();
    expect(calls).toEqual([[false, 16, 9]]);
    const stream = { canvas: canvas(16 / 9) };
    setRp({ stream, phase: "connecting" });
    setRp({ stream, phase: "playing" });
    setRp({ stream, phase: "playing", stats: {} });
    expect(calls).toEqual([
      [false, 16, 9],
      [true, 16, 9],
    ]);
    setRp({ stream: null, phase: "ended" });
    expect(calls[calls.length - 1]).toEqual([false, 16, 9]);
    undo();
  });

  it("follows the activity in and out of picture in picture", () => {
    const undo = installAndroidPip();
    expect(useAndroidPipStore.getState().inPip).toBe(false);
    window.dispatchEvent(new CustomEvent(PIP_EVENT, { detail: { inPip: true } }));
    expect(useAndroidPipStore.getState().inPip).toBe(true);
    window.dispatchEvent(new CustomEvent(PIP_EVENT, { detail: { inPip: false } }));
    expect(useAndroidPipStore.getState().inPip).toBe(false);
    undo();
    // Uninstalled: the activity is told nothing plays, and events no longer count.
    expect(calls[calls.length - 1]).toEqual([false, 16, 9]);
    window.dispatchEvent(new CustomEvent(PIP_EVENT, { detail: { inPip: true } }));
    expect(useAndroidPipStore.getState().inPip).toBe(false);
  });

  it("picks up a page loaded while already in picture in picture", () => {
    inPip = true;
    const undo = installAndroidPip();
    expect(useAndroidPipStore.getState().inPip).toBe(true);
    undo();
  });
});
