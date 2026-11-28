import { describe, expect, it } from "vitest";

import {
  displayPairCode,
  normalizePairCode,
  pairCodeReady,
  pairResultText,
  readPairing,
  readPairResult,
} from "./playAppPairingState";

const tr = (_key: string, _vars: Record<string, string | number> | undefined, fallback: string) => fallback;

describe("PS5Upload Play pairing", () => {
  it("takes a code typed any way, with only the TV's letters", () => {
    expect(normalizePairCode("k7m-2qx")).toBe("K7M2QX");
    expect(normalizePairCode(" K7M 2QX ")).toBe("K7M2QX");
    // No look-alikes (I, L, O, 0, 1), and 6 at most.
    expect(normalizePairCode("IL0O1ABCDEFGH")).toBe("ABCDEF");
    expect(pairCodeReady("k7m-2qx")).toBe(true);
    expect(pairCodeReady("K7M2Q")).toBe(false);
    expect(displayPairCode("K7M2QX")).toBe("K7M-2QX");
    expect(displayPairCode("K7")).toBe("K7");
  });

  it("reads the paired list and what waits, and nothing from an older engine", () => {
    const s = readPairing({
      apps: [
        { id: "ab12cd34", console: "192.168.1.50", name: "Living room", paired_unix: 7, last_seen_unix: 9, connected: true },
        { id: "", console: "x" },
        { id: "ee", console: "192.168.1.51", name: null, paired_unix: 8, last_seen_unix: null, connected: false },
      ],
      waiting: [{ console: "192.168.1.60", name: "Den", code_display: "K7M-2QX", expires_in_s: 200 }, {}],
      moves: [{ id: "ab12cd34", name: "Living room", addr_old: "192.168.1.50", addr_new: "192.168.1.70", expires_in_s: 90 }, { id: "x" }],
      locked_s: null,
    });
    expect(s).toEqual({
      apps: [
        { id: "ab12cd34", console: "192.168.1.50", name: "Living room", pairedUnix: 7, lastSeenUnix: 9, connected: true },
        { id: "ee", console: "192.168.1.51", name: null, pairedUnix: 8, lastSeenUnix: null, connected: false },
      ],
      waiting: [{ console: "192.168.1.60", name: "Den", codeDisplay: "K7M-2QX" }],
      moves: [{ id: "ab12cd34", name: "Living room", addrOld: "192.168.1.50", addrNew: "192.168.1.70" }],
      lockedS: null,
    });
    // An engine that lists only the address: compared by typing, as before.
    expect(readPairing({ apps: [], waiting: [{ console: "192.168.1.60" }] })?.waiting).toEqual([
      { console: "192.168.1.60", name: null, codeDisplay: null },
    ]);
    expect(readPairResult(false, { error: "not_waiting" })).toEqual({ kind: "not_waiting" });
    expect(readPairing({ apps: [], waiting: [], locked_s: 420 })?.lockedS).toBe(420);
    expect(readPairing({ error: "not found" })).toBeNull();
    expect(readPairing(null)).toBeNull();
  });

  it("says what typing a code did", () => {
    expect(readPairResult(true, { ok: true, id: "a", console: "192.168.1.50", name: "Den" })).toEqual({
      kind: "ok",
      console: "192.168.1.50",
      name: "Den",
    });
    expect(readPairResult(false, { error: "wrong_code", tries_left: 3 })).toEqual({ kind: "wrong", triesLeft: 3 });
    expect(readPairResult(false, { error: "locked", retry_s: 300 })).toEqual({ kind: "locked", retryS: 300 });
    expect(readPairResult(false, { error: "code" })).toEqual({ kind: "code" });
    expect(readPairResult(false, { error: "app_gone" })).toEqual({ kind: "app_gone" });
    expect(readPairResult(false, "Not Found")).toEqual({ kind: "engine" });

    expect(pairResultText(tr, { kind: "ok", console: "192.168.1.50", name: null })).toBe("Paired with 192.168.1.50.");
    expect(pairResultText(tr, { kind: "wrong", triesLeft: 2 })).toContain("2 tries left");
    expect(pairResultText(tr, { kind: "locked", retryS: 61 })).toContain("2 min");
    expect(pairResultText(tr, { kind: "locked", retryS: null })).toContain("10 min");
  });
});
