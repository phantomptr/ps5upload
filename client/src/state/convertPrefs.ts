import { create } from "zustand";

import type { FpkgCompression, ImageFormat } from "../api/fpkg";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/**
 * What Convert remembers between sessions, per viewer: where packages go and how hard to
 * compress them. Reads and writes never throw (the browser build may deny storage), and a
 * remembered folder is kept as-is even if it has since gone away: the engine's check says so.
 */
const KEY_DIR = "ps5upload.convert.output_dir";
const KEY_LEVEL = "ps5upload.convert.compression";
const KEY_VERIFY_CHUNK = "ps5upload.convert.verify_chunk_kib";
const KEY_FULL_VERIFY = "ps5upload.convert.full_verify";
const KEY_MAKE = "ps5upload.convert.make";
const KEY_IMAGE_FORMAT = "ps5upload.convert.image_format";
const KEY_IMAGE_COMPRESS = "ps5upload.convert.image_compress";
const IMAGE_FORMATS: readonly ImageFormat[] = ["ffpkg", "exfat", "ffpfs"];
const LEVELS: readonly FpkgCompression[] = ["fast", "balanced", "smallest"];
/** How much of a file a game image's read-back check reads per call, in KiB. The engine
 *  takes 1024 to 65536; these are the choices offered. */
export const VERIFY_CHUNK_KIB: readonly number[] = [1024, 4096, 8192, 16384, 32768];
/** The fastest in measurement (reading plus hashing on all cores), and the engine's own
 *  default. */
export const DEFAULT_VERIFY_CHUNK_KIB = 8192;

interface ConvertPrefs {
  outputDir: string;
  compression: FpkgCompression;
  /** A game image's read-back check: KiB per read. */
  verifyChunkKib: number;
  /** Re-read every byte of the output to check it. Off (the default) is the fast check:
   *  structure, small files and a random sample. */
  fullVerify: boolean;
  /** What Convert makes, and which image: remembered so leaving the screen mid-build and
   *  coming back shows the build that is running, not the default. */
  make: "pkg" | "image";
  imageFormat: ImageFormat;
  imageCompress: boolean;
  setMake: (m: "pkg" | "image") => void;
  setImageChoice: (format: ImageFormat, compress: boolean) => void;
  setOutputDir: (dir: string) => void;
  setCompression: (c: FpkgCompression) => void;
  setVerifyChunkKib: (kib: number) => void;
  setFullVerify: (full: boolean) => void;
}

function storedLevel(): FpkgCompression {
  const v = safeGetItem(KEY_LEVEL);
  return LEVELS.includes(v as FpkgCompression) ? (v as FpkgCompression) : "balanced";
}

export function parseVerifyChunkKib(v: string | null): number {
  const n = Number(v);
  return VERIFY_CHUNK_KIB.includes(n) ? n : DEFAULT_VERIFY_CHUNK_KIB;
}

export function parseFullVerify(v: string | null): boolean {
  return v === "1";
}

export function parseMake(v: string | null): "pkg" | "image" {
  return v === "image" ? "image" : "pkg";
}

export function parseImageFormat(v: string | null): ImageFormat {
  return IMAGE_FORMATS.includes(v as ImageFormat) ? (v as ImageFormat) : "ffpkg";
}

export const useConvertPrefs = create<ConvertPrefs>((set) => ({
  outputDir: safeGetItem(KEY_DIR) ?? "",
  compression: storedLevel(),
  verifyChunkKib: parseVerifyChunkKib(safeGetItem(KEY_VERIFY_CHUNK)),
  fullVerify: parseFullVerify(safeGetItem(KEY_FULL_VERIFY)),
  make: parseMake(safeGetItem(KEY_MAKE)),
  imageFormat: parseImageFormat(safeGetItem(KEY_IMAGE_FORMAT)),
  imageCompress: safeGetItem(KEY_IMAGE_COMPRESS) === "1",
  setMake: (m) => {
    safeSetItem(KEY_MAKE, m);
    set({ make: m });
  },
  setImageChoice: (format, compress) => {
    safeSetItem(KEY_IMAGE_FORMAT, format);
    safeSetItem(KEY_IMAGE_COMPRESS, compress ? "1" : "0");
    set({ imageFormat: format, imageCompress: compress });
  },
  setOutputDir: (dir) => {
    safeSetItem(KEY_DIR, dir);
    set({ outputDir: dir });
  },
  setCompression: (c) => {
    safeSetItem(KEY_LEVEL, c);
    set({ compression: c });
  },
  setVerifyChunkKib: (kib) => {
    const v = VERIFY_CHUNK_KIB.includes(kib) ? kib : DEFAULT_VERIFY_CHUNK_KIB;
    safeSetItem(KEY_VERIFY_CHUNK, String(v));
    set({ verifyChunkKib: v });
  },
  setFullVerify: (full) => {
    safeSetItem(KEY_FULL_VERIFY, full ? "1" : "0");
    set({ fullVerify: full });
  },
}));
