// The FPKG converter: look at a game source, then build a package from it.
//
// The engine does the work (desktop, Docker or Android — the console is never
// involved); this module is the client's thin wrapper over its two endpoints.
// Both commands live in client/src-tauri/src/commands/ps5_engine.rs and the
// browser build reaches the same routes through lib/browserInvoke.ts.

import { invoke } from "../lib/invokeLogged";
import { useConvertPrefs, type AmprEmuBuild, type ZstdRuntime } from "../state/convertPrefs";

/** The game image filesystems the engine writes: UFS2 (`.ffpkg`, what ShadowMount+
 *  recommends), exFAT, or PFS (`.ffpfs`, experimental in ShadowMount+ 1.7). */
export type ImageFormat = "ffpkg" | "exfat" | "ffpfs";

/** Where an image build puts the image: this computer (the output folder), or straight into a
 *  PS5's folder over AVA1, sent as it is written and read back there. */
export type ImageDestination =
  | { kind: "local" }
  | { kind: "console"; addr: string; dir: string }
  /** A saved server (`source_id`, a Connections id) and an absolute folder on it. */
  | { kind: "server"; source_id: string; dir: string };
/** How hard a .ffpfsc is compressed. */
export type FfpfscLevel = "fast" | "balanced" | "smallest";

export interface FpkgCheck {
  name: string;
  ok: boolean;
  detail: string;
  /** On a finding about the game's own `ampr_emu.index`, which outputs it matters for:
   *  `packs_kept` only for one that keeps the game's asset packs (that one carries the index as
   *  it is), `source_as_is` for none (everything made from the game gets a fresh index).
   *  Absent: it matters whatever is made. */
  applies_to?: "packs_kept" | "source_as_is" | (string & {});
}

/** Whether a failed check is a warning for what is about to be made. `packsKept`: the output
 *  holds the game in the asset packs the source has it in (never for a game in no packs). */
export function checkMatters(check: FpkgCheck, packsKept: boolean): boolean {
  if (check.applies_to === "packs_kept") return packsKept;
  if (check.applies_to === "source_as_is") return false;
  return true;
}

/** One compression level's estimated package size and build time for a game. */
export interface FpkgEstimate {
  bytes: number;
  seconds: number;
}

export interface FpkgEstimates {
  fast: FpkgEstimate;
  balanced: FpkgEstimate;
  smallest: FpkgEstimate;
}

export interface FpkgInspection {
  /** "folder /games/x" or "exfat …/PPSA09519.exfat (64 KiB clusters…)". */
  source: string;
  files: number;
  bytes: number;
  content_id?: string | null;
  title?: string | null;
  required_firmware?: string | null;
  /** The lowest firmware the game runs on, from its modules' SDK stamps (e.g. "4.00" for a
   *  backport); the package declares it. Absent/null keeps the game's own. */
  min_firmware?: string | null;

