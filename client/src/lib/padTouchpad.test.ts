import { afterEach, describe, expect, it, vi } from "vitest";

import {
  Follower,
  applyPadTouchForTest,
  padTouchState,
  parseDualSenseReport,
  resetPadTouchpadForTest,
  setPadTouchpad,
  touchFromRaw,
  webHidTouchpadAvailable,
} from "./padTouchpad";

/** A finger's four bytes: up flag + id, then 12-bit x and y. */
function finger(down: boolean, id: number, x: number, y: number): number[] {
  return [(down ? 0 : 0x80) | (id & 0x7f), x & 0xff, ((x >> 8) & 0x0f) | ((y & 0x0f) << 4), y >> 4];
}
const UP = [0x80, 0, 0, 0];

function report(id: number, len: number, off: number, click: boolean, f1: number[], f2: number[]): Uint8Array {
  const b = new Uint8Array(len);
  b[0] = id;
  b[off + 9] = 0x01 | (click ? 0x02 : 0); // PS held too: only bit 1 is the click
  b.set(f1, off + 32);
  b.set(f2, off + 36);
  return b;
}
const usb = (click: boolean, f1: number[], f2: number[]) => report(0x01, 64, 1, click, f1, f2);
const bt = (click: boolean, f1: number[], f2: number[]) => report(0x31, 78, 2, click, f1, f2);

describe("parseDualSenseReport", () => {
  it("reads two fingers from a USB report", () => {
    const r = parseDualSenseReport(usb(false, finger(true, 5, 1919, 1079), finger(true, 6, 0, 1)));
    expect(r).toEqual({
      fingers: [
        { id: 5, x: 1919, y: 1079 },
        { id: 6, x: 0, y: 1 },
      ],
      click: false,
    });
  });

  it("reads a finger and the click from a Bluetooth report", () => {
    const r = parseDualSenseReport(bt(true, finger(true, 9, 960, 540), UP));
    expect(r).toEqual({ fingers: [{ id: 9, x: 960, y: 540 }, null], click: true });
  });

  it("treats a lifted finger as none", () => {
    expect(parseDualSenseReport(usb(false, finger(false, 5, 700, 300), UP))?.fingers).toEqual([null, null]);
  });

  it("ignores reports with no touchpad", () => {
    expect(parseDualSenseReport(new Uint8Array(10).fill(1))).toBeNull(); // short Bluetooth 0x01
    expect(parseDualSenseReport(new Uint8Array(40).fill(0x31))).toBeNull();
    expect(parseDualSenseReport(new Uint8Array(64).fill(2))).toBeNull();
    expect(parseDualSenseReport(new Uint8Array(0))).toBeNull();
  });
});

describe("Follower", () => {
  it("follows the first finger until it lifts, then reports the lift before another", () => {
    const f = new Follower();
    const next = (b: Uint8Array) => f.next(parseDualSenseReport(b)!).touch;
    expect(next(usb(false, finger(true, 1, 100, 200), UP))).toEqual([100, 200]);
    expect(next(usb(false, finger(true, 2, 1500, 900), finger(true, 1, 110, 210)))).toEqual([110, 210]);
    const one = usb(false, finger(true, 2, 1500, 900), finger(false, 1, 110, 210));
    expect(next(one)).toBeNull();
    expect(next(one)).toBeNull();
    expect(next(one)).toEqual([1500, 900]);
    expect(next(usb(false, UP, UP))).toBeNull();
  });
});

describe("the touchpad state", () => {
  afterEach(() => resetPadTouchpadForTest());

  it("maps the DualSense's 1920×1080 onto the console's 1920×942", () => {
    expect(touchFromRaw([0, 0])).toEqual([0, 0]);
    expect(touchFromRaw([1919, 1079])).toEqual([1919, 941]);
    expect(touchFromRaw([960, 540])).toEqual([960, 471]);
    expect(touchFromRaw(null)).toBeNull();
  });

  it("tells the stream on each change, not on repeats", () => {
    const sent = vi.fn();
    setPadTouchpad(false, sent);
    applyPadTouchForTest({ touch: [1919, 1079], click: false });
    expect(padTouchState()).toEqual({ touch: [1919, 941], click: false });
    applyPadTouchForTest({ touch: [1919, 1079], click: false });
    applyPadTouchForTest({ touch: [1919, 1079], click: true });
    applyPadTouchForTest({ touch: null, click: false });
    expect(sent).toHaveBeenCalledTimes(3);
    expect(padTouchState()).toEqual({ touch: null, click: false });
  });

  it("offers WebHID only in a browser that has it", () => {
    expect(webHidTouchpadAvailable()).toBe(false); // jsdom: no navigator.hid
    const nav = navigator as Navigator & { hid?: unknown };
    nav.hid = { getDevices: async () => [], requestDevice: async () => [] };
    try {
      expect(webHidTouchpadAvailable()).toBe(true);
    } finally {
      delete nav.hid;
    }
  });
});
