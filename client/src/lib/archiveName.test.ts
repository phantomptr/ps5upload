import { describe, expect, it } from "vitest";
import {
  archiveFolderName,
  extractCause,
  freeFolderName,
  isExtractableArchive,
  needsPassword,
  toExtractResult,
  validFolderName,
  freeNote,
  freePlanNeeds,
  freePlanSaves,
  partlyFreed,
  toFreePlan,
} from "./archiveName";

describe("isExtractableArchive", () => {
  it("accepts the single-file formats in any case", () => {
    for (const n of ["a.zip", "a.ZIPX", "a.7z", "a.rar", "a.tar", "a.tgz", "a.tbz", "a.tbz2", "a.txz", "a.tzst", "a.tar.gz", "a.gz", "a.bz2", "a.xz", "a.zst", "a.TAR.ZST"]) {
      expect(isExtractableArchive(n), n).toBe(true);
    }
  });
  it("offers only the first volume of a split archive", () => {
    expect(isExtractableArchive("g.part1.rar")).toBe(true);
    expect(isExtractableArchive("g.part01.rar")).toBe(true);
    expect(isExtractableArchive("g.part001.rar")).toBe(true);
    expect(isExtractableArchive("g.part2.rar")).toBe(false);
    expect(isExtractableArchive("g.part02.rar")).toBe(false);
    expect(isExtractableArchive("g.part10.rar")).toBe(false);
    expect(isExtractableArchive("g.7z.001")).toBe(true);
    expect(isExtractableArchive("g.zip.001")).toBe(true);
    expect(isExtractableArchive("g.001")).toBe(true);
    expect(isExtractableArchive("g.7z.002")).toBe(false);
    expect(isExtractableArchive("g.010")).toBe(false);
  });
  it("leaves other files (and .ffpfsc) alone", () => {
    for (const n of ["a.ffpfsc", "a.exfat", "a.pkg", "zip", "a.zip.txt", "a.rar5", "a.1"]) {
      expect(isExtractableArchive(n), n).toBe(false);
    }
  });
});

describe("archiveFolderName", () => {
  it("strips the archive extensions", () => {
    expect(archiveFolderName("Game.zip")).toBe("Game");
    expect(archiveFolderName("x.tar.gz")).toBe("x");
    expect(archiveFolderName("x.TAR.XZ")).toBe("x");
    expect(archiveFolderName("x.tgz")).toBe("x");
    expect(archiveFolderName("x.tar")).toBe("x");
    expect(archiveFolderName("x.7z.001")).toBe("x");
    expect(archiveFolderName("x.zip.001")).toBe("x");
    expect(archiveFolderName("x.001")).toBe("x");
    expect(archiveFolderName("x.part01.rar")).toBe("x");
    expect(archiveFolderName("x.part1.rar")).toBe("x");
    expect(archiveFolderName("x.rar")).toBe("x");
    expect(archiveFolderName("x.bin.gz")).toBe("x.bin");
    expect(archiveFolderName("My Game v1.02.7z")).toBe("My Game v1.02");
  });
  it("falls back when nothing is left", () => {
    expect(archiveFolderName(".zip")).toBe("extracted");
    expect(archiveFolderName("..zip")).toBe("extracted");
    expect(archiveFolderName(".tar.gz")).toBe("extracted");
  });
});

describe("folder names", () => {
  it("validates one plain name", () => {
    expect(validFolderName("a")).toBe(true);
    expect(validFolderName("")).toBe(false);
    expect(validFolderName(".")).toBe(false);
    expect(validFolderName("..")).toBe(false);
    expect(validFolderName("a/b")).toBe(false);
    expect(validFolderName("x".repeat(256))).toBe(false);
  });
  it("finds a free name", () => {
    const taken = new Set(["x", "x (2)"]);
    expect(freeFolderName("x", (n) => taken.has(n))).toBe("x (3)");
    expect(freeFolderName("y", (n) => taken.has(n))).toBe("y");
  });
});

