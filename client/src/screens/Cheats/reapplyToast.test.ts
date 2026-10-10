import { describe, expect, it } from "vitest";

import type { CheatsStatusResponse } from "../../api/ps5";
import { reapplyToast } from "./reapplyToast";

const tr = (_k: string, _v?: Record<string, string | number>, fallback?: string) => fallback ?? "";

const status = (over: Partial<CheatsStatusResponse>): CheatsStatusResponse => ({
  enabled: true,
  patches_last: 0,
  patches_total: 0,
  game_running: false,
  game_title_id: "",
  game_pid: 0,
  ...over,
});

describe("reapplyToast", () => {
  it("names the game it re-applied to", () => {
    const t = reapplyToast(status({ game_running: true, game_title_id: "PPSA07631" }), tr);
    expect(t.tone).toBe("success");
    expect(t.message).toContain("PPSA07631");
  });

  it("does not claim a re-apply when no game is running", () => {
    for (const st of [status({ game_running: false }), null]) {
      const t = reapplyToast(st, tr);
      expect(t.tone).toBe("info");
      expect(t.message).toMatch(/No game is running/);
    }
  });
});
