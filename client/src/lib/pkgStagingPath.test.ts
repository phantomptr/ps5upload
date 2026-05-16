import { describe, expect, it } from "vitest";

import { isSafeContentId, stagingBasename } from "./pkgStagingPath";

describe("isSafeContentId", () => {
  it("accepts realistic Sony ContentIDs", () => {
    expect(isSafeContentId("IV9999-PSPS69691_00-SONICLOADER00001")).toBe(true);
    expect(isSafeContentId("EP9000-CUSA00744_00-METALGEARSOLIDV0")).toBe(true);
    expect(isSafeContentId("UP0001-NPXS40012_00-BLURAYPLAYERAPP1")).toBe(true);
  });

  it("rejects empty / null / undefined", () => {
    expect(isSafeContentId("")).toBe(false);
    expect(isSafeContentId(null)).toBe(false);
    expect(isSafeContentId(undefined)).toBe(false);
  });

  it("rejects overlong IDs (> 36 chars)", () => {
    // Sony's header field is exactly 36 bytes — anything longer is
    // either a parse bug or a tampered header. Refuse rather than
    // truncate so we don't accidentally collide with someone else's
    // real ContentID.
    expect(isSafeContentId("A".repeat(37))).toBe(false);
    expect(isSafeContentId("A".repeat(36))).toBe(true);
  });

  it("rejects path traversal characters", () => {
    expect(isSafeContentId("../../etc/passwd")).toBe(false);
    expect(isSafeContentId("..")).toBe(false);
    expect(isSafeContentId("foo/bar")).toBe(false);
    expect(isSafeContentId("foo\\bar")).toBe(false);
    expect(isSafeContentId("foo..bar")).toBe(false);
  });

  it("rejects shell-injection / NUL / control chars", () => {
    expect(isSafeContentId("foo;rm")).toBe(false);
    expect(isSafeContentId("foo bar")).toBe(false);
    expect(isSafeContentId("foo\0bar")).toBe(false);
    expect(isSafeContentId("foo\nbar")).toBe(false);
    expect(isSafeContentId("foo*bar")).toBe(false);
  });
});

describe("stagingBasename", () => {
  it("uses <ContentID>.pkg when ContentID is safe", () => {
    expect(
      stagingBasename(
        "IV9999-PSPS69691_00-SONICLOADER00001",
        "queue-id-123",
        1700000000000,
      ),
    ).toBe("IV9999-PSPS69691_00-SONICLOADER00001.pkg");
  });

  it("falls back to <queueId>_<ts>.pkg when ContentID is missing", () => {
    expect(stagingBasename("", "queue-id-123", 1700000000000)).toBe(
      "queue-id-123_1700000000000.pkg",
    );
    expect(stagingBasename(null, "qid", 42)).toBe("qid_42.pkg");
    expect(stagingBasename(undefined, "qid", 42)).toBe("qid_42.pkg");
  });

  it("falls back when ContentID is malformed (oversized)", () => {
    // A parse bug that returns a giant string must not cause us to
    // write a file with that giant basename. Fall back is correct.
    expect(stagingBasename("X".repeat(50), "qid", 99)).toBe("qid_99.pkg");
  });

  it("falls back when ContentID contains path traversal", () => {
    // Defence-in-depth: even if a tampered PKG header sneaks past
    // the parser, the basename derivation refuses to point at
    // /user/data/ps5upload/pkg_temp/../../somewhere/evil.
    expect(stagingBasename("../escape", "qid", 1)).toBe("qid_1.pkg");
    expect(stagingBasename("foo..bar", "qid", 1)).toBe("qid_1.pkg");
    expect(stagingBasename("a/b", "qid", 1)).toBe("qid_1.pkg");
  });

  it("is deterministic for the same inputs (same name → same path)", () => {
    // A retry of the same PKG should overwrite the same staging
    // file rather than accumulating duplicates. Critical because
    // the engine's 24h cleanup is lazy.
    const a = stagingBasename(
      "IV9999-PSPS69691_00-SONICLOADER00001",
      "queue-A",
      111,
    );
    const b = stagingBasename(
      "IV9999-PSPS69691_00-SONICLOADER00001",
      "queue-B-different-id",
      999,
    );
    // Both reduce to the same path because ContentID is the
    // identity for retries.
    expect(a).toBe(b);
  });
});
