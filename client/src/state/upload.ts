import { create } from "zustand";
import {
  inspectFolder,
  type FolderInspectResult,
  type WrappedGameHint,
} from "../api/ps5";

/**
 * Detected source kind. Drives which options the Upload screen shows.
 *
 *   file        — plain file; needs only a destination.
 *   image       — disk image (.exfat or .ffpkg); adds an optional
 *                 post-upload copy so an auto-mounting tool on the
 *                 PS5 picks it up. (.exfat = exFAT filesystem;
 *                 .ffpkg = UFS2 filesystem; mount path is identical
 *                 as far as our upload flow cares.)
 *   folder      — plain folder; adds exclude rules.
 *   game-folder — folder with parseable sce_sys/param.*; adds the
 *                 meta card above excludes.
 */
export type SourceKind = "file" | "image" | "folder" | "game-folder";

export interface PickedSource {
  kind: SourceKind;
  path: string;
  /** Populated for `folder` and `game-folder`. Null otherwise. */
  meta: FolderInspectResult | null;
  /** Present when the root isn't a game but a single child subdir is. */
  wrappedHint: WrappedGameHint | null;
}

/** Exclude model. Default is "all" — upload everything, no filters.
 *  The user can switch to "rules" to enable the curated junk-file
 *  patterns (plus anything they add). Mirrors the two-button radio at
 *  the top of the excludes section. */
export type ExcludeMode = "all" | "rules";

export interface ExcludeRule {
  pattern: string;
  enabled: boolean;
}

export interface UploadState {
  source: PickedSource | null;
  detecting: boolean;
  detectError: string | null;

  mountAfterUpload: boolean;

  destinationVolume: string | null;
  destinationSubpath: string;

  excludeMode: ExcludeMode;
  excludes: ExcludeRule[];

  pickFile(path: string): Promise<void>;
  pickFolder(path: string): Promise<void>;
  reset(): void;

  setMountAfterUpload(on: boolean): void;
  setDestination(volume: string | null, subpath?: string): void;
  setExcludeMode(mode: ExcludeMode): void;
  toggleExclude(pattern: string): void;
  addExclude(pattern: string): void;
  removeExclude(pattern: string): void;
}

const defaultExcludes: ExcludeRule[] = [
  { pattern: ".DS_Store", enabled: true },
  { pattern: "Thumbs.db", enabled: true },
  { pattern: "desktop.ini", enabled: true },
  { pattern: "*.esbak", enabled: true },
  { pattern: ".git/**", enabled: true },
];

/** Lowercase extension test covering both disk-image types we
 *  support — `.exfat` (exFAT) and `.ffpkg` (UFS2). */
function isImagePath(path: string): boolean {
  const p = path.toLowerCase();
  return p.endsWith(".exfat") || p.endsWith(".ffpkg");
}

export const useUploadStore = create<UploadState>((set, get) => ({
  source: null,
  detecting: false,
  detectError: null,
  mountAfterUpload: false,
  destinationVolume: null,
  destinationSubpath: "ps5upload",
  excludeMode: "all",
  excludes: defaultExcludes,

  async pickFile(path) {
    const kind: SourceKind = isImagePath(path) ? "image" : "file";
    set({
      source: { kind, path, meta: null, wrappedHint: null },
      detecting: false,
      detectError: null,
      // Default mount-after-upload ON when the user picks a disk
      // image — routing it through the auto-mount flow is the whole
      // reason they picked an .exfat/.ffpkg over a plain folder
      // upload.
      mountAfterUpload: kind === "image" ? true : get().mountAfterUpload,
    });
  },

  async pickFolder(path) {
    // Optimistic set so the UI can render "Inspecting <path>…" immediately.
    set({
      source: { kind: "folder", path, meta: null, wrappedHint: null },
      detecting: true,
      detectError: null,
    });
    try {
      const inspection = await inspectFolder(path);
      const isGame = inspection.result.meta_source !== "none";
      set({
        source: {
          kind: isGame ? "game-folder" : "folder",
          path,
          meta: inspection.result,
          wrappedHint: inspection.wrapped_hint,
        },
        detecting: false,
        detectError: null,
      });
    } catch (e) {
      set({
        detecting: false,
        detectError: e instanceof Error ? e.message : String(e),
      });
    }
  },

  reset: () =>
    set({
      source: null,
      detecting: false,
      detectError: null,
      mountAfterUpload: false,
    }),

  setMountAfterUpload: (mountAfterUpload) => set({ mountAfterUpload }),

  setDestination: (destinationVolume, destinationSubpath) =>
    set((s) => ({
      destinationVolume,
      destinationSubpath: destinationSubpath ?? s.destinationSubpath,
    })),

  setExcludeMode: (excludeMode) => set({ excludeMode }),

  toggleExclude: (pattern) =>
    set((s) => ({
      excludes: s.excludes.map((e) =>
        e.pattern === pattern ? { ...e, enabled: !e.enabled } : e
      ),
    })),

  addExclude: (pattern) =>
    set((s) => {
      const trimmed = pattern.trim();
      if (!trimmed || s.excludes.some((e) => e.pattern === trimmed)) return s;
      return { excludes: [...s.excludes, { pattern: trimmed, enabled: true }] };
    }),

  removeExclude: (pattern) =>
    set((s) => ({
      excludes: s.excludes.filter((e) => e.pattern !== pattern),
    })),
}));
