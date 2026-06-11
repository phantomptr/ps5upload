import { create } from "zustand";
import {
  inspectFolder,
  zipInspectStream,
  sevenzInspectStream,
  rarInspect,
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
  /** Running CDFH-record count while `zipInspectStream` is in flight. Null
   *  when no archive scan is happening or the engine hasn't sent its first
   *  progress tick yet. Drives the "Scanning archive… N entries" indicator
   *  on the Upload screen — it's how the user sees the engine is alive
   *  during a slow cold-cache central-directory read. */
  zipInspectEntries: number | null;

  /** Password for an encrypted `.rar` source. Held only in memory for the
   *  current pick — fed to both the rar inspect and the rar transfer, and
   *  never persisted or logged. Null/empty = no password (the common case).
   *  Ignored for non-rar sources. */
  rarPassword: string | null;

  mountAfterUpload: boolean;
  /** Mount the image read-only when mount-after-upload runs. Default
   *  true — the safer choice: the PS5 can't accidentally write
   *  save-data into the image and corrupt it on next mount. Users
   *  who want save-data to persist back into the image (rare; mostly
   *  for editable homebrew scratchpads) flip this off explicitly. */
  mountReadOnly: boolean;
  /** Game-folder-only: register the game with the PS5 OS right after the
   *  upload commits, so it lands on the home screen in one step. Sticky
   *  preference (localStorage), default ON — uploading a game and NOT
   *  wanting to play it is the rare case. */
  registerAfterUpload: boolean;

  destinationVolume: string | null;
  destinationSubpath: string;

  excludeMode: ExcludeMode;
  excludes: ExcludeRule[];

  pickFile(path: string): Promise<void>;
  pickFolder(path: string): Promise<void>;
  reset(): void;
  /** Switching consoles: drop the in-progress draft so a source picked +
   *  configured for console A can't fire against console B. Clears the source,
   *  the mount toggle and the (PS5-side) destination volume; KEEPS the
   *  cross-console preferences (subpath, excludes, register-after-upload). */
  clearForHostChange(): void;

  /** Set the encrypted-`.rar` password and re-inspect the current source so
   *  the file tree / "needs password" state updates. */
  setRarPassword(password: string): Promise<void>;
  setMountAfterUpload(on: boolean): void;
  setMountReadOnly(on: boolean): void;
  setRegisterAfterUpload(on: boolean): void;
  setDestination(volume: string | null, subpath?: string): void;
  setExcludeMode(mode: ExcludeMode): void;
  toggleExclude(pattern: string): void;
  addExclude(pattern: string): void;
  removeExclude(pattern: string): void;
}

const defaultExcludes: ExcludeRule[] = [
  { pattern: ".DS_Store", enabled: true },
  // macOS AppleDouble sidecars (._eboot.bin, ._sce_sys, …). Created on
  // non-HFS/exFAT volumes; uploading them pollutes the PS5 with junk and
  // can confuse folder-dump registration. Prefix wildcard → any basename
  // starting with "._".
  { pattern: "._*", enabled: true },
  { pattern: "Thumbs.db", enabled: true },
  { pattern: "desktop.ini", enabled: true },
  { pattern: "*.esbak", enabled: true },
  { pattern: ".git/**", enabled: true },
];

/** Lowercase extension test for all PS5 disk-image formats we accept for
 *  upload — `.exfat` (exFAT), `.ffpkg` (UFS2), `.ffpfs` (PFS), and `.ffpfsc`
 *  (compressed/nested PFS container). All four can be dropped in a
 *  ShadowMount+ scan folder and SMP will mount + register them. */
export function isImagePath(path: string): boolean {
  const p = path.toLowerCase();
  return (
    p.endsWith(".exfat") ||
    p.endsWith(".ffpkg") ||
    p.endsWith(".ffpfs") ||
    p.endsWith(".ffpfsc")
  );
}

/** Which images ps5upload's OWN mount path can attach directly (LVD/MD)
 *  without ShadowMount+: exFAT, UFS (.ffpkg), and PFS (.ffpfs). The payload's
 *  fs_mount_detect_fstype handles exactly these (runtime.c). `.ffpfsc`
 *  (compressed/nested PFS container) is NOT directly mountable — only
 *  ShadowMount+ can open those — so the "mount after upload" option is hidden
 *  for it; upload it into a scan folder and let SMP handle it. */