  /** What the package is expected to cost, used for the free-space check. */
  planned_size: number;
  /** Bytes free where the output goes; null where the platform does not say. */
  output_free?: number | null;
  checks: FpkgCheck[];
  /** Present for a title that reads its files through libSceAmpr: whether a game image can
   *  carry AMPR LZ4 asset packs, which the game's own ampr_emu (fakelib) must serve. */
  ampr_packs?: AmprPacksReadiness | null;
  /** A game in zstd packs: the runtime it carries. */
  zstd_runtime?: ZstdRuntimeInfo | null;
  /** Which asset packs the game's files are in; absent/null for a plain game. A packed game
   *  can be written out as plain files again (`unpackPacks`). */
  packs?: "lz4" | "zstd" | null;
  /** How many of its executables are plain ELF files, which a build fake-signs. */
  unsigned_executables?: number;
  /** A PS4 game, update or add-on: it becomes an image or a folder, not a PS5 package. */
  ps4_game?: boolean;
  /** The version it says it is: param.json's contentVersion, or param.sfo's APP_VER. */
  content_version?: string | null;
  /** What a PS4 source is: a game, an update (`patch`, which goes into its game) or an add-on. */
  ps4_kind?: Ps4Kind | null;
  /** The PS4 title it is or belongs to (CUSA…): an update's or add-on's game. */
  ps4_title_id?: string | null;
  /** An add-on: whether it holds files (false: a licence, nothing to convert). */
  ps4_addon_data?: boolean | null;
  /** Inspected with an update merged in: what the merge is. */
  ps4_merge?: Ps4Merge | null;
  /** The add-on space (acSizeInGib) and size profile (appSizeInGib) param.json declares, which
   *  the console's package size check counts (lib/packageLimit). */
  param_sizes?: { ac_gib?: number | null; app_gib?: number | null } | null;
  /** The package's layout index less 8 bytes per 64 KiB of package (an estimate); 0 for a
   *  source that isn't a PS5 game. */
  layout_index_fixed?: number;
  /** Present when the game's fakelib holds backport libraries: what "Remove backport
   *  libraries" would leave out of an image or folder, and why it can't when it can't. */
  backport?: BackportInfo | null;
  /** A PS5 game in no asset packs that carries an AMPR emulator in its fakelib: what a package
   *  can leave out (`leaveOutAmprEmu` alone), and why not when it can't. */
  plain_ampr_emu?: PlainAmprEmu | null;
  /** Why this source could not be moved to the Trash after an image or folder is made of it,
   *  whatever is made (it is on a PS5 or a server, or the engine has no Trash); absent when
   *  it could. */
  delete_source_refusal?: string | null;
}

/** What a plain game carries of an AMPR emulator (the engine's `plain_emulator::PlainEmulator`). */
export interface PlainAmprEmu {
  /** Its libSceAmpr.sprx files, by path. */
  modules: string[];
  /** What it is: "ampr_emu 0.4.2.1", "an AMPR emulator", … */
  emulator: string;
  /** The game imports libSceAmpr: without the emulator, the PS5's own library serves it. */
  imports_ampr: boolean;
  /** Why it can't be left out; absent when it can. `not_an_emulator`: its libSceAmpr.sprx may
   *  be a backport's system library. `pack_files`, `index_lists_missing`: something there is
   *  read only through the emulator. */
  refusal?: "not_an_emulator" | "pack_files" | "index_lists_missing" | null;
  /** With a refusal, the facts behind it (a path, a file the index lists). */
  detail?: string | null;
}

/** A game's backport, as the check found it (the engine's `remove_backport::BackportReport`). */
export interface BackportInfo {
  /** The backport libraries in fakelib / fakelib2, by path. */
  libraries: string[];
  /** The emulators beside them, which are kept: `name` is AMPR, DLC, PlayGo or Other. */
  emulators: { path: string; name: string }[];
  /** Why they can't be left out whatever is made (the backport lowered eboot.bin's SDK). */
  blocked?: string | null;
  /** The game is in asset packs: they can be left out only when new zstd packs are made. */
  packed?: "lz4" | "zstd" | null;
}

/** Choices of one image or folder build that are off unless asked for. */
export interface BuildExtras {
  /** Leave the backport libraries in the game's fakelib out of what is made. */
  removeBackport?: boolean;
  /** Once the output is read back in full and in place, move the source to the Trash. The
   *  finished job's `commit_ack` says whether it went (`source_removed`, `source_note`). */
  deleteSource?: boolean;
  /** A game in asset packs is written as plain files again (not with `lz4`/`zstd`); for a
   *  folder build the source may then also be a game folder. */
  unpackPacks?: boolean;
  /** The AMPR emulator choice that goes with unpacking into a folder (a queued game's own;
   *  the settings when absent). An image build takes its own `amprEmu` argument. */
  amprEmu?: { add: boolean; build: AmprEmuBuild };
  /** A folder build: its files in AMPR LZ4 asset packs (not with zstd packs); the source may
   *  then also be a game folder. An image build takes its packs as its `lz4` argument. */
  lz4?: AmprLz4Options;
  /** The password of a `.rar` source read in place; never stored or logged. */
  archivePassword?: string;
}

export type Ps4Kind = "game" | "patch" | "addon";

