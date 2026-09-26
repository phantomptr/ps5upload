// The FPKG converter: look at a game source, then build a package from it.
//
// The engine does the work (desktop, Docker or Android — the console is never
// involved); this module is the client's thin wrapper over its two endpoints.
// Both commands live in client/src-tauri/src/commands/ps5_engine.rs and the
// browser build reaches the same routes through lib/browserInvoke.ts.

import { invoke } from "../lib/invokeLogged";

export interface FpkgCheck {
  name: string;
  ok: boolean;
  detail: string;
}

export interface FpkgInspection {
  /** "folder /games/x" or "exfat …/PPSA09519.exfat (64 KiB clusters…)". */
  source: string;
  files: number;
  bytes: number;
  content_id?: string | null;
  title?: string | null;
  required_firmware?: string | null;
  /** What the package is expected to cost, used for the free-space check. */
  planned_size: number;
  /** Bytes free where the output goes; null where the platform does not say. */
  output_free?: number | null;
  checks: FpkgCheck[];
}

export interface FpkgBuildRequest {
  /** A game folder, or an .exfat / .ffpkg mount image. */
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
    }),
  /** Compress an .exfat / .ffpkg game image into a .ffpfsc for ShadowMountPlus.
   *  Starts a job; the output lands next to the source unless outputDir says otherwise. */
  compress: (source: string, outputDir?: string) =>
    invoke<{ job_id: string }>("ffpfsc_compress", { source, outputDir }),
};
