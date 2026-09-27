// Where uploaded install packages are kept on the PS5, per console.
//
// Internal storage by default (the historical /user/data/ps5upload/pkg_library).
// The user can make another drive the default from the Volumes screen — an M.2
// or USB drive — and packages then go to <drive>/ps5upload/pkg_library. If
// that drive is not there (unplugged, read-only, or the drive list could not
// be read), packages fall back to internal storage rather than failing.
//
// The helper's own files (logs, transfer journals) always stay on internal
// storage; only install packages move.

import { create } from "zustand";

import type { Volume } from "../api/ps5";
import { hostOf } from "./addr";
import { safeGetItem, safeSetItem } from "./safeStorage";

export const INTERNAL_PKG_DIR = "/user/data/ps5upload/pkg_library";

const STORAGE_KEY = "ps5upload.pkg_storage.v1";
const INTERNAL_MOUNTS = new Set(["/data", "/user", "/user/data"]);
const IMAGE_MOUNT_PREFIX = "/mnt/ps5upload/";

const trimSlash = (p: string) => p.replace(/\/+$/, "") || "/";

export function isInternalVolume(path: string | null | undefined): boolean {
  return !path || INTERNAL_MOUNTS.has(trimSlash(path));
}

/** The package folder on a drive (`null` or an internal mount = internal). */
export function pkgDirOnVolume(volume: string | null): string {
  if (isInternalVolume(volume)) return INTERNAL_PKG_DIR;
  return `${trimSlash(volume as string)}/ps5upload/pkg_library`;
}

/** A real storage drive packages can live on: writable, not a placeholder,
 *  and not one of our own disk-image mounts. */
function isPackageDrive(v: Volume): boolean {
  return (
    v.writable &&
    !v.is_placeholder &&
    !v.source_image &&
    !v.path.startsWith(IMAGE_MOUNT_PREFIX)
  );
}

export interface PkgStorage {
  /** Folder new packages go to. */
  dir: string;
  /** The drive in use, or null for internal storage. */
  volume: string | null;
  /** A drive was chosen but is not usable right now, so internal is used. */
  fellBack: boolean;
}

/** Where new packages go: the chosen drive when it is present and writable,
 *  otherwise internal storage. `volumes` null = the drive list is unknown. */
export function resolvePkgStorage(
  chosen: string | null,
  volumes: Volume[] | null,
): PkgStorage {
  if (isInternalVolume(chosen)) {
    return { dir: INTERNAL_PKG_DIR, volume: null, fellBack: false };
  }
  const want = trimSlash(chosen as string);
  const usable = volumes?.some((v) => trimSlash(v.path) === want && isPackageDrive(v));
  return usable
    ? { dir: pkgDirOnVolume(want), volume: want, fellBack: false }
    : { dir: INTERNAL_PKG_DIR, volume: null, fellBack: true };
}

/** Every folder the package library lists: internal plus each storage drive,
 *  so packages stay visible after the default drive changes. */
export function libraryDirs(volumes: Volume[] | null): string[] {
  const dirs = [INTERNAL_PKG_DIR];
  for (const v of volumes ?? []) {
    if (!isPackageDrive(v) || isInternalVolume(v.path)) continue;
    const dir = pkgDirOnVolume(v.path);
    if (!dirs.includes(dir)) dirs.push(dir);
  }
  return dirs;
}

/** The drive a package path lives on, for labelling library rows. */
export function volumeOfPkgPath(path: string): string | null {
  if (path.startsWith(`${INTERNAL_PKG_DIR}/`)) return null;
  const at = path.indexOf("/ps5upload/pkg_library/");
  return at > 0 ? path.slice(0, at) : null;
}

function load(): Record<string, string> {
  try {
    const raw = safeGetItem(STORAGE_KEY);
    const parsed = raw ? JSON.parse(raw) : {};
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
}

interface PkgStorageState {
  /** Console (host without port) → chosen drive path. Absent = internal. */
  defaults: Record<string, string>;
  defaultFor: (host: string) => string | null;
  setDefault: (host: string, volume: string | null) => void;
}

export const usePkgStorageStore = create<PkgStorageState>((set, get) => ({
  defaults: load(),
  defaultFor: (host) => get().defaults[hostOf(host)] ?? null,
  setDefault: (host, volume) => {
    const key = hostOf(host);
    if (!key) return;
    const defaults = { ...get().defaults };
    if (isInternalVolume(volume)) delete defaults[key];
    else defaults[key] = trimSlash(volume as string);
    safeSetItem(STORAGE_KEY, JSON.stringify(defaults));
    set({ defaults });
  },
}));

/** Where a new package for `host` goes right now: its chosen drive, or
 *  internal storage when that drive is not in `volumes`. */
export function pkgStorageFor(
  host: string,
  volumes: Volume[] | null,
): PkgStorage & { chosen: string | null } {
  const chosen = usePkgStorageStore.getState().defaultFor(host);
  return { ...resolvePkgStorage(chosen, volumes), chosen };
}

/** Folders to create, top-down, so a package folder exists: everything from
 *  the drive's `ps5upload/` folder down to `dir` (mkdir is one level at a
 *  time). A path outside a package library is returned alone. */
export function pkgMkdirChain(dir: string): string[] {
  const clean = trimSlash(dir);
  const at = clean.indexOf("/ps5upload/pkg_library");
  if (at < 0) return [clean];
  const root = clean.slice(0, at); // the drive, or /user/data
  const rest = clean.slice(at + 1).split("/"); // ["ps5upload","pkg_library",...]
  const out: string[] = [];
  let cur = root;
  for (const part of rest) {
    cur = `${cur}/${part}`;
    out.push(cur);
  }
  return out;
}
