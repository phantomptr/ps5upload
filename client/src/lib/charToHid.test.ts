import { describe, expect, it } from "vitest";

import { charToKey, namedKey, textDiff, typeText } from "./charToHid";
import { HID_USAGE } from "./kbmInput";

describe("charToKey (US layout)", () => {
  it("maps every printable ASCII character to a key with an HID usage", () => {
    for (let c = 0x20; c < 0x7f; c++) {
      const k = charToKey(String.fromCharCode(c));
      expect(k, String.fromCharCode(c)).not.toBeNull();
      expect(HID_USAGE[k!.code], k!.code).toBeGreaterThan(0);
    }
  });

  it("knows which need Shift", () => {
    expect(charToKey("a")).toEqual({ code: "KeyA", shift: false });
    expect(charToKey("A")).toEqual({ code: "KeyA", shift: true });
    expect(charToKey("1")).toEqual({ code: "Digit1", shift: false });
    expect(charToKey("!")).toEqual({ code: "Digit1", shift: true });
    expect(charToKey("?")).toEqual({ code: "Slash", shift: true });
    expect(charToKey(" ")).toEqual({ code: "Space", shift: false });
    expect(charToKey("\n")).toEqual({ code: "Enter", shift: false });
  });

  it("has no key for other letters", () => {
    expect(charToKey("é")).toBeNull();
    expect(charToKey("日")).toBeNull();
    expect(charToKey("ab")).toBeNull();
  });

  it("named keys: Backspace, Enter, arrows", () => {
    expect(namedKey("Backspace")).toBe("Backspace");
    expect(namedKey("ArrowLeft")).toBe("ArrowLeft");
    expect(namedKey("Unidentified")).toBeNull();
  });
});

describe("typeText", () => {
  it("presses each key, with Shift around capitals, and returns what it could not type", () => {
    const log: string[] = [];
    const skipped = typeText({ key: (c, d) => (log.push(`${c}${d ? "↓" : "↑"}`), true) }, "Hi é!");
    expect(log).toEqual([
      "ShiftLeft↓",
      "KeyH↓",
      "KeyH↑",
      "ShiftLeft↑",
      "KeyI↓",
      "KeyI↑",
      "Space↓",
      "Space↑",
      "ShiftLeft↓",
      "Digit1↓",
      "Digit1↑",
      "ShiftLeft↑",
    ]);
    expect(skipped).toBe("é");
  });
});

describe("textDiff", () => {
  it("finds deletions at the end and what was added", () => {
    expect(textDiff("  ", "  a")).toEqual({ deleted: 0, inserted: "a" });
    expect(textDiff("  hel", "  hello")).toEqual({ deleted: 0, inserted: "lo" });
    expect(textDiff("  teh", "  the")).toEqual({ deleted: 2, inserted: "he" }); // autocorrect
    expect(textDiff("  ", " ")).toEqual({ deleted: 1, inserted: "" });
  });
});
