import { describe, expect, it } from "vitest";
import { applyRenamePattern, isSinglePathName } from "./index";

describe("applyRenamePattern", () => {
  it("returns the name unchanged for an empty pattern", () => {
    expect(applyRenamePattern("save.zip", "")).toBe("save.zip");
    expect(applyRenamePattern("save.zip", "   ")).toBe("save.zip");
  });

  it("returns the name unchanged for an unrecognised pattern", () => {
    expect(applyRenamePattern("save.zip", "garbage")).toBe("save.zip");
  });

  it("first-match substitution", () => {
    expect(applyRenamePattern("foo-foo.zip", "s/foo/bar/")).toBe(
      "bar-foo.zip",
    );
  });

  it("global substitution", () => {
    expect(applyRenamePattern("foo-foo.zip", "s/foo/bar/g")).toBe(
      "bar-bar.zip",
    );
  });

  it("case-insensitive substitution", () => {
    expect(applyRenamePattern("CUSA.zip", "s/cusa/PCSA/i")).toBe("PCSA.zip");
  });

  it("global + case-insensitive", () => {
    expect(applyRenamePattern("CuSa-cusa.zip", "s/cusa/X/gi")).toBe(
      "X-X.zip",
    );
  });

  it("prefix and suffix forms", () => {
    expect(applyRenamePattern("save.zip", "^backup_")).toBe("backup_save.zip");
    expect(applyRenamePattern("save.zip", "_v1$")).toBe("save_v1.zip");
    expect(applyRenamePattern("README", "_v1$")).toBe("README_v1");
  });

  it("case transforms", () => {
    expect(applyRenamePattern("Save.ZIP", "lower")).toBe("save.zip");
    expect(applyRenamePattern("Save.zip", "upper")).toBe("SAVE.ZIP");
  });

  it("ignores patterns where old is empty (s///)", () => {
    expect(applyRenamePattern("save.zip", "s//x/")).toBe("save.zip");
  });

  it("returns name unchanged when pattern is malformed", () => {
    expect(applyRenamePattern("save.zip", "s/no-second-slash")).toBe(
      "save.zip",
    );
    expect(applyRenamePattern("save.zip", "s/old/new")).toBe("save.zip");
  });

  it("does not treat regex metacharacters specially", () => {
    expect(applyRenamePattern("a.b.c", "s/./_/g")).toBe("a_b_c");
    expect(applyRenamePattern("a.b.c", "s/./_/")).toBe("a_b.c");
  });
});

describe("isSinglePathName", () => {
  it("accepts plain file and folder names", () => {
    expect(isSinglePathName("save.zip")).toBe(true);
    expect(isSinglePathName("CUSA00001-Backup")).toBe(true);
    expect(isSinglePathName("..backup")).toBe(true);
  });

  it("rejects traversal and nested path names", () => {
    expect(isSinglePathName("")).toBe(false);
    expect(isSinglePathName(" ")).toBe(false);
    expect(isSinglePathName(".")).toBe(false);
    expect(isSinglePathName("..")).toBe(false);
    expect(isSinglePathName("nested/name")).toBe(false);
    expect(isSinglePathName("nested\\name")).toBe(false);
    expect(isSinglePathName(" name")).toBe(false);
    expect(isSinglePathName("name ")).toBe(false);
  });
});