describe("extract results and causes", () => {
  it("reads a result with defaults", () => {
    expect(toExtractResult({ path: "/data/x", files: 3, skipped: 1 })).toEqual({
      path: "/data/x",
      files: 3,
      dirs: 0,
      bytes: 0,
      skipped: 1,
      unsafe: 0,
      links: 0,
      partsFreed: 0,
      bytesFreed: 0,
    });
    expect(toExtractResult(null).files).toBe(0);
    expect(toExtractResult({ path: "/data/x", parts_freed: 4, bytes_freed: 4000 })).toMatchObject({ partsFreed: 4, bytesFreed: 4000 });
  });
  it("finds the cause token", () => {
    expect(extractCause("fs_extract_no_space: needs 5 bytes, 1 free")).toBe("fs_extract_no_space");
    expect(extractCause("job failed: fs_extract_corrupt")).toBe("fs_extract_corrupt");
    expect(extractCause("other")).toBeNull();
    expect(needsPassword("fs_extract_password_required")).toBe(true);
    expect(needsPassword("fs_extract_wrong_password")).toBe(true);
    expect(needsPassword("fs_extract_rar_encrypted")).toBe(false);
  });
});

describe("deleting the archive as it unpacks", () => {
  const plan = toFreePlan({
    mode: "parts",
    parts: 10,
    archive_bytes: 10_000,
    unpacked: 12_000,
    peak: 3_000,
    same_drive: true,
    known: true,
    why: "",
  })!;

  it("reads the console's plan; an older console answers none", () => {
    expect(plan).toEqual({
      mode: "parts",
      parts: 10,
      archiveBytes: 10_000,
      unpacked: 12_000,
      peak: 3_000,
      sameDrive: true,
      known: true,
      why: "",
    });
    expect(toFreePlan(undefined)).toBeNull();
    expect(toFreePlan({ mode: "weird", parts: -1 })).toMatchObject({ mode: "", parts: 0, known: false });
  });

  it("saves what it unpacks to less the most it takes at once, only part by part on the same drive", () => {
    expect(freePlanSaves(plan)).toBe(9_000);
    expect(freePlanNeeds(plan)).toBe(3_000);
    expect(freeNote(plan)).toEqual({ kind: "parts", parts: 10, saves: 9_000 });
    // Another drive: no room made where it unpacks, and it needs all of it there.
    const other = { ...plan, sameDrive: false };
    expect(freePlanSaves(other)).toBe(0);
    expect(freePlanNeeds(other)).toBe(12_000);
    expect(freeNote(other)).toEqual({ kind: "other_drive" });
    // At the end only: nothing saved while it runs.
    const end = { ...plan, mode: "end" as const, peak: 12_000 };
    expect(freePlanSaves(end)).toBe(0);
    expect(freeNote(end)).toEqual({ kind: "end" });
    // Not known (a set whose sizes the listing doesn't give): no figure is made up.
    const unknown = { ...plan, known: false };
    expect(freePlanSaves(unknown)).toBe(0);
    expect(freePlanNeeds(unknown)).toBeNull();
    expect(freeNote(unknown)).toEqual({ kind: "parts_plain", parts: 10 });
    expect(freeNote({ ...plan, mode: "", why: "fs_extract_free_refused: x" })).toEqual({ kind: "refused" });
  });

  it("reads what a stop after parts were deleted left, whatever wraps it", () => {
    const raw = "ARCHIVE_EXTRACT(/data/g.part1.rar): fs_extract_partly_freed: 3 5 g.part4.rar.partial-deleted | /data/g | fs_extract_no_space: g/a.bin, 0 free";
    expect(partlyFreed(raw)).toEqual({
      freed: 3,
      left: 5,
      firstLeft: "g.part4.rar.partial-deleted",
      folder: "/data/g",
      cause: "fs_extract_no_space: g/a.bin, 0 free",
    });
    expect(extractCause(raw)).toBe("fs_extract_partly_freed");
    expect(partlyFreed("fs_extract_partly_freed: 2 0 - | /data/My Game | fs_extract_cancelled")).toMatchObject({
      left: 0,
      firstLeft: "-",
      folder: "/data/My Game",
      cause: "fs_extract_cancelled",
    });
    expect(partlyFreed("fs_extract_no_space")).toBeNull();
  });
});
