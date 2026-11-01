// The FPKG converter: look at a game source, then build a package from it.
//
// The engine does the work (desktop, Docker or Android — the console is never
// involved); this module is the client's thin wrapper over its two endpoints.
// Both commands live in client/src-tauri/src/commands/ps5_engine.rs and the
// browser build reaches the same routes through lib/browserInvoke.ts.

import { invoke } from "../lib/invokeLogged";

/** The game image filesystems the engine writes: UFS2 (`.ffpkg`, what ShadowMount+
 *  recommends), exFAT, or PFS (`.ffpfs`, experimental in ShadowMount+ 1.7). */
export type ImageFormat = "ffpkg" | "exfat" | "ffpfs";
/** How hard a .ffpfsc is compressed. */
export type FfpfscLevel = "fast" | "balanced" | "smallest";

export interface FpkgCheck {
  name: string;
  ok: boolean;
  detail: string;
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
}

export interface AmprPacksReadiness {
  /** The ampr_emu version the game's libSceAmpr.sprx names, when it does. */
  runtime_version?: string | null;
  /** Why packs cannot be used for this game; null when they can. */
  refusal?: string | null;
  /** `ok`; `runtime` (installing ampr_emu 0.4.2.1 into the image makes packs possible);
   *  `no_ampr` (the game does not read through AMPR: .ffpfsc is its compression). */
  kind?: "ok" | "runtime" | "no_ampr";
}

/** Pack a game image's files into AMPR LZ4 asset packs first (exFAT images only). */
export interface AmprLz4Options {
  /** 1 (fastest) to 12 (smallest); the engine uses 9 when absent. */
  level?: number;
  /** A profile in drakmor's ampr_pack TOML format, in place of the built-in one. */
  profileToml?: string;
  /** Put ampr_emu 0.4.2.1 into the image when the game's own is missing or too old. */
  installRuntime?: boolean;
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
  /** The one PlayGo language the package declares (`de-DE`, …); every language when absent. */
  language?: string;
  /** Re-read every byte to check the package; the engine's fast check when false or absent. */
  fullVerify?: boolean;
  /** When the package's name is taken in the output folder, name it `<name> (2).pkg` instead
   *  of refusing (Workshop's "Save each next to its source"). */
  keepBoth?: boolean;
}

/** `stored` = no compression: the fallback for a console that refuses a compressed package. */
export type FpkgCompression = "fast" | "balanced" | "smallest" | "stored";

export const fpkg = {
  inspect: (source: string, outputDir?: string) =>
    invoke<FpkgInspection>("fpkg_inspect", { source, outputDir }),
  /** Starts a job; poll it with jobStatus from api/ps5. */
  build: (req: FpkgBuildRequest) =>
    invoke<{ job_id: string }>("fpkg_build", {
      source: req.source,
      outputDir: req.outputDir,
      contentId: req.contentId,
      name: req.name,
      compression: req.compression,
      firmware: req.firmware,
      language: req.language,
      fullVerify: req.fullVerify ?? false,
      keepBoth: req.keepBoth ?? false,
    }),
  /** Unpack a .zip / .7z / .rar into an unpack folder in `outputDir`, as a job; the job's
   *  `dest` is the game found inside. A RAR's password goes with it, never stored. */
  extract: (source: string, outputDir?: string, password?: string) =>
    invoke<{ job_id: string }>("fpkg_extract", { source, outputDir, password }),
  /** Remove the unpack folder a game path from `extract` lies in. */
  cleanupExtract: (path: string) => invoke<{ ok: boolean }>("fpkg_extract_cleanup", { path }),
  /** Size and time at each compression level for a game: a separate request from the check,
   *  since sampling a large game on a slow drive takes a while. */
  estimate: (source: string, outputDir?: string) =>
    invoke<FpkgEstimates>("fpkg_estimate", { source, outputDir }),
  /** Delete a package this engine built (the Convert screen's Delete package); the engine
   *  refuses any other file. */
  deletePackage: (path: string) => invoke<{ ok: boolean }>("fpkg_delete", { path }),
  /** Compress an .exfat / .ffpkg game image into a .ffpfsc for ShadowMountPlus.
   *  Starts a job; the output lands next to the source unless outputDir says otherwise. */
  compress: (source: string, outputDir?: string, fullVerify = false) =>
    invoke<{ job_id: string }>("ffpfsc_compress", { source, outputDir, fullVerify }),
  /** Write a game folder as one .exfat image for ShadowMountPlus. Starts a job; the image
   *  lands in the output folder, named after the game folder. */
  /** One job: the image is planned, written (straight into a .ffpfsc when `compress`), read
   *  back and hash-checked. With `lz4`, the game's files are packed into AMPR LZ4 asset packs
   *  first (exFAT, not compressed). */
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
  ) =>
    invoke<{ job_id: string }>("exfat_build", {
      source,
      outputDir,
      format,
      compress,
      amprLz4: lz4
        ? { level: lz4.level, profile_toml: lz4.profileToml, install_runtime: lz4.installRuntime ?? false }
        : undefined,
      verifyChunkKib,
      fullVerify,
      name,
      ffpfscLevel: compress ? ffpfscLevel : undefined,
      keepBoth,
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
  ) =>
    invoke<{ job_id: string }>("folder_build", { source, outputDir, name, fullVerify, entry, keepBoth }),
  /** Write an edited COPY of a game image (#479): the image's files with `edits` applied,
   *  as the same kind of image (.exfat, .ffpkg, .ffpfs or .ffpfsc), read back and checked,
   *  named `<name>-edited` next to it unless `outputDir`/`name` say otherwise. The image
   *  itself is only read. One job, like an image build. */
  editImage: (source: string, edits: ImageEdit[], outputDir?: string, name?: string, fullVerify = false) =>
    invoke<{ job_id: string }>("exfat_build", { source, outputDir, name, fullVerify, edits }),
  /** Rebuild a libSceAmpr game folder's `ampr_emu.index` where it is, on this computer or the
   *  PS5 (`ps5://`), the game's own kept once as `ampr_emu.index.bak` (#621). A job: its stage
   *  is `list`, `check` or `write`; done, `dest` is the index and `commit_ack` the
   *  {@link AmprIndexOutcome}. */
  rebuildAmprIndex: (source: string) => invoke<{ job_id: string }>("fpkg_ampr_index", { source }),
  /** An edited copy of a game image whose one change is a regenerated `ampr_emu.index` (#621),
   *  named `<name>-edited` next to it. The image itself is only read; a .ffpfsc is refused. */
  rebuildImageAmprIndex: (source: string) =>
    invoke<{ job_id: string }>("exfat_build", { source, edits: [], rebuildAmprIndex: true }),
};

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

/** One change for an edited copy: delete a file or folder, or put a file (or a folder's
 *  contents) from this computer at `path`, replacing what is there. */
export type ImageEdit =
  | { op: "delete"; path: string }
  | { op: "put"; path: string; from: string };
