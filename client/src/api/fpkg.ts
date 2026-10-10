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
}

/** Pack a game image's files into AMPR LZ4 asset packs first (exFAT images only). */
export interface AmprLz4Options {
  /** 1 (fastest) to 12 (smallest); the engine uses 9 when absent. */
  level?: number;
  /** A profile in drakmor's ampr_pack TOML format, in place of the built-in one. */
  profileToml?: string;
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
}

export type FpkgCompression = "fast" | "balanced" | "smallest";

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
  compress: (source: string, outputDir?: string) =>
    invoke<{ job_id: string }>("ffpfsc_compress", { source, outputDir }),
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
  ) =>
    invoke<{ job_id: string }>("exfat_build", {
      source,
      outputDir,
      format,
      compress,
      amprLz4: lz4 ? { level: lz4.level, profile_toml: lz4.profileToml } : undefined,
    }),
};
