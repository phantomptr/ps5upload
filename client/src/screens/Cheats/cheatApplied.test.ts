import { describe, expect, it } from "vitest";

import type { CheatMod } from "../../api/ps5";
import {
  afterToggle,
  anyWaiting,
  cheatAppliedLine,
  lateSeconds,
} from "./cheatApplied";

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

  it("says when it may have come too late for a patch the game reads at start", () => {
    // on time: just applied
    expect(
      cheatAppliedLine(mod({ applied: "applied", applied_after_ms: 180 }), tr),
    ).toEqual({ tone: "ok", text: "Applied ✓" });
    expect(
      cheatAppliedLine(
        mod({ applied: "applied", applied_after_ms: 6400, applied_late: true }),
        tr,
      ),
    ).toEqual({
      tone: "warn",
      text: "Applied 6 s after the game started — if it has no effect, restart the game with it on.",
    });
    // the helper did not see the game start
    expect(
      cheatAppliedLine(mod({ applied: "applied", applied_late: true }), tr),
    ).toEqual({
      tone: "warn",
      text: "Applied after the game had already started — if it has no effect, restart the game with it on.",
    });
    // only "applied" is ever late
    expect(
      cheatAppliedLine(mod({ applied: "waiting", applied_late: true }), tr)
        ?.tone,
    ).toBe("wait");
    expect(lateSeconds(3200)).toBe(3);
    expect(lateSeconds(200)).toBe(1);
    expect(lateSeconds(undefined)).toBeNull();
    expect(lateSeconds(-1)).toBeNull();
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
    // turned on by hand: when the player chose, not late
    const late = mod({
      applied: "applied",
      applied_after_ms: 9000,
      applied_late: true,
    });
    expect(afterToggle(late, true)).toEqual({ ...mod(), applied: "applied" });
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
