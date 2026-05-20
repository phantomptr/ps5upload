import { create } from "zustand";
import {
  inspectFolder,
  zipInspect,
  type FolderInspectResult,
  type WrappedGameHint,
  type ZipInspect,
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
 *   archive     — a `.zip` game dump; the engine decompresses on the host
 *                 and streams the files in, so they land already extracted
 *                 on the PS5. Adds a "compressed → extracted" card and the
 *                 same exclude rules as a folder.
 */
export type SourceKind = "file" | "image" | "folder" | "game-folder" | "archive";

export interface PickedSource {
  kind: SourceKind;
  path: string;
  /** Populated for `folder` and `game-folder`. Null otherwise. */
  meta: FolderInspectResult | null;
  /** Present when the root isn't a game but a single child subdir is. */
  wrappedHint: WrappedGameHint | null;
  /** Populated for `archive` — the zip's central-directory preview. */
  zipInfo: ZipInspect | null;
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
  /** Mount the image read-only when mount-after-upload runs. Default
   *  true — the safer choice: the PS5 can't accidentally write
   *  save-data into the image and corrupt it on next mount. Users
   *  who want save-data to persist back into the image (rare; mostly
   *  for editable homebrew scratchpads) flip this off explicitly. */
  mountReadOnly: boolean;

  destinationVolume: string | null;
  destinationSubpath: string;

  excludeMode: ExcludeMode;
  excludes: ExcludeRule[];

  pickFile(path: string): Promise<void>;
  pickFolder(path: string): Promise<void>;
  reset(): void;

  setMountAfterUpload(on: boolean): void;
  setMountReadOnly(on: boolean): void;
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

/** A `.zip` game dump — the engine decompresses it on the host and streams
 *  the files in. ZIP is the only archive format we support (see the
 *  "drop .rar" scope decision: modern scene .rar is split + encrypted). */
function isArchivePath(path: string): boolean {
  return path.toLowerCase().endsWith(".zip");
}

export const useUploadStore = create<UploadState>((set, get) => ({
  source: null,
  detecting: false,
  detectError: null,
  mountAfterUpload: false,
  mountReadOnly: true,
  destinationVolume: null,
  // Default to /data/homebrew/ — the community-standard scan path
  // that third-party PS5 game scanners typically walk. Files landed
  // here are auto-discoverable by other PS5 tools the user might also
  // be running. Pre-2.2.32 we defaulted to "ps5upload", which forced
  // users into a tool-specific subfolder and broke interop. Users with
  // a different preference can still edit the field; the change only
  // affects the first-launch default.
  destinationSubpath: "homebrew",
  excludeMode: "all",
  excludes: defaultExcludes,

  async pickFile(path) {
    // A .zip is an archive source: optimistically mark it, then inspect the
    // central directory (fast — no inflation) to show what it expands to and
    // detect an embedded game. Mirrors pickFolder's stale-result guard.
    if (isArchivePath(path)) {
      set({
        source: {
          kind: "archive",
          path,
          meta: null,
          wrappedHint: null,
          zipInfo: null,
        },
        detecting: true,
        detectError: null,
        mountAfterUpload: false,
      });
      try {
        const zipInfo = await zipInspect(path);
        if (get().source?.path !== path) return;
        set({
          source: {
            kind: "archive",
            path,
            meta: null,
            wrappedHint: null,
            zipInfo,
          },
          detecting: false,
          detectError: null,
        });
      } catch (e) {
        if (get().source?.path !== path) return;
        set({
          detecting: false,
          detectError: e instanceof Error ? e.message : String(e),
        });
      }
      return;
    }
    const kind: SourceKind = isImagePath(path) ? "image" : "file";
    set({
      source: { kind, path, meta: null, wrappedHint: null, zipInfo: null },
      detecting: false,
      detectError: null,
      // mount-after-upload defaults OFF — even for disk images. The
      // mount call attaches the image to /dev/lvdN (or /dev/mdN) +
      // nmount(2)s it onto /mnt/ps5upload/<name>, which alters
      // kernel-visible state and is harder to undo than a plain
      // upload. Users opt in by ticking the checkbox; the alternative
      // pre-2.2.31 behavior auto-flipped it on for .exfat/.ffpkg and
      // surprised users who only wanted to land the file on disk.
      mountAfterUpload: get().mountAfterUpload,
    });
  },

  async pickFolder(path) {
    // Optimistic set so the UI can render "Inspecting <path>…" immediately.
    set({
      source: {
        kind: "folder",
        path,
        meta: null,
        wrappedHint: null,
        zipInfo: null,
      },
      detecting: true,
      detectError: null,
    });
    try {
      const inspection = await inspectFolder(path);
      // Stale-result guard. inspectFolder is async (walks the dir,
      // parses param.sfo, etc. — can take seconds on slow disks). If
      // the user picked a different source while this inspect was
      // in flight (another folder via pickFolder, or a file via
      // pickFile), the latest pick wins. Without this guard, an
      // earlier inspect that resolves second would set source.path
      // back to its closure-captured `path` arg, silently clobbering
      // the newer source — the destination preview then renders
      // the OLD file/folder name even though the user already moved
      // on, and the upload at that preview path would land the
      // wrong source entirely if they hit Upload before noticing.
      if (get().source?.path !== path) return;
      const isGame = inspection.result.meta_source !== "none";
      set({
        source: {
          kind: isGame ? "game-folder" : "folder",
          path,
          meta: inspection.result,
          wrappedHint: inspection.wrapped_hint,
          zipInfo: null,
        },
        detecting: false,
        detectError: null,
      });
    } catch (e) {
      // Same race window for the failure branch — don't surface an
      // error for an inspect the user already abandoned.
      if (get().source?.path !== path) return;
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
  setMountReadOnly: (mountReadOnly) => set({ mountReadOnly }),

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
