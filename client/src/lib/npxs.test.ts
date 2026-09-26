import { describe, expect, it } from "vitest";

import { isNpxsContentId, NPXS_CONTENT_ID_RE } from "./npxs";

describe("isNpxsContentId", () => {
  it("matches canonical NPXS content_ids across regions", () => {
    // Every region prefix Sony uses for system pkgs. A regression that
    // narrows the regex to one region would silently let other-region
    // NPXS pkgs into the polling loop and surface the "failed" UI.
    expect(isNpxsContentId("UP0000-NPXS21001_00-PS5SOMETHING0000")).toBe(true);
    expect(isNpxsContentId("EP9000-NPXS40012_00-PS5OTHER00000000")).toBe(true);
    expect(isNpxsContentId("JP0000-NPXS33333_00-XXXXXXXXXXXXXXXX")).toBe(true);
    expect(isNpxsContentId("HP0001-NPXS00001_00-XXXXXXXXXXXXXXXX")).toBe(true);
  });

  it("matches case-insensitively", () => {
    // The /i flag matters: BGFT and AppInstUtil sometimes echo back
    // content_ids in different cases; downcasing changes the path.
    expect(isNpxsContentId("up0000-npxs21001_00")).toBe(true);
    expect(isNpxsContentId("Up0000-Npxs21001_00")).toBe(true);
  });

  it("rejects ordinary game content_ids", () => {
    // Regular game pkgs follow CUSA / PPSA / PCSA / EP-prefix shapes.
    // A false positive here would route a real game install through
    // the fire-and-forget path and skip its real progress polling.
    expect(isNpxsContentId("UP0006-CUSA45456_00-MARVELSAVENGER01")).toBe(false);
    expect(isNpxsContentId("EP4040-PPSA01342_00-DEADSPACEPS5BB00")).toBe(false);
    expect(isNpxsContentId("HP9000-PCSA12345_00-XXXXXXXXXXXXXXXX")).toBe(false);
  });

  it("rejects partial matches and obvious garbage", () => {
    expect(isNpxsContentId("")).toBe(false);
    expect(isNpxsContentId(undefined)).toBe(false);
    expect(isNpxsContentId(null)).toBe(false);
    expect(isNpxsContentId("NPXS21001")).toBe(false); // missing region prefix
    expect(isNpxsContentId("UP0000-NPXS")).toBe(false); // missing digits after NPXS
    expect(isNpxsContentId("UP-NPXS21001")).toBe(false); // wrong region shape
    expect(isNpxsContentId("XX0000-NPXS21001")).toBe(true); // any 2 letters allowed (anchored 'a-z')
  });

  it("anchored at start so a trailing match doesn't sneak through", () => {
    // A poorly-anchored regex would match "garbage UP0000-NPXS21001"
    // when the user pastes a multi-line error message that happens to
    // contain an NPXS-shaped substring further down.
    expect(isNpxsContentId(" UP0000-NPXS21001_00")).toBe(false);
    expect(isNpxsContentId("garbage UP0000-NPXS21001_00")).toBe(false);
  });

  it("exposes the regex for places that need .test() inline", () => {
    // The regex is exported separately so callers that need to test
    // string fragments inside larger context (e.g. the engine's
    // /api/install error text matching the content_id) can avoid the
    // wrapper overhead.
    expect(NPXS_CONTENT_ID_RE.test("UP0000-NPXS21001_00")).toBe(true);
    expect(NPXS_CONTENT_ID_RE.test("UP0006-CUSA45456_00")).toBe(false);
  });
});