/** A PS4 update merged into its game (the engine's `ps4_merge::MergeInfo`). */
export interface Ps4Merge {
  title_id: string;
  title?: string | null;
  game_version: string;
  update_version: string;
  /** Files of the game the update replaces, and files it adds. */
  replaced: number;
  added: number;
}

export interface AmprPacksReadiness {
  /** The ampr_emu version the game's libSceAmpr.sprx names, when it does. */
  runtime_version?: string | null;
  /** Why packs cannot be used for this game; null when they can. */
  refusal?: string | null;
  /** `ok`; `runtime` (installing ampr_emu 0.4.2.1 into the image makes packs possible);
   *  `no_ampr` (the game does not read through AMPR: no LZ4 packs; .ffpfsc is its compression,
   *  or zstd packs, which bring ampr-emu-ng whatever the game imports). */
  kind?: "ok" | "runtime" | "no_ampr";
  /** The game reads through AMPR and ships no emulator: it won't start from an image without one. */
  emulator_missing?: boolean;
}

/** Pack a game's files into asset packs first: AMPR LZ4 ones, or zstd ones with `zstdLevel`.
 *  Either goes into any image, compressed into a .ffpfsc or not. */
export interface AmprLz4Options {
  /** 1 (fastest) to 12 (smallest); the engine uses 9 when absent. */
  level?: number;
  /** A profile in drakmor's ampr_pack TOML format, in place of the built-in one. With zstd
   *  packs (`zstdLevel`) it only decides which files stay loose. */
  profileToml?: string;
  /** Put ampr_emu 0.4.2.1 into the image when the game's own is missing or too old. */
  installRuntime?: boolean;
  /** zstd packs at this level in place of LZ4 ones, in an image of any format; ampr-emu-ng goes
   *  in the image. */
  zstdLevel?: number;
  /** Which ampr-emu-ng goes in with zstd packs; Convert's setting when absent. */
  zstdRuntime?: ZstdRuntime;
}

/** zstd packs for a plain folder (the engine's `ampr_zstd`). */
export interface ZstdPacks {
  level: number;
  /** The newest ampr-emu-ng release (checked against GitHub's digest) or the tested one. */
  runtime?: ZstdRuntime;
  /** A profile in drakmor's ampr_pack TOML format deciding which files stay loose. */
  profileToml?: string;
}

/** The engine's `ampr_zstd` body: the level, the runtime choice (Convert's setting unless
 *  given) and a pack profile when one was picked. */
function amprZstd(level: number, runtime: ZstdRuntime | undefined, profileToml?: string) {
  return {
    level,
    runtime: runtime ?? useConvertPrefs.getState().zstdRuntime,
    profile_toml: profileToml,
  };
}

export interface FpkgBuildRequest {
  /** A game folder, an .exfat / .ffpkg / .ffpfsc image, or a .zip / .7z / .rar holding one. */
  source: string;
  /** Defaults to ~/Downloads/fpkgs in the engine. */
  outputDir?: string;
  contentId?: string;
  name?: string;
  /** How hard the Kraken encoder works; the engine defaults to balanced. */
  compression?: FpkgCompression;
  /** The package's minimum firmware, like "5.10"; the game's own when absent. A console older
   *  than the package's minimum refuses to install it. */
  firmware?: string;
  /** The connected console's firmware ("13.60"): without `firmware`, the package's size is
   *  checked against what this console opens (the engine refuses one it can't). */
  consoleFirmware?: string;
  /** The PlayGo language the package declares: one code (`de-DE`, …) for that one alone, `all`
   *  for every language; absent, the game's own (only its default when it names none). */
  language?: string;
  /** Re-read every byte to check the package; the engine's fast check when false or absent. */
  fullVerify?: boolean;
  /** When the package's name is taken in the output folder, name it `<name> (2).pkg` instead
   *  of refusing (Workshop's "Save each next to its source"). */
  keepBoth?: boolean;
  /** A PS4 update merged into the PS4 game `source` first: the PS4 package is the game at the
   *  update's version. */
  update?: string;
  /** A game in asset packs is packaged as plain files (#933). */
  unpackPacks?: boolean;
  /** With `unpackPacks`: the AMPR emulator choice (a queued game's own; the settings when
   *  absent), as an image or folder build takes it. */
  amprEmu?: { add: boolean; build: AmprEmuBuild };
  /** The AMPR emulator is left out of the package: with `unpackPacks` for a packed game, alone
   *  for a game in no packs whose inspection offers it (`plain_ampr_emu`). */
  leaveOutAmprEmu?: boolean;
}

