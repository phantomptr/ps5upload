import { describe, expect, it } from "vitest";

import {
  isSafeContentId,
  stagingBasename,
  stagingSubdirForCategory,
  stagingDirectoryForPackage,
  fingerprintFromStagingSubdir,
  PACKAGE_FINGERPRINT_DIR_HEX_LEN,
  categoryForSubdir,
  pkgCategoryLabel,
  isAddonCategory,
} from "./pkgStagingPath";

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

describe("stagingSubdirForCategory", () => {
  it("routes updates and DLC to their own dirs, base/unknown to root", () => {
    expect(stagingSubdirForCategory("gd")).toBe(""); // base
    expect(stagingSubdirForCategory("gp")).toBe("updates"); // patch
    expect(stagingSubdirForCategory("ac")).toBe("dlc"); // add-on
    expect(stagingSubdirForCategory("")).toBe("");
    expect(stagingSubdirForCategory(null)).toBe("");
    expect(stagingSubdirForCategory(undefined)).toBe("");
    expect(stagingSubdirForCategory("weird")).toBe("");
  });

  it("gives a base and its update DIFFERENT staging paths (the bug fix)", () => {
    // Same ContentID, different category → must not collide.
    const cid = "EP9000-CUSA00207_00-BLOODBORNE000000";
    const base = `lib/${stagingSubdirForCategory("gd")}/${cid}.pkg`.replace(
      "//",
      "/",
    );
    const update = `lib/${stagingSubdirForCategory("gp")}/${cid}.pkg`;
    expect(base).not.toBe(update);
    // basename is identical (installer keys on it); only the dir differs.
    expect(base.endsWith(`${cid}.pkg`)).toBe(true);
    expect(update.endsWith(`${cid}.pkg`)).toBe(true);
  });
});

describe("stagingDirectoryForPackage", () => {
  const fpA = "a".repeat(64);
  const fpB = "b".repeat(64);

  it("keeps exact update and DLC variants in separate directories", () => {
    expect(stagingDirectoryForPackage("gp", fpA)).toBe(
      `updates/${fpA.slice(0, PACKAGE_FINGERPRINT_DIR_HEX_LEN)}`,
    );
    expect(stagingDirectoryForPackage("gp", fpB)).toBe(
      `updates/${fpB.slice(0, PACKAGE_FINGERPRINT_DIR_HEX_LEN)}`,
    );
    expect(stagingDirectoryForPackage("ac", fpA)).toBe(
      `dlc/${fpA.slice(0, PACKAGE_FINGERPRINT_DIR_HEX_LEN)}`,
    );
  });

  it("keeps the longest canonical AppInstUtil path below 128 bytes", () => {
    const dir = stagingDirectoryForPackage("gp", fpA);
    const maxContentIdPkg = `${"C".repeat(36)}.pkg`;
    const absolute = `/user/data/ps5upload/pkg_library/${dir}/${maxContentIdPkg}`;
    expect(absolute.length).toBeLessThan(128);
  });

  it("keeps base packages at the legacy root", () => {
    expect(stagingDirectoryForPackage("gd", fpA)).toBe("");
  });

  it("falls back to the legacy category dir for missing/bad fingerprints", () => {
    expect(stagingDirectoryForPackage("gp", undefined)).toBe("updates");
    expect(stagingDirectoryForPackage("ac", "../escape")).toBe("dlc");
  });
});

describe("categoryForSubdir", () => {
  it("is the inverse mapping used by refresh", () => {
    expect(categoryForSubdir("updates")).toBe("gp");
    expect(categoryForSubdir("dlc")).toBe("ac");
    expect(categoryForSubdir(`updates/${"a".repeat(64)}`)).toBe("gp");
    expect(categoryForSubdir(`dlc/${"b".repeat(64)}`)).toBe("ac");
    expect(categoryForSubdir("")).toBeUndefined();
    expect(categoryForSubdir("anything-else")).toBeUndefined();
  });
});

describe("fingerprintFromStagingSubdir", () => {
  it("accepts current short tokens and legacy full fingerprints", () => {
    expect(fingerprintFromStagingSubdir(`updates/${"a".repeat(32)}`)).toBe(
      "a".repeat(32),
    );
    expect(fingerprintFromStagingSubdir(`dlc/${"B".repeat(64)}`)).toBe(
      "b".repeat(64),
    );
  });

  it("rejects malformed or differently-sized directory names", () => {
    expect(fingerprintFromStagingSubdir("updates")).toBeUndefined();
    expect(
      fingerprintFromStagingSubdir(`updates/${"a".repeat(31)}`),
    ).toBeUndefined();
    expect(fingerprintFromStagingSubdir("updates/../escape")).toBeUndefined();
  });
});

describe("pkgCategoryLabel", () => {
  it("labels the badge-worthy categories", () => {
    expect(pkgCategoryLabel("gd")).toBe("Base");
    expect(pkgCategoryLabel("gp")).toBe("Update");
    expect(pkgCategoryLabel("ac")).toBe("DLC");
    expect(pkgCategoryLabel("")).toBeNull();
    expect(pkgCategoryLabel(undefined)).toBeNull();
    expect(pkgCategoryLabel("misc")).toBeNull();
  });
});

describe("isAddonCategory", () => {
  it("flags updates and DLC (which need the base installed first)", () => {
    expect(isAddonCategory("gp")).toBe(true); // update
    expect(isAddonCategory("ac")).toBe(true); // DLC
  });
  it("does not flag base games or unknown categories", () => {
    expect(isAddonCategory("gd")).toBe(false); // base
    expect(isAddonCategory("")).toBe(false);
    expect(isAddonCategory(undefined)).toBe(false);
    expect(isAddonCategory(null)).toBe(false);
    expect(isAddonCategory("misc")).toBe(false);
  });
});
