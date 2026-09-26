// Multi-part archive detection for the Upload screen.
//
// Sites publish a game as `Game.part01.zip` … `Game.part06.zip`. Unlike a
// spanned RAR set — where UnRAR opens the first volume and pulls its
// siblings in automatically — those zips are SEPARATE, self-contained
// archives, each holding a different slice of the game's files. Nothing
// links them, so uploading part01 sends only part01's contents and the
// user is left wondering where the rest went.
//
// We can't merge them for the user (each is its own upload), but we can
// stop them finding out afterwards.

/** A `.partN` marker split off an archive filename. */
export interface ArchivePartInfo {
  /** Filename with the `.partN` marker and extension removed. */
  stem: string;
  /** The part number as written (`01` → 1). */
  index: number;
  /** Lowercased extension without the dot: `zip` | `7z` | `rar`. */
  ext: string;
}

const PART_RE = /^(.*)\.part(\d+)\.(zip|7z|rar)$/i;

/** Parse `Game.part01.zip` → `{ stem: "Game", index: 1, ext: "zip" }`.
 *  Returns null when the name carries no multi-part marker. */
export function parseArchivePart(filename: string): ArchivePartInfo | null {
  const m = PART_RE.exec(filename);
  if (!m) return null;
  const index = Number.parseInt(m[2], 10);
  if (!Number.isFinite(index)) return null;
  return { stem: m[1], index, ext: m[3].toLowerCase() };
}

/** Given the chosen archive's filename and the names sitting beside it,
 *  return every sibling part of the SAME set, sorted by part number.
 *
 *  Matching is on the stem AND extension, so `Game.part1.zip` never pulls
 *  in an unrelated `Game.part1.rar` sitting in the same folder. Returns an
 *  empty array when the file isn't part of a set, or is the only part
 *  present — in both cases there is nothing useful to tell the user.
 */
export function siblingParts(
  filename: string,
  namesInFolder: readonly string[],
): ArchivePartInfo[] {
  const self = parseArchivePart(filename);
  if (!self) return [];
  const found = namesInFolder
    .map((n) => ({ name: n, info: parseArchivePart(n) }))
    .filter(
      (x): x is { name: string; info: ArchivePartInfo } =>
        x.info !== null &&
        x.info.ext === self.ext &&
        x.info.stem.toLowerCase() === self.stem.toLowerCase(),
    )
    .map((x) => x.info);
  // De-dupe by index: a folder holding both `G.part1.zip` and `G.part01.zip`
  // is malformed, and counting it twice would overstate the set.
  const byIndex = new Map<number, ArchivePartInfo>();
  for (const p of found) if (!byIndex.has(p.index)) byIndex.set(p.index, p);
  const all = [...byIndex.values()].sort((a, b) => a.index - b.index);
  return all.length > 1 ? all : [];
}

/** True when this archive is one of several parts that the user must
 *  upload individually — i.e. a zip/7z set. A `.rar` set is excluded:
 *  UnRAR genuinely does pull the sibling volumes in from the first one,
 *  so warning about it would be wrong. */
export function needsManualPartUploads(
  filename: string,
  namesInFolder: readonly string[],
): boolean {
  const self = parseArchivePart(filename);
  if (!self || self.ext === "rar") return false;
  return siblingParts(filename, namesInFolder).length > 1;
}

/** Rebuild the full paths of every part in the set, given the selected
 *  archive's full path and the filenames sitting beside it in its folder.
 *
 *  Ordered by part number, so feeding the result straight into the upload
 *  queue runs the parts in sequence.
 *
 *  The separator is read off the selected path rather than hardcoded: these
 *  paths go back to the host filesystem, and a Windows source
 *  (`C:\Users\...\Game.part01.zip`) has to stay a Windows path.
 *
 *  Filenames come from the folder listing rather than being synthesised
 *  from the stem, so the exact casing and zero-padding on disk survive.
 */
export function siblingPartPaths(
  selectedPath: string,
  namesInFolder: readonly string[],
): string[] {
  const sep = Math.max(
    selectedPath.lastIndexOf("/"),
    selectedPath.lastIndexOf("\\"),
  );
  if (sep <= 0) return [];
  const dir = selectedPath.slice(0, sep);
  const slash = selectedPath[sep];
  const parts = siblingParts(selectedPath.slice(sep + 1), namesInFolder);
  if (parts.length === 0) return [];

  const nameByIndex = new Map<number, string>();
  for (const n of namesInFolder) {
    const info = parseArchivePart(n);
    if (!info) continue;
    const inSet = parts.some(
      (p) =>
        p.index === info.index &&
        p.ext === info.ext &&
        p.stem.toLowerCase() === info.stem.toLowerCase(),
    );
    if (inSet && !nameByIndex.has(info.index)) nameByIndex.set(info.index, n);
  }

  return parts
    .map((p) => nameByIndex.get(p.index))
    .filter((n): n is string => n !== undefined)
    .map((n) => `${dir}${slash}${n}`);
}