/** `stored` = no compression: the fallback for a console that refuses a compressed package. */
export type FpkgCompression = "fast" | "balanced" | "smallest" | "stored";

/** A Workshop job running in the engine. `source`: what the user chose (an archive for a game
 *  unpacked from one). */
export interface WorkshopRun {
  job_id: string;
  kind: "package" | "image" | "folder" | "extract";
  source: string;
  started_at_ms: number;
}

/** How an archive gives its own space back while it unpacks (the engine's free-check). */
export interface ArchiveFreeCheck {
  /** `entries`: a .zip, freed file by file. `blocks`: a .7z of more than one block, block by
   *  block. `parts`: a multi-part .rar, part by part. `end`: anything else, where nothing is
   *  freed until the unpack is finished and checked. */
  mode: "entries" | "blocks" | "parts" | "end";
  /** The one game in it: a game folder, or a game image or package. */
  game: "folder" | "image";
  /** The pieces it is freed in: a .7z's blocks, a set's parts; else one. */
  parts: number;
  archive_bytes: number;
  unpacked: number;
  /** The archive is on the output folder's drive, so its space is room for the job. */
  same_drive: boolean;
  /** The free room the job needs in the output folder, and what is there (when known). */
  needed: number;
  free: number | null;
  /** How much less free room that is than with the archive kept to the end (0 on another
   *  drive). Absent from an engine older than this. */
  saves?: number;
}

