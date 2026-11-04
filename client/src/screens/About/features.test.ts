import { describe, expect, it } from "vitest";

import { FEATURES } from "./features";

describe("About feature cards", () => {
  it("lists the main features", () => {
    const keys = FEATURES.map((f) => f.titleKey);
    for (const k of [
      "about_feat_play_title",
      "about_feat_together_title",
      "about_feat_install_title",
      "about_feat_convert_title",
      "about_feat_saves_title",
      "about_feat_cheats_title",
    ]) {
      expect(keys).toContain(k);
    }
  });

  it("never mentions the payload port", () => {
    for (const f of FEATURES) {
      expect(f.bodyFallback).not.toMatch(/9021/);
    }
  });
});
