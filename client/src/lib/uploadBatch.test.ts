import { describe, expect, it } from "vitest";

import { classifyScanEntry, validateBatch, volumeOfDest, type BatchCheckRow } from "./uploadBatch";

describe("classifying a scanned folder's children", () => {
  it("pre-checks games, packages, archives and images, and leaves the rest out", () => {
    expect(classifyScanEntry("My Game", true)).toBe("folder");
    expect(classifyScanEntry("a.pkg", false)).toBe("package");
    expect(classifyScanEntry("g.part1.rar", false)).toBe("archive");
    expect(classifyScanEntry("G.exfat", false)).toBe("image");
    expect(classifyScanEntry("readme.txt", false)).toBe("other");
    expect(classifyScanEntry(".DS_Store", false)).toBe("other");
  });
});

describe("the drive a destination is on", () => {
  it("is /data or the /mnt drive", () => {
    expect(volumeOfDest("/data/homebrew/G")).toBe("/data");
    expect(volumeOfDest("/mnt/ext0/ps5upload/pkg_library/a.pkg")).toBe("/mnt/ext0");
    expect(volumeOfDest("/user/app/x")).toBe("/user");
  });
});

describe("checking a batch before it is added", () => {
  const row = (id: string, dest: string, size: number | null, over: Partial<BatchCheckRow> = {}): BatchCheckRow => ({
    id,
    sourcePath: `/src/${id}`,
    dest,
    size,
    needsPassword: false,
    ...over,
  });

  it("flags both rows that would land in the same place, and blocks", () => {
    const r = validateBatch([row("a", "/data/homebrew/G", 1), row("b", "/data/homebrew/G", 1)], [], new Map());
    expect(r.blocked).toBe(true);
    expect(r.issues.get("a")?.[0].text).toMatch(/same destination/);
    expect(r.issues.get("b")?.[0].key).toBe("batch_same_dest");
  });

  it("skips a row already in the queue, without blocking the rest", () => {
    const r = validateBatch(
      [row("a", "/data/homebrew/A", 1), row("b", "/data/homebrew/B", 1)],
      [{ sourcePath: "/src/a", resolvedDest: "/data/homebrew/A" }],
      new Map(),
    );
    expect(r.blocked).toBe(false);
    expect(r.skip.has("a")).toBe(true);
    expect(r.skip.has("b")).toBe(false);
  });

  it("blocks a password-protected archive until it has its password", () => {
    const r = validateBatch([row("a", "/data/homebrew/A", 1, { needsPassword: true })], [], new Map());
    expect(r.blocked).toBe(true);
    expect(r.issues.get("a")?.[0].text).toMatch(/password/);
  });

  it("blocks only when a drive clearly can't take it, and warns when it can't tell", () => {
    const free = new Map<string, number | null>([["/data", 100], ["/mnt/ext0", null]]);
    const over = validateBatch([row("a", "/data/x/A", 60), row("b", "/data/x/B", 50)], [], free);
    expect(over.blocked).toBe(true);
    expect(over.space.map((m) => m.text).join(" ")).toMatch(/\/data/);
    const fits = validateBatch([row("a", "/data/x/A", 60), row("b", "/data/x/B", null)], [], free);
    expect(fits.blocked).toBe(false);
    expect(fits.space.map((m) => m.text).join(" ")).toMatch(/size of 1/);
    const unknownFree = validateBatch([row("a", "/mnt/ext0/A", 60)], [], free);
    expect(unknownFree.blocked).toBe(false);
    expect(unknownFree.space.map((m) => m.text).join(" ")).toMatch(/free space/);
  });

  it("warns, without blocking, when internal storage would be left too tight", () => {
    // 100 fits by bytes (120 free), but the PS5 holds back more as it writes: 95 is what
    // is likely to fit.
    const free = new Map<string, number | null>([["/data", 120]]);
    const fits = new Map<string, number | null>([["/data", 95]]);
    const tight = validateBatch([row("a", "/data/x/A", 100)], [], free, fits);
    expect(tight.blocked).toBe(false);
    expect(tight.space.map((m) => m.key)).toEqual(["batch_space_tight"]);

    const fine = validateBatch([row("a", "/data/x/A", 90)], [], free, fits);
    expect(fine.space).toEqual([]);

    // A real shortfall is still the blocking message, not the warning.
    const over = validateBatch([row("a", "/data/x/A", 130)], [], free, fits);
    expect(over.blocked).toBe(true);
    expect(over.space.map((m) => m.key)).toEqual(["batch_space_short"]);
  });
});