export const fpkg = {
  /** With `update`, the PS4 game `source` with that update merged in, or why they don't go
   *  together. */
  inspect: (source: string, outputDir?: string, update?: string) =>
    invoke<FpkgInspection>("fpkg_inspect", { source, outputDir, update }),
  /** Starts a job; poll it with jobStatus from api/ps5. */
  build: (req: FpkgBuildRequest) =>
    invoke<{ job_id: string }>("fpkg_build", {
      source: req.source,
      outputDir: req.outputDir,
      contentId: req.contentId,
      name: req.name,
      compression: req.compression,
      firmware: req.firmware,
      consoleFirmware: req.consoleFirmware,
      language: req.language,
      fullVerify: req.fullVerify ?? false,
      keepBoth: req.keepBoth ?? false,
      update: req.update,
      // Only when unpacking: a plain package's call stays as it was.
      ...(req.unpackPacks
        ? {
            unpackPacks: true,
            addAmprEmu: req.amprEmu?.add ?? useConvertPrefs.getState().addAmprEmu,
            amprEmuBuild: req.amprEmu?.build ?? useConvertPrefs.getState().amprEmuBuild,
            ...(req.leaveOutAmprEmu ? { leaveOutAmprEmu: true } : {}),
          }
        : req.leaveOutAmprEmu
          ? // A plain game's emulator left out: nothing to unpack, no emulator choice.
            { leaveOutAmprEmu: true }
          : {}),
    }),
  /** Unpack a .zip / .7z / .rar into an unpack folder in `outputDir`, as a job; the job's
   *  `dest` is the game found inside. A RAR's password goes with it, never stored. With
   *  `freeArchive` the archive is DELETED as it unpacks, and the game is unpacked into a
   *  folder that stays (`<archive>.unpacked`): only after `freeCheck` and the user's yes. */
  extract: (source: string, outputDir?: string, password?: string, freeArchive?: boolean) =>
    invoke<{ job_id: string }>("fpkg_extract", {
      source,
      outputDir,
      password,
      ...(freeArchive ? { freeArchive: true } : {}),
    }),
  /** Whether an archive can be unpacked with its own space given back, and how. Changes
   *  nothing; rejects with the engine's plain reason when it can't be done. */
  freeCheck: (source: string, outputDir?: string, password?: string) =>
    invoke<ArchiveFreeCheck>("fpkg_extract_free_check", { source, outputDir, password }),
  /** Remove the unpack folder a game path from `extract` lies in. `kept`: the folder an
   *  unpack with `freeArchive` made, removed only once what was made from it is finished. */
  cleanupExtract: (path: string, kept?: boolean) =>
    invoke<{ ok: boolean }>("fpkg_extract_cleanup", { path, ...(kept ? { kept: true } : {}) }),
  /** Size and time at each compression level for a game: a separate request from the check,
   *  since sampling a large game on a slow drive takes a while. */
  estimate: (source: string, outputDir?: string) =>
    invoke<FpkgEstimates>("fpkg_estimate", { source, outputDir }),
  /** Delete a package this engine built (the Convert screen's Delete package); the engine
   *  refuses any other file. */
  deletePackage: (path: string) => invoke<{ ok: boolean }>("fpkg_delete", { path }),
  /** Workshop jobs the engine is running now, whoever started them. */
  runningJobs: () => invoke<WorkshopRun[]>("workshop_jobs", {}),
  /** Write a game folder as one .exfat image for ShadowMountPlus. Starts a job; the image
   *  lands in the output folder, named after the game folder. */
  /** One job: the image is planned, written (straight into a .ffpfsc when `compress`), read
   *  back and hash-checked. With `lz4`, the game's files are packed into asset packs first (any
   *  format, compressed or not), and read back through them too. */
  buildImage: (
    source: string,
    outputDir?: string,
    format: ImageFormat = "exfat",
    compress = false,
    lz4?: AmprLz4Options,
    /** The read-back check's KiB per read (Convert's preference); the engine's default
     *  when absent. */
    verifyChunkKib?: number,
    /** Read the whole image back (Convert's preference); the fast sampled check when false. */
    fullVerify = false,
    /** What to call the image instead of the source's own name (a game unpacked from an
     *  archive is named after the archive). */
    name?: string,
    /** How hard a .ffpfsc is compressed (with `compress`); the engine's Balanced when absent. */
    ffpfscLevel?: FfpfscLevel,
    /** When the image's name is taken in the output folder, add " (2)" instead of refusing. */
    keepBoth = false,
    /** The AMPR emulator choice for this build (a queued game's own); the settings when absent. */
    amprEmu?: { add: boolean; build: AmprEmuBuild },
    /** Straight to a PS5's folder, never stored on this computer; `outputDir` when absent. */
    destination?: ImageDestination,
    /** A PS4 update merged into the PS4 game `source`: the image is the game at its version. */
    update?: string,
    extra?: BuildExtras,
  ) =>
    invoke<{ job_id: string }>("exfat_build", {
      unpackPacks: extra?.unpackPacks || undefined,
      update,
      removeBackport: extra?.removeBackport || undefined,
      deleteSource: extra?.deleteSource || undefined,
      archivePassword: extra?.archivePassword || undefined,
      source,
      outputDir,
      format,
      compress,
      amprZstd: lz4?.zstdLevel ? amprZstd(lz4.zstdLevel, lz4.zstdRuntime, lz4.profileToml) : undefined,
      amprLz4: lz4 && !lz4.zstdLevel
        ? { level: lz4.level, profile_toml: lz4.profileToml, install_runtime: lz4.installRuntime ?? false }
        : undefined,
      verifyChunkKib,
      fullVerify,
      name,
      ffpfscLevel: compress ? ffpfscLevel : undefined,
      keepBoth,
      addAmprEmu: amprEmu?.add ?? useConvertPrefs.getState().addAmprEmu,
      amprEmuBuild: amprEmu?.build ?? useConvertPrefs.getState().amprEmuBuild,
      destination,
    }),
  /** Unpack a game image (.exfat, .ffpkg, .ffpfs, .ffpfsc) or a package made with the debug
   *  key into a plain folder in the output folder, named after the source (or `name`). One
   *  job: written, read back (sizes, and SHA-256 of a sample or of every file), then given its
   *  name; the job's `dest` is the folder. */
  buildFolder: (
    source: string,
    outputDir?: string,
    name?: string,
    fullVerify = false,
    /** Only this file or folder of the image, by its path inside it (the image browser's
     *  Save): a folder becomes a folder named after it, a file a file. */
    entry?: string,
    /** When the folder's name is taken in the output folder, add " (2)" instead of refusing. */
    keepBoth = false,
    /** Write the game with its files in zstd packs (ampr-emu-ng goes in too); the source may
     *  then also be a game folder. */
    zstd?: ZstdPacks,
    /** A PS4 update merged into the PS4 game `source` (which may then also be a folder). */
    update?: string,
    /** Straight to a folder on a saved server, never stored on this computer; `outputDir`
     *  when absent. */
    destination?: ImageDestination,
    extra?: BuildExtras,
  ) =>
    invoke<{ job_id: string }>("folder_build", {
      ...(extra?.unpackPacks
        ? {
            unpackPacks: true,
            addAmprEmu: extra.amprEmu?.add ?? useConvertPrefs.getState().addAmprEmu,
            amprEmuBuild: extra.amprEmu?.build ?? useConvertPrefs.getState().amprEmuBuild,
          }
        : {}),
      update,
      removeBackport: extra?.removeBackport || undefined,
      deleteSource: extra?.deleteSource || undefined,
      archivePassword: extra?.archivePassword || undefined,
      destination,
      source,
      outputDir,
      name,
      fullVerify,
      entry,
      keepBoth,
      amprZstd: zstd ? amprZstd(zstd.level, zstd.runtime, zstd.profileToml) : undefined,
      amprLz4: extra?.lz4
        ? {
            level: extra.lz4.level,
            profile_toml: extra.lz4.profileToml,
            install_runtime: extra.lz4.installRuntime ?? false,
          }
        : undefined,
    }),
  /** Write an edited COPY of a game image (#479): the image's files with `edits` applied,
   *  as the same kind of image (.exfat, .ffpkg, .ffpfs or .ffpfsc), read back and checked,
   *  named `<name>-edited` next to it unless `outputDir`/`name` say otherwise. The image
   *  itself is only read. One job, like an image build. */
  editImage: (source: string, edits: ImageEdit[], outputDir?: string, name?: string, fullVerify = false) =>
    invoke<{ job_id: string }>("exfat_build", { source, outputDir, name, fullVerify, edits }),
  /** Apply `edits` to the image ITSELF (an uncompressed .exfat, .ffpkg or .ffpfs on this
   *  computer), with an undo journal beside it so an interrupted change is undone; every changed
   *  file is read back and any failure restores the image. Refused, with the reason, when the
   *  changes don't fit the image's free space (it never grows). One job. */
  editImageInPlace: (source: string, edits: ImageEdit[], fullVerify = false) =>
    invoke<{ job_id: string }>("exfat_build", { source, fullVerify, edits, inPlace: true }),
  /** Whether `edits` can go into the image itself, without writing anything. */
  checkEditInPlace: (path: string, edits: ImageEdit[]) =>
    invoke<InPlaceCheck>("image_edit_check", { path, edits }),
  /** Rebuild a libSceAmpr game folder's `ampr_emu.index` where it is, on this computer or the
   *  PS5 (`ps5://`), the game's own kept once as `ampr_emu.index.bak` (#621). A job: its stage
   *  is `list`, `check` or `write`; done, `dest` is the index and `commit_ack` the
   *  {@link AmprIndexOutcome}. */
  /** The AMPR emulator added to a game where it stands: in a folder, or as a backport folder for
   *  an image on the PS5. `build`: which one (Convert's setting when absent). */
  addAmprEmu: (source: string, build?: AmprEmuBuild) =>
    invoke<AmprEmuOutcome>("fpkg_ampr_emu", { source, build: build ?? useConvertPrefs.getState().amprEmuBuild }),
  /** A game in zstd packs gets the newest ampr-emu-ng where it stands (a backport folder for an
   *  image on the PS5). */
  updateZstdRuntime: (source: string) => invoke<ZstdRuntimeOutcome>("fpkg_zstd_runtime", { source }),
  /** Whether ShadowMount+ on `addr` swaps zstd-packed games' runtime for one that won't run them. */
  smpEmulator: (addr: string) => invoke<SmpEmulatorCheck>("ps5_smp_emulator", { addr }),
  rebuildAmprIndex: (source: string) => invoke<{ job_id: string }>("fpkg_ampr_index", { source }),
  /** An edited copy of a game image whose one change is a regenerated `ampr_emu.index` (#621),
   *  named `<name>-edited` next to it (`<name>-edited (2)` when a rebuild already made one).
   *  The image itself is only read; a .ffpfsc is refused. */
  rebuildImageAmprIndex: (source: string) =>
    invoke<{ job_id: string }>("exfat_build", { source, edits: [], rebuildAmprIndex: true, keepBoth: true }),
};

