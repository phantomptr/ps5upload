import { describe, expect, it } from "vitest";
import {
  needsManualPartUploads,
  parseArchivePart,
  siblingParts,
  siblingPartPaths,
  diagnoseArchiveSet,
  parseSplitVolume,
  isSpannedZip,
} from "./archiveParts";

const SET = [
  "[SITE]- 01.021 PPSA23226.part01.zip",
  "[SITE]- 01.021 PPSA23226.part02.zip",
  "[SITE]- 01.021 PPSA23226.part03.zip",
  "readme.txt",
];

describe("parseArchivePart", () => {
  it("splits the marker off", () => {
    expect(parseArchivePart("Game.part01.zip")).toEqual({
      stem: "Game",
      index: 1,
      ext: "zip",
    });
    expect(parseArchivePart("Game.part7.7z")?.index).toBe(7);
    expect(parseArchivePart("Game.PART02.ZIP")?.ext).toBe("zip");
  });

  it("returns null for names with no marker", () => {
    expect(parseArchivePart("Game.zip")).toBeNull();
    // "part" without digits is not a marker.
    expect(parseArchivePart("Counterpart.zip")).toBeNull();
  });
});

describe("siblingParts", () => {
  it("finds the whole set and ignores unrelated files", () => {
    const parts = siblingParts(SET[0], SET);
    expect(parts.map((p) => p.index)).toEqual([1, 2, 3]);
  });

  it("does not mix extensions — a .rar of the same name is a different set", () => {
    const names = ["G.part1.zip", "G.part2.zip", "G.part1.rar", "G.part2.rar"];
    expect(siblingParts("G.part1.zip", names).map((p) => p.ext)).toEqual([
      "zip",
      "zip",
    ]);
  });

  it("returns nothing when the part is alone — no set to warn about", () => {
    expect(siblingParts("G.part1.zip", ["G.part1.zip", "other.zip"])).toEqual(
      [],
    );
  });

  it("counts a duplicated index once (G.part1 and G.part01)", () => {
    const names = ["G.part1.zip", "G.part01.zip", "G.part2.zip"];
    expect(siblingParts("G.part1.zip", names).map((p) => p.index)).toEqual([
      1, 2,
    ]);
  });
});

describe("needsManualPartUploads", () => {
  it("is true for a multi-part zip set", () => {
    expect(needsManualPartUploads(SET[0], SET)).toBe(true);
  });

  it("is FALSE for rar — UnRAR really does pull the siblings itself", () => {
    const rars = ["G.part1.rar", "G.part2.rar", "G.part3.rar"];
    expect(needsManualPartUploads("G.part1.rar", rars)).toBe(false);
  });

  it("is false for an ordinary single archive", () => {
    expect(needsManualPartUploads("Game.zip", ["Game.zip"])).toBe(false);
  });
});

describe("siblingPartPaths", () => {
  it("keeps a Windows path a Windows path", () => {
    const dir = "C:\\Users\\Majid\\Downloads\\Black Myth Wukong";
    expect(siblingPartPaths(`${dir}\\${SET[0]}`, SET)).toEqual([
      `${dir}\\${SET[0]}`,
      `${dir}\\${SET[1]}`,
      `${dir}\\${SET[2]}`,
    ]);
  });

  it("keeps a POSIX path a POSIX path", () => {
    expect(siblingPartPaths(`/home/me/games/${SET[1]}`, SET)).toEqual([
      `/home/me/games/${SET[0]}`,
      `/home/me/games/${SET[1]}`,
      `/home/me/games/${SET[2]}`,
    ]);
  });

  it("orders by part number, not by listing order", () => {
    const shuffled = [SET[2], SET[0], SET[3], SET[1]];
    expect(siblingPartPaths(`/g/${SET[0]}`, shuffled)).toEqual([
      `/g/${SET[0]}`,
      `/g/${SET[1]}`,
      `/g/${SET[2]}`,
    ]);
  });

  it("preserves the zero-padding actually on disk", () => {
    const mixed = ["Game.part1.zip", "Game.part2.zip", "Game.part10.zip"];
    expect(siblingPartPaths("/g/Game.part1.zip", mixed)).toEqual([
      "/g/Game.part1.zip",
      "/g/Game.part2.zip",
      "/g/Game.part10.zip",
    ]);
  });

  it("returns nothing for a lone archive", () => {
    expect(siblingPartPaths("/g/Game.zip", ["Game.zip"])).toEqual([]);
    expect(siblingPartPaths("/g/Game.part01.zip", ["Game.part01.zip"])).toEqual(
      [],
    );
  });

  it("returns nothing when there is no directory to rebuild against", () => {
    expect(siblingPartPaths(SET[0], SET)).toEqual([]);
  });

  it("does not mix a rar set into a zip set", () => {
    const both = [...SET, "[SITE]- 01.021 PPSA23226.part01.rar"];
    expect(siblingPartPaths(`/g/${SET[0]}`, both)).toEqual([
      `/g/${SET[0]}`,
      `/g/${SET[1]}`,
      `/g/${SET[2]}`,
    ]);
  });
});

// Reported 2026-08-23 against 5.4.17: part 1 uploaded, "Upload complete",
// nothing about the other ten parts. The folder turned out to hold ONE zip
// and TEN rar volumes under the same stem — so the extension-matching in
// siblingParts found zero siblings and the app said nothing at all.
const MIXED = [
  "[DLPSGAME.COM]- 01.021 PPSA23226.part01.zip",
  ...Array.from(
    { length: 10 },
    (_, i) =>
      `[DLPSGAME.COM]- 01.021 PPSA23226.part${String(i + 2).padStart(2, "0")}.rar`,
  ),
];

