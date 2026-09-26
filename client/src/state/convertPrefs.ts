import { create } from "zustand";

import type { FpkgCompression } from "../api/fpkg";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/**
 * What Convert remembers between sessions, per viewer: where packages go and how hard to
 * compress them. Reads and writes never throw (the browser build may deny storage), and a
 * remembered folder is kept as-is even if it has since gone away: the engine's check says so.
 */
const KEY_DIR = "ps5upload.convert.output_dir";
const KEY_LEVEL = "ps5upload.convert.compression";
const LEVELS: readonly FpkgCompression[] = ["fast", "balanced", "smallest"];

interface ConvertPrefs {
  outputDir: string;
  compression: FpkgCompression;
  setOutputDir: (dir: string) => void;
  setCompression: (c: FpkgCompression) => void;
}

function storedLevel(): FpkgCompression {
  const v = safeGetItem(KEY_LEVEL);
  return LEVELS.includes(v as FpkgCompression) ? (v as FpkgCompression) : "balanced";
}

export const useConvertPrefs = create<ConvertPrefs>((set) => ({
  outputDir: safeGetItem(KEY_DIR) ?? "",
  compression: storedLevel(),
  setOutputDir: (dir) => {
    safeSetItem(KEY_DIR, dir);
    set({ outputDir: dir });
  },
  setCompression: (c) => {
    safeSetItem(KEY_LEVEL, c);
    set({ compression: c });
  },
}));
