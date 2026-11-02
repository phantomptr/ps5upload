import { describe, expect, it } from "vitest";

import type { CheatMod } from "../../api/ps5";
import { afterToggle, anyWaiting, cheatAppliedLine } from "./cheatApplied";

const tr = (
  _k: string,
  v?: Record<string, string | number>,
  fallback?: string,
) => (fallback ?? "").replace("{reason}", String(v?.reason ?? ""));

const mod = (over: Partial<CheatMod> = {}): CheatMod => ({
  index: 8,
  name: "60 FPS (01.18)",
  desc: "",
  type: "checkbox",
  on: true,
  ...over,
});

describe("cheatAppliedLine (#627)", () => {
  it("says what happened to a switched-on cheat in the running game", () => {
    expect(cheatAppliedLine(mod({ applied: "applied" }), tr)).toEqual({
      tone: "ok",
      text: "Applied ✓",
    });
    expect(cheatAppliedLine(mod({ applied: "waiting" }), tr)).toEqual({
      tone: "wait",
      text: "Waiting for the game to load…",
    });
    expect(
      cheatAppliedLine(
        mod({
          applied: "failed",
          applied_reason: "doesn't match this game version",
        }),
        tr,
      ),
    ).toEqual({
      tone: "warn",
      text: "Couldn't apply: doesn't match this game version",
    });
    expect(cheatAppliedLine(mod({ applied: "changed" }), tr)?.text).toBe(
      "The game changed this; not touched",
    );
  });

  it("says nothing when it is off, a button, or the helper is older", () => {
    expect(
      cheatAppliedLine(mod({ on: false, applied: "applied" }), tr),
    ).toBeNull();
    expect(
      cheatAppliedLine(mod({ type: "button", applied: "applied" }), tr),
    ).toBeNull();
    expect(cheatAppliedLine(mod(), tr)).toBeNull();
  });

  it("a switch flipped by hand starts the status over", () => {
    const failed = mod({ applied: "failed", applied_reason: "x" });
    expect(afterToggle(failed, true)).toEqual({ ...mod(), applied: "applied" });
    expect(afterToggle(failed, false)).toEqual(mod({ on: false }));
  });

  it("re-reads only while a switched-on cheat waits", () => {
    expect(
      anyWaiting([mod({ applied: "applied" }), mod({ applied: "waiting" })]),
    ).toBe(true);
    expect(
      anyWaiting([
        mod({ applied: "applied" }),
        mod({ on: false, applied: "waiting" }),
      ]),
    ).toBe(false);
  });
});