export function payloadCanMountImage(path: string): boolean {
  const p = path.toLowerCase();
  return p.endsWith(".exfat") || p.endsWith(".ffpkg") || p.endsWith(".ffpfs");
}

/** Archive format we can host-decompress + stream: `.zip` (deflate) or `.7z`
 *  (LZMA2, commonly a single `.exfat` image). `.rar` is intentionally out of
 *  scope — modern scene .rar is split + encrypted. Returns null for anything
 *  else. */
export function archiveFormat(path: string): "zip" | "7z" | "rar" | null {
  const p = path.toLowerCase();
  if (p.endsWith(".zip")) return "zip";
  if (p.endsWith(".7z")) return "7z";
  // `.rar` covers a single archive and the first volume of a multi-part set
  // (`name.part1.rar`); UnRAR pulls in the sibling volumes automatically.
  // Old-style `.r00`/`.r01` parts accompany a `.rar`, so the `.rar` is what
  // the user picks. RAR is desktop-only (see engine `caps.rar`).
  if (p.endsWith(".rar")) return "rar";
  return null;
}

/** A `.zip` or `.7z` game dump — the engine decompresses it on the host and
 *  streams the files in. */
function isArchivePath(path: string): boolean {
  return archiveFormat(path) !== null;
}

export const useUploadStore = create<UploadState>((set, get) => ({
  source: null,
  detecting: false,
  detectError: null,
  zipInspectEntries: null,
  rarPassword: null,
  mountAfterUpload: false,
  mountReadOnly: true,
  registerAfterUpload:
    typeof window === "undefined"
      ? true
      : window.localStorage.getItem("ps5upload.register_after_upload") !==
        "false",
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
    // A new pick clears any stale .rar password. `setRarPassword` re-inspects
    // the SAME path, so it deliberately doesn't trip this reset.
    if (get().source?.path !== path) set({ rarPassword: null });
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
        zipInspectEntries: null,
      });
      try {
        const fmt = archiveFormat(path);
        let zipInfo: ZipInspect;
        if (fmt === "rar") {
          // RAR inspect is a one-shot metadata read (no streaming progress).
          // Pass the current password (may be null); it throws a message
          // containing `rar_password_required` / `rar_password_wrong` which
          // the catch surfaces so the Upload screen shows the password field.
          zipInfo = await rarInspect(path, get().rarPassword);
        } else {
          // Same inspect-stream shape for zip + 7z — pick by extension.
          const inspect = fmt === "7z" ? sevenzInspectStream : zipInspectStream;
          zipInfo = await inspect(path, (p) => {
            // Stale-result guard: if the user dropped another file mid-scan,
            // ignore late ticks from the previous one. Without this the
            // entry count would briefly flicker between two unrelated archives.
            if (get().source?.path !== path) return;
            set({ zipInspectEntries: p.entries_seen });
          });
        }
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
          zipInspectEntries: null,
        });
      } catch (e) {
        if (get().source?.path !== path) return;
        set({
          detecting: false,
          detectError: e instanceof Error ? e.message : String(e),
          zipInspectEntries: null,
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
      rarPassword: null,
      mountAfterUpload: false,
    }),

  clearForHostChange: () =>
    set({
      source: null,
      detecting: false,
      detectError: null,
      zipInspectEntries: null,
      rarPassword: null,
      mountAfterUpload: false,
      // Destination volume is a PS5-side path that only exists on the previous
      // console — clear it so the new console re-detects its own volumes.
      destinationVolume: null,
    }),

  async setRarPassword(password) {
    set({ rarPassword: password });
    // Re-inspect the current source with the new password so the file tree
    // (or the "needs password" error) updates. pickFile keeps rarPassword
    // because the path is unchanged.
    const src = get().source;
    if (src && archiveFormat(src.path) === "rar") {
      await get().pickFile(src.path);
    }
  },

  setMountAfterUpload: (mountAfterUpload) => set({ mountAfterUpload }),
  setMountReadOnly: (mountReadOnly) => set({ mountReadOnly }),
  setRegisterAfterUpload: (registerAfterUpload) => {
    // Sticky across sessions — this is a workflow preference, not a
    // per-upload decision (unlike mountAfterUpload, which resets with
    // the picked source).
    window.localStorage.setItem(
      "ps5upload.register_after_upload",
      registerAfterUpload ? "true" : "false",
    );
    set({ registerAfterUpload });
  },

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