/** What adding the AMPR emulator to a game did. */
/** What runs a game in zstd packs (the engine's `ampr_pack::ng::ZstdRuntime`). */
export interface ZstdRuntimeInfo {
  module?: string | null;
  kind: "ampr-emu-ng" | "ampr_emu" | "unknown" | "missing";
  version?: string | null;
  /** Missing, drakmor's (no zstd), or ampr-emu-ng older than 0.3.4: a newer one fixes it. */
  outdated: boolean;
  /** ampr-emu-ng 0.4.0, which refuses very large games (its 48 MiB index cap): 0.4.1 starts
   *  them. Absent from an engine older than this field. */
  capped?: boolean;
}

/** What updating a zstd-packed game's runtime did. */
export interface ZstdRuntimeOutcome {
  kind: "folder" | "backport";
  folder: string;
  written: string[];
  /** `ampr-emu-ng 0.3.2`, `ampr_emu 0.4.2.1`, `unknown`, `none`. */
  from: string;
  /** `ampr-emu-ng 0.4.0`. */
  to: string;
  already: boolean;
  rescanned: boolean;
  /** Kraken Mount runs on the console and puts this file over every game's runtime: the game
   *  starts with it, not with the one written. */
  overridden_by?: string | null;
}

/** ShadowMount+'s emulator updates on a console, as they touch zstd-packed games. */
export interface SmpEmulatorCheck {
  updates_on: boolean;
  /** The libSceAmpr.sprx it puts into every game in place of the game's own. */
  path?: string | null;
  kind?: "ampr-emu-ng" | "ampr_emu" | "unknown" | null;
  version?: string | null;
  /** zstd-packed games get that file and it won't start them. */
  breaks_zstd: boolean;
  /** Whose folder: ShadowMount+'s emulators, or Kraken Mount's ampr/ (absent from an older
   *  engine: ShadowMount+). */
  mounter?: "shadowmount" | "kraken";
}