describe("diagnoseArchiveSet", () => {
  it("flags the mixed zip/rar folder the old code was silent about", () => {
    expect(needsManualPartUploads(MIXED[0], MIXED)).toBe(false); // the bug
    expect(diagnoseArchiveSet(MIXED[0], MIXED)).toEqual({
      kind: "mixed-extensions",
      selectedExt: "zip",
      otherExt: "rar",
      otherCount: 10,
      missingFirst: "[DLPSGAME.COM]- 01.021 PPSA23226.part01.rar",
    });
  });

  it("names no missing volume when the other set starts at part 1", () => {
    const folder = ["G.part01.zip", "G.part01.rar", "G.part02.rar"];
    const d = diagnoseArchiveSet("G.part01.zip", folder);
    expect(d).toMatchObject({ kind: "mixed-extensions", missingFirst: null });
  });

  it("still reports a plain zip set as manual-parts", () => {
    expect(diagnoseArchiveSet(SET[0], SET)).toEqual({
      kind: "manual-parts",
      total: 3,
    });
  });

  it("says nothing for a healthy rar set — unrar follows the volumes", () => {
    const rars = ["G.part01.rar", "G.part02.rar", "G.part03.rar"];
    expect(diagnoseArchiveSet(rars[0], rars)).toBeNull();
  });

  it("says nothing for a lone archive", () => {
    expect(diagnoseArchiveSet("Game.zip", ["Game.zip"])).toBeNull();
  });

  it("picks the largest foreign set when several formats collide", () => {
    const folder = [
      "G.part01.zip",
      "G.part02.rar",
      "G.part03.rar",
      "G.part04.7z",
    ];
    expect(diagnoseArchiveSet("G.part01.zip", folder)).toMatchObject({
      otherExt: "rar",
      otherCount: 2,
    });
  });
});

describe("parseSplitVolume", () => {
  it("reads 7-Zip numeric splits and names the joined archive", () => {
    expect(parseSplitVolume("Game.7z.001")).toEqual({
      scheme: "numeric",
      index: 1,
      entryName: "Game.7z",
    });
    expect(parseSplitVolume("Game.zip.014")).toMatchObject({
      scheme: "numeric",
      index: 14,
      entryName: "Game.zip",
    });
    expect(parseSplitVolume("Game.001")).toMatchObject({
      scheme: "numeric",
      entryName: "Game",
    });
  });

  it("reads spanned-zip and old-style rar volumes", () => {
    expect(parseSplitVolume("Game.z01")).toEqual({
      scheme: "zip",
      index: 1,
      entryName: "Game.zip",
    });
    expect(parseSplitVolume("Game.r00")).toEqual({
      scheme: "rar",
      index: 0,
      entryName: "Game.rar",
    });
  });

  it("leaves whole archives and .partN sets alone", () => {
    for (const n of [
      "Game.zip",
      "Game.7z",
      "Game.rar",
      "Game.part01.zip",
      "Game.part01.rar",
      "readme.txt",
    ]) {
      expect(parseSplitVolume(n)).toBeNull();
    }
  });
});

describe("isSpannedZip", () => {
  it("is true only when .zNN volumes sit beside the .zip", () => {
    const spanned = ["G.zip", "G.z01", "G.z02"];
    expect(isSpannedZip("G.zip", spanned)).toBe(true);
    expect(isSpannedZip("G.zip", ["G.zip"])).toBe(false);
    expect(isSpannedZip("G.zip", ["G.zip", "Other.z01"])).toBe(false);
  });
});

describe("diagnoseArchiveSet — native split schemes", () => {
  it("points a .r00 pick at the .rar that opens it", () => {
    const folder = ["G.rar", "G.r00", "G.r01"];
    expect(diagnoseArchiveSet("G.r00", folder)).toEqual({
      kind: "pick-entry-volume",
      entryName: "G.rar",
    });
  });

  it("flags a rar volume set whose .rar is missing", () => {
    const folder = ["G.r00", "G.r01"];
    expect(diagnoseArchiveSet("G.r00", folder)).toMatchObject({
      kind: "split-unsupported",
      entryName: "G.rar",
    });
  });

  it("flags a 7-Zip numeric split and counts the volumes", () => {
    const folder = ["G.7z.001", "G.7z.002", "G.7z.003", "notes.txt"];
    expect(diagnoseArchiveSet("G.7z.001", folder)).toEqual({
      kind: "split-unsupported",
      scheme: "numeric",
      entryName: "G.7z",
      volumeCount: 3,
    });
  });

  it("flags a spanned zip picked by its .zip", () => {
    const folder = ["G.zip", "G.z01", "G.z02"];
    expect(diagnoseArchiveSet("G.zip", folder)).toMatchObject({
      kind: "split-unsupported",
      scheme: "zip",
      volumeCount: 3,
    });
  });

  it("leaves an ordinary single .zip alone", () => {
    expect(diagnoseArchiveSet("G.zip", ["G.zip", "readme.txt"])).toBeNull();
  });

  it("leaves a healthy .partN.rar set alone", () => {
    const rars = ["G.part01.rar", "G.part02.rar"];
    expect(diagnoseArchiveSet("G.part01.rar", rars)).toBeNull();
  });
});

describe("numeric suffixes that are not split archives", () => {
  it("leaves an ordinary file with a numeric extension alone", () => {
    for (const n of ["save.2024", "backup.1999", "PS5UPDATE.100", "clip.264"]) {
      expect(diagnoseArchiveSet(n, [n, "readme.txt"])).toBeNull();
    }
  });

  it("still flags it once a second volume shows up", () => {
    expect(diagnoseArchiveSet("save.001", ["save.001", "save.002"])).toMatchObject(
      { kind: "split-unsupported", volumeCount: 2 },
    );
  });
});
