import { describe, expect, it, vi } from "vitest";

import * as storage from "../lib/safeStorage";
import { isNotifRead, loadNotifReadState, markAllUpTo, setOneRead } from "./ps5NotifRead";

describe("PS5 notification read state", () => {
  it("starts with everything unread", () => {
    expect(isNotifRead(undefined, 1)).toBe(false);
  });

  it("marks one read, and back", () => {
    const s = setOneRead(undefined, 5, true);
    expect(isNotifRead(s, 5)).toBe(true);
    expect(isNotifRead(s, 4)).toBe(false);
    expect(isNotifRead(setOneRead(s, 5, false), 5)).toBe(false);
  });

  it("mark all moves a watermark; later entries arrive unread", () => {
    const s = markAllUpTo(setOneRead(undefined, 2, true), 10);
    expect(isNotifRead(s, 1)).toBe(true);
    expect(isNotifRead(s, 10)).toBe(true);
    expect(isNotifRead(s, 11)).toBe(false);
    // exceptions below the watermark are folded in, so it stays small
    expect(s.read).toEqual([]);
  });

  it("can mark an old one unread again", () => {
    const s = setOneRead(markAllUpTo(undefined, 10), 3, false);
    expect(isNotifRead(s, 3)).toBe(false);
    expect(isNotifRead(s, 4)).toBe(true);
    expect(isNotifRead(setOneRead(s, 3, true), 3)).toBe(true);
  });

  it("never moves the watermark backwards", () => {
    expect(markAllUpTo(markAllUpTo(undefined, 10), 4).upTo).toBe(10);
  });
});

describe("the stored read state", () => {
  it("keeps good entries and drops damaged ones", () => {
    const good = { upTo: 3, read: [1], unread: [] };
    const get = vi.spyOn(storage, "safeGetItem").mockReturnValue(
      JSON.stringify({ a: good, b: { upTo: 1 }, c: { upTo: 1, read: ["x"], unread: [] }, d: null }),
    );
    expect(loadNotifReadState()).toEqual({ a: good });
    get.mockRestore();
  });
});