/** Every file beside this one sharing its `.partN` stem, whatever the
 *  extension. `siblingParts` deliberately requires a matching extension —
 *  this does not, so a MISMATCHED set can be spotted and explained. */
function stemMatesAnyExt(
  self: ArchivePartInfo,
  namesInFolder: readonly string[],
): ArchivePartInfo[] {
  const byKey = new Map<string, ArchivePartInfo>();
  for (const n of namesInFolder) {
    const info = parseArchivePart(n);
    if (!info) continue;
    if (info.stem.toLowerCase() !== self.stem.toLowerCase()) continue;
    const key = `${info.ext}:${info.index}`;
    if (!byKey.has(key)) byKey.set(key, info);
  }
  return [...byKey.values()];
}

/** What, if anything, is worth telling the user about the set this archive
 *  belongs to. `null` means nothing notable. */
export type ArchiveSetIssue =
  | {
      /** A zip/7z set: each part is its own upload. */
      kind: "manual-parts";
      total: number;
    }
  | {
      /** The parts beside this one are a DIFFERENT archive format — so they
       *  are not siblings of the selected file at all, and uploading it
       *  delivers only its own contents. Nearly always means a download
       *  went wrong: one part was fetched from a different packaging of
       *  the game than the rest. */
      kind: "mixed-extensions";
      selectedExt: string;
      otherExt: string;
      otherCount: number;
      /** The volume the other-format set needs but does not have, e.g.
       *  `Game.part01.rar`. Null when that set is complete from part 1. */
      missingFirst: string | null;
    }
  | {
      /** The picked file is a volume of a natively-split set whose opening
       *  file is sitting right there — `Game.r00` beside `Game.rar`.
       *  Recoverable in one click: pick `entryName` instead. */
      kind: "pick-entry-volume";
      entryName: string;
    }
  | {
      /** A split set we cannot open at all: the volumes have to be joined
       *  back into one archive first. Covers `X.7z.001` (no opening file
       *  exists until the parts are joined) and spanned zips (`X.z01` +
       *  `X.zip`), which need a multi-disk reader we do not have. */
      kind: "split-unsupported";
      scheme: "numeric" | "zip";
      /** The joined archive the volumes reconstruct, when nameable. */
      entryName: string | null;
      volumeCount: number;
    };

/** Diagnose the set the chosen archive belongs to.
 *
 *  Checked before the plain multi-part case, because a mismatched set is
 *  the more urgent thing to say: telling someone "upload the other 10
 *  parts too" is wrong — and costs them hours — when those parts are a
 *  different format that their part 1 cannot open.
 */
export function diagnoseArchiveSet(
  filename: string,
  namesInFolder: readonly string[],
): ArchiveSetIssue | null {
  // Natively-split volumes first: these never reach `parseArchivePart`,
  // and a `.001` is not even recognised as an archive, so without this it
  // uploads as a plain file and the user gets a useless blob on the PS5.
  const vol = parseSplitVolume(filename);
  if (vol) {
    const present = new Set(namesInFolder.map((n) => n.toLowerCase()));
    // A rar volume set opens from its `.rar`, which UnRAR then follows.
    if (vol.scheme === "rar" && present.has(vol.entryName.toLowerCase())) {
      return { kind: "pick-entry-volume", entryName: vol.entryName };
    }
    const volumeCount = namesInFolder.filter((n) => {
      const v = parseSplitVolume(n);
      return v !== null && v.scheme === vol.scheme && v.entryName === vol.entryName;
    }).length;
    // A trailing run of digits is not distinctive on its own: `save.2024`
    // and `backup.1999` both look like `X.001`. Demand an actual SET — two
    // or more volumes sharing a base — before calling a numeric suffix a
    // split archive, so an ordinary file with a numeric extension is left
    // alone. `.z01`/`.r00` are distinctive enough to stand by themselves.
    if (vol.scheme === "numeric" && volumeCount < 2) return null;
    return {
      kind: "split-unsupported",
      // A rar set missing its `.rar` cannot be opened either; report it the
      // same way, naming the file they need.
      scheme: vol.scheme === "zip" ? "zip" : "numeric",
      entryName: vol.entryName || null,
      volumeCount,
    };
  }

  // A `.zip` with `.z01` volumes beside it is the LAST disk of a spanned
  // set. It opens — the central directory is in this file — but its entries
  // reference earlier disks the single-disk reader never reads.
  if (isSpannedZip(filename, namesInFolder)) {
    const stem = filename.replace(/\.zip$/i, "");
    return {
      kind: "split-unsupported",
      scheme: "zip",
      entryName: filename,
      volumeCount:
        namesInFolder.filter((n) =>
          new RegExp(`^${stem.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\.z\\d{2,}$`, "i").test(n),
        ).length + 1,
    };
  }

  const self = parseArchivePart(filename);
  if (!self) return null;

  const others = stemMatesAnyExt(self, namesInFolder).filter(
    (p) => p.ext !== self.ext,
  );
  if (others.length > 0) {
    // Report against the largest foreign set: the one the user has most of.
    const byExt = new Map<string, ArchivePartInfo[]>();
    for (const p of others) {
      const list = byExt.get(p.ext);
      if (list) list.push(p);
      else byExt.set(p.ext, [p]);
    }
    const [otherExt, group] = [...byExt.entries()].sort(
      (a, b) => b[1].length - a[1].length,
    )[0];
    const hasFirst = group.some((p) => p.index === 1);
    return {
      kind: "mixed-extensions",
      selectedExt: self.ext,
      otherExt,
      otherCount: group.length,
      missingFirst: hasFirst ? null : `${self.stem}.part01.${otherExt}`,
    };
  }

  if (needsManualPartUploads(filename, namesInFolder)) {
    return {
      kind: "manual-parts",
      total: siblingParts(filename, namesInFolder).length,
    };
  }
  return null;
}

