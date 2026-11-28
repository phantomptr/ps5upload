import { describe, expect, it } from "vitest";

import {
  archiveChoosable,
  canChooseEntries,
  choiceOf,
  chosenPatterns,
  initialTicks,
  packageRole,
  packagesToUnpack,
  withArchiveChoice,
} from "./archiveChoice";

const rows = [
  { path: "Game/base.pkg", size: 100, files: 1 },
  { path: "Game/patch.pkg", size: 10, files: 1 },
  { path: "Extras", size: 7, files: 3 },
];

describe("choose files in an archive (#969)", () => {
  it("lists a zip, 7z, rar or tar, never a lone .zst", () => {
    for (const p of ["/x/a.zip", "C:\\x\\a.7z", "/x/a.part1.rar", "/x/a.tar", "/x/a.tar.zst", "/x/a.tgz"]) {
      expect(canChooseEntries(p), p).toBe(true);
    }
    for (const p of ["/x/disc.exfat.zst", "/x/a.pkg", "/x/a.7z.001"]) {
      expect(canChooseEntries(p), p).toBe(false);
    }
  });

  it("sends the chosen paths as =path entries after the excludes", () => {
    expect(chosenPatterns(["Game/base.pkg", "Extras"])).toEqual(["=Game/base.pkg", "=Extras"]);
    expect(chosenPatterns(null)).toEqual([]);
    expect(withArchiveChoice([".DS_Store"], ["Extras"])).toEqual([".DS_Store", "=Extras"]);
    // No choice: the excludes as they were, the whole archive.
    expect(withArchiveChoice([".DS_Store"], null)).toEqual([".DS_Store"]);
    expect(withArchiveChoice([], [])).toEqual([]);
  });

  it("every row ticked is the whole archive; some ticked are those paths and their bytes", () => {
    expect(choiceOf(rows, new Set(rows.map((r) => r.path)))).toEqual({ paths: null, bytes: 117, count: 3, files: 5 });
    expect(choiceOf(rows, new Set(["Game/patch.pkg", "Extras"]))).toEqual({
      paths: ["Game/patch.pkg", "Extras"],
      bytes: 17,
      count: 2,
      files: 4,
    });
    expect(choiceOf(rows, new Set()).count).toBe(0);
    // A cut listing never narrows what is sent.
    expect(choiceOf(rows, new Set(["Extras"]), false).paths).toBeNull();
  });

  it("starts from the earlier choice, or everything", () => {
    expect([...initialTicks(rows, null)]).toEqual(rows.map((r) => r.path));
    expect([...initialTicks(rows, ["Extras", "Gone/x"])]).toEqual(["Extras"]);
    // Nothing of the earlier choice is listed any more: everything again.
    expect(initialTicks(rows, ["Gone/x"]).size).toBe(3);
  });

  it("a part set's lead brings its parts; every package ticked is no choice", () => {
    const all = [{ path: "A/G_0.pkg" }, { path: "A/G_1.pkg" }, { path: "A/G_2.pkg" }, { path: "B/G_1.pkg" }, { path: "U.pkg" }];
    expect(packagesToUnpack(all, new Set(["A/G_0.pkg"]))).toEqual(["A/G_0.pkg", "A/G_1.pkg", "A/G_2.pkg"]);
    expect(packagesToUnpack(all, new Set(["U.pkg"]))).toEqual(["U.pkg"]);
    expect(packagesToUnpack(all, new Set(all.map((p) => p.path)))).toBeNull();
  });

  it("names a package's role from its category", () => {
    expect(packageRole("gd")).toBe("base");
    expect(packageRole("GP")).toBe("update");
    expect(packageRole("ac")).toBe("dlc");
    expect(packageRole("")).toBeNull();
  });

  it("offers Choose files on a waiting archive row read from here only", () => {
    const row = { sourceKind: "archive", sourcePath: "/x/a.zip", status: "pending" };
    expect(archiveChoosable(row)).toBe(true);
    expect(archiveChoosable({ ...row, status: "running" })).toBe(false);
    expect(archiveChoosable({ ...row, sourceKind: "file" })).toBe(false);
    expect(archiveChoosable({ ...row, sourcePath: "remote://nas/x/a.zip" })).toBe(false);
  });
});
