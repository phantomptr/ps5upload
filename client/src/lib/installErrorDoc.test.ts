import { describe, expect, it } from "vitest";

import faqText from "virtual:doc/faq";

import { DOC_ANCHORS, faqLink, installErrorDoc, installErrorLink } from "./installErrorDoc";

// FAQ.md is what the in-app FAQ renders. Every anchor an error can link to must
// appear in it, or the toast's "What does this mean?" lands on an empty page.
const faq = faqText.toLowerCase();

describe("installErrorDoc", () => {
  it("maps each documented error to its explanation", () => {
    const cases: Array<[string, number | undefined, string]> = [
      ["The PS5 refused this package from its own storage (0x80b2116f)", undefined, DOC_ANCHORS.stagedRefused],
      ["install failed", 0x80b2116f, DOC_ANCHORS.stagedRefused],
      ["Sony 0x80B2150F", undefined, DOC_ANCHORS.stagedRefused],
      ["The PS5 never reached this computer at http://x (0x80431064)", undefined, DOC_ANCHORS.streamUnreachable],
      ["The PS5 cannot connect to this computer at http://x (timed out)", undefined, DOC_ANCHORS.streamUnreachable],
      ["x", 0x8041013d, DOC_ANCHORS.streamUnreachable],
      ["x", 0x80431068, DOC_ANCHORS.streamUnreachable],
      ["The PS5's proxy setting blocked the stream (0x80431084)", undefined, DOC_ANCHORS.proxy],
      ["E2-80B22410", undefined, DOC_ANCHORS.e2],
      ["CE-108255-1", undefined, DOC_ANCHORS.ce],
      ["missing base entitlement", undefined, DOC_ANCHORS.entitlement],
      ["shows View product instead of Play", undefined, DOC_ANCHORS.viewProduct],
      ["This PS4 game isn't playable on PS5", undefined, DOC_ANCHORS.ps4OnPs5],
      ["The PS5 declined the install. (0x80b21104)", undefined, DOC_ANCHORS.declined1104],
      ["x", 0x80b21104, DOC_ANCHORS.declined1104],
    ];
    for (const [raw, code, query] of cases) {
      expect(installErrorDoc(raw, code)?.query, `${raw} / ${code}`).toBe(query);
    }
  });

  it("has no explanation for an unrelated or empty error", () => {
    expect(installErrorDoc("disk full")).toBeNull();
    expect(installErrorDoc("")).toBeNull();
    expect(installErrorDoc(null)).toBeNull();
  });

  it("falls back to the support matrix and encodes the query", () => {
    expect(installErrorLink("disk full")).toBe(faqLink(DOC_ANCHORS.matrix));
    expect(faqLink("won't launch")).toBe("/faq?q=won't%20launch");
  });

  it("every anchor resolves to a section of FAQ.md", () => {
    for (const [name, anchor] of Object.entries(DOC_ANCHORS)) {
      expect(faq.includes(anchor.toLowerCase()), `${name}: "${anchor}" not in FAQ.md`).toBe(true);
    }
  });

  it("FAQ.md documents the PS5 fake-game limit and the matrix", () => {
    expect(faq).toContain("## install routes: what works for what (support matrix)");
    expect(faq).toContain("## my uploaded game won't launch");
    expect(faq).toContain("above 11.60");
    expect(faq).toContain("ps4 fake packages");
  });
});