// ── Native split schemes ────────────────────────────────────────────────
//
// `.partN.<ext>` (above) is what most repackers use, but each format also
// has its OWN split convention, and those look nothing alike:
//
//   Game.7z.001, Game.7z.002   7-Zip's byte-splitter. Also seen as
//                              Game.zip.001 / Game.rar.001 / Game.001 —
//                              the splitter doesn't care what it's cutting.
//   Game.z01, Game.z02 + .zip  Spanned zip. The central directory lives in
//                              the final `.zip`; earlier volumes are raw.
//   Game.r00, Game.r01 + .rar  Old-style RAR volumes. UnRAR follows these
//                              from the `.rar`, so that one is fine — the
//                              user just has to pick the `.rar`.
//
// None of these end in `.zip`/`.7z`/`.rar`, so `archiveFormat()` returns
// null and the file is treated as a PLAIN FILE — uploaded byte-for-byte to
// the PS5, where it is useless. That failure is silent, which makes it
// worse than the `.partN` case: no warning can fire because the file never
// registers as an archive at all.

/** A volume of a natively-split set. */
export interface SplitVolumeInfo {
  /** `numeric` → `X.7z.001`; `zip` → `X.z01`; `rar` → `X.r00`. */
  scheme: "numeric" | "zip" | "rar";
  /** Volume number as written (`001` → 1, `r00` → 0). */
  index: number;
  /** The file that OPENS the set, if the scheme has one we can name:
   *  `X.rar` for rar volumes, `X.zip` for a spanned zip, and for numeric
   *  splits the joined file the volumes reconstruct (`X.7z`). */
  entryName: string;
}

const NUMERIC_SPLIT_RE = /^(.*?)(\.(?:zip|7z|rar))?\.(\d{3,})$/i;
const ZIP_VOL_RE = /^(.*)\.z(\d{2,})$/i;
const RAR_VOL_RE = /^(.*)\.r(\d{2,})$/i;

/** Parse a natively-split volume name. Returns null for anything else —
 *  including the `.partN.<ext>` scheme, which `parseArchivePart` owns. */
export function parseSplitVolume(filename: string): SplitVolumeInfo | null {
  // `.partN.zip` is a whole archive, not a volume: let it through untouched.
  if (parseArchivePart(filename)) return null;

  const num = NUMERIC_SPLIT_RE.exec(filename);
  if (num) {
    const stem = num[1];
    const inner = num[2] ?? "";
    return {
      scheme: "numeric",
      index: Number.parseInt(num[3], 10),
      // `X.7z.001` rebuilds `X.7z`. A bare `X.001` gives no hint about the
      // joined format, so name the stem alone.
      entryName: `${stem}${inner}`,
    };
  }

  const z = ZIP_VOL_RE.exec(filename);
  if (z) {
    return {
      scheme: "zip",
      index: Number.parseInt(z[2], 10),
      entryName: `${z[1]}.zip`,
    };
  }

  const r = RAR_VOL_RE.exec(filename);
  if (r) {
    return {
      scheme: "rar",
      index: Number.parseInt(r[2], 10),
      entryName: `${r[1]}.rar`,
    };
  }

  return null;
}

/** True when this `.zip` is the last volume of a SPANNED set — i.e. there
 *  are `.z01`-style volumes beside it. The zip reader is single-disk, so
 *  such a `.zip` opens but its entries point at data we never see. */
export function isSpannedZip(
  filename: string,
  namesInFolder: readonly string[],
): boolean {
  const m = /^(.*)\.zip$/i.exec(filename);
  if (!m) return false;
  const stem = m[1].toLowerCase();
  return namesInFolder.some((n) => {
    const v = ZIP_VOL_RE.exec(n);
    return v !== null && v[1].toLowerCase() === stem;
  });
}
