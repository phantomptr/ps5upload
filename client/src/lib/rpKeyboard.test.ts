import { afterEach, describe, expect, it, vi } from "vitest";

import {
  KEYBOARD_DEBOUNCE_MS,
  KeyboardTyper,
  nextKeyboard,
} from "./rpKeyboard";

describe("rp keyboard state", () => {
  it("opens with the console's text, follows its answers, and closes", () => {
    const a = nextKeyboard(null, { open: true, text: "héllo 日本" });
    expect(a?.text).toBe("héllo 日本");
    const b = nextKeyboard(a, { text: "abc" });
    expect(b).toEqual({ text: "abc", seq: a!.seq });
    expect(nextKeyboard(b, { open: false })).toBeNull();
  });

  it("each opening is a new one, even with the same text", () => {
    const a = nextKeyboard(null, { open: true, text: "" });
    const b = nextKeyboard(a, { open: true, text: "" });
    expect(b!.seq).toBeGreaterThan(a!.seq);
    expect(a!.text).toBe("");
  });

  it("a text without an opening, or junk, changes nothing", () => {
    expect(nextKeyboard(null, { text: "x" })).toBeNull();
    expect(nextKeyboard(null, null as never)).toBeNull();
    const a = nextKeyboard(null, { open: true });
    expect(a?.text).toBe("");
  });
});

describe("rp keyboard typer", () => {
  afterEach(() => vi.useRealTimers());

  it("sends the latest text once typing pauses", () => {
    vi.useFakeTimers();
    const sent: string[] = [];
    const t = new KeyboardTyper((s) => sent.push(s));
    t.type("a");
    t.type("ab");
    vi.advanceTimersByTime(KEYBOARD_DEBOUNCE_MS - 1);
    expect(sent).toEqual([]);
    t.type("abc");
    vi.advanceTimersByTime(KEYBOARD_DEBOUNCE_MS);
    expect(sent).toEqual(["abc"]);
    t.type("abcd😀");
    vi.advanceTimersByTime(KEYBOARD_DEBOUNCE_MS);
    expect(sent).toEqual(["abc", "abcd😀"]);
  });

  it("flush sends at once, and never the same text twice", () => {
    vi.useFakeTimers();
    const sent: string[] = [];
    const t = new KeyboardTyper((s) => sent.push(s), "start");
    t.type("start");
    t.flush();
    expect(sent).toEqual([]);
    t.type("next");
    t.flush();
    expect(sent).toEqual(["next"]);
    vi.advanceTimersByTime(KEYBOARD_DEBOUNCE_MS * 2);
    expect(sent).toEqual(["next"]);
  });

  it("cancel drops what is waiting", () => {
    vi.useFakeTimers();
    const sent: string[] = [];
    const t = new KeyboardTyper((s) => sent.push(s));
    t.type("x");
    t.cancel();
    vi.advanceTimersByTime(KEYBOARD_DEBOUNCE_MS * 2);
    t.flush();
    expect(sent).toEqual([]);
  });
});
