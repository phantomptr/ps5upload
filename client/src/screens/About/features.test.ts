import { describe, expect, it } from "vitest";

import { visibleFeatures } from "./features";

describe("About feature cards", () => {
  it("lists the main features", () => {
    const keys = visibleFeatures(true).map((f) => f.titleKey);
    for (const k of [
      "about_feat_install_title",
      "about_feat_convert_title",
      "about_feat_saves_title",
      "about_feat_cheats_title",
    ]) {
      expect(keys).toContain(k);
    }
  });

  it("doesn't offer sending payloads in the browser build", () => {
    const keys = visibleFeatures(false).map((f) => f.titleKey);
    expect(keys).not.toContain("about_feat_payloads_title");
    expect(visibleFeatures(true).map((f) => f.titleKey)).toContain(
      "about_feat_payloads_title",
    );
  });

  it("never mentions the payload port", () => {
    for (const f of visibleFeatures(true)) {
      expect(f.bodyFallback).not.toMatch(/9021/);
    }
  });
});