export interface AmprEmuOutcome {
  kind: "folder" | "backport";
  folder: string;
  written: string[];
  rescanned: boolean;
}

/** What a folder's index rebuild did. */
export interface AmprIndexOutcome {
  /** Files the new index lists. */
  files: number;
  /** It lists other files or sizes than the game's own did (or there was none). */
  differed: boolean;
  had_index: boolean;
  /** Where the game's own index was kept, when this rebuild kept it. */
  backup?: string | null;
}

/** One change for an edited copy: delete a file or folder, put a file (or a folder's
 *  contents) from this computer at `path`, replacing what is there, rename or move a file or
 *  folder (`from` and `to` are both paths inside the image; nothing may be at `to`), or make a
 *  new empty folder. The engine applies them in order. */
/** What `/api/image/edit/check` says about applying edits to the image itself. */
export interface InPlaceCheck {
  in_place: boolean;
  /** Why not, for the user (when `in_place` is false). */
  reason?: string;
  /** The image is in use right now. */
  busy: boolean;
  filesystem?: "exfat" | "ufs2" | "pfs";
  needed_bytes?: number;
  free_bytes?: number;
  journal_bytes?: number;
}

export type ImageEdit =
  | { op: "delete"; path: string }
  | { op: "put"; path: string; from: string }
  | { op: "rename"; from: string; to: string }
  | { op: "mkdir"; path: string };
