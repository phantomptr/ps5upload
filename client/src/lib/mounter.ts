// Which mounter runs on a console, and what that means for what PS5Upload offers.
//
// The engine's mounter snapshot (`smp_status`: `mounter` + `kraken`, see
// engine/crates/ps5upload-core/src/kraken.rs) is the one source. Kraken Mount
// (quer3q/kraken-mount 0.2.0) mounts debug FPKG `.pkg` files and game folders from its scan
// roots without installing them; it does not mount disk images. ShadowMount+ mounts images and
// folders; it installs packages but does not mount them (1.7beta12).

import { create } from "zustand";

import type { KrakenGame, KrakenStatus, Mounter, SmpStatus } from "../api/ps5";
import { hostOf } from "./addr";
import { compareVersions } from "./semver";

/** The mounter a snapshot names; an engine older than the field gives only ShadowMount+'s state. */
export function mounterOf(s: SmpStatus | null | undefined): Mounter | null {
  if (!s) return null;
  if (s.mounter) return s.mounter;
  return s.running ? "shadowmount" : "none";
}

/** Kraken Mount runs (alone or, wrongly, beside ShadowMount+). */
export function krakenRuns(s: SmpStatus | null | undefined): boolean {
  const m = mounterOf(s);
  return m === "kraken" || m === "both";
}

/** Kraken Mount runs and ShadowMount+ does not: the flows that put a package where it mounts it,
 *  instead of installing it, are offered. */
export function krakenIsTheMounter(s: SmpStatus | null | undefined): boolean {
  return mounterOf(s) === "kraken";
}

/** The titles Kraken Mount has put on the home screen. */
export function krakenTitleIds(s: SmpStatus | null | undefined): Set<string> {
  return new Set((s?.kraken?.title_ids ?? []).map((id) => id.toUpperCase()));
}

/** Kraken Mount's library row for a title (not a duplicate copy), when its API answered. */
export function krakenGameFor(
  s: SmpStatus | null | undefined,
  titleId: string,
): KrakenGame | null {
  const id = titleId.toUpperCase();
  return (
    s?.kraken?.games.find((g) => g.title_id.toUpperCase() === id && g.status !== "duplicate") ??
    null
  );
}

/** The drive a console path is on: `/data`, `/mnt/ext1`, `/mnt/usb0`; null for anything else. */
export function driveOf(path: string): string | null {
  if (path === "/data" || path.startsWith("/data/")) return "/data";
  const m = /^\/mnt\/(ext|usb)\d{1,2}(?=\/|$)/.exec(path);
  return m ? m[0] : null;
}

/** Where a package goes on `drive` for Kraken Mount to find it: the drive's `homebrew` folder
 *  (a root of its default list on every drive), or, when the user's `scanpath` lines replace that
 *  list, the first of them on the drive. Null when it scans nothing on that drive. It scans one
 *  level deep, so the package goes straight in that folder. Mirrors `scan_root_for_drive` in
 *  engine/crates/ps5upload-core/src/kraken.rs. */
export function krakenScanRoot(drive: string, kraken: KrakenStatus | null | undefined): string | null {
  const d = drive.replace(/\/+$/, "");
  if (!d) return null;
  const own = kraken?.settings?.scan_paths ?? [];
  if (own.length === 0) return `${d}/homebrew`;
  return own.find((p) => p === d || p.startsWith(`${d}/`)) ?? null;
}

/** Why Kraken Mount can't mount a package, from what its header says; null when it can, as far as
 *  the header tells. It reads only PS5 packages made with the debug key (a PS4 package, a retail
 *  one and a patch package are refused: crates/kraken-mount-pkg/src/pkg.rs:86-145), and only
 *  whole ones. What the header can't tell (a plaintext outer image with a Kraken layout, which
 *  every package Convert makes has) Kraken Mount reports itself after the scan. */
export function krakenRefusal(head: {
  platform?: string | null;
  authenticity?: string | null;
  parts?: number;
}): "ps4" | "retail" | "split" | null {
  if ((head.parts ?? 1) > 1) return "split";
  if (head.platform && head.platform.toUpperCase() !== "PS5") return "ps4";
  if (head.authenticity === "retail") return "retail";
  return null;
}

/** The file name a package takes in a scan root: the user's own name, kept readable, with any
 *  folder part and characters a PS5 drive can't hold taken out, and `.pkg` at the end. */
export function mountFileName(originalName: string, contentId: string): string {
  const base = [...(originalName.split(/[\\/]/).pop() ?? "")]
    .map((c) => (c < " " || '<>:"|?*'.includes(c) ? "_" : c))
    .join("")
    .trim();
  const stem = base.replace(/\.pkg$/i, "").replace(/^\.+/, "").trim();
  return `${stem || contentId || "game"}.pkg`;
}

/** The last mounter snapshot read for each console, so screens that don't probe themselves (the
 *  Library's badges, Convert's ②) can follow it. Filled by `useSmpState`. */
export const useMounterStore = create<{
  byHost: Record<string, SmpStatus>;
  put: (host: string, s: SmpStatus) => void;
}>((set) => ({
  byHost: {},
  put: (host, s) => set((st) => ({ byHost: { ...st.byHost, [hostOf(host)]: s } })),
}));

/** The last snapshot for `host`, or null before one was read. */
export function useMounterFor(host: string | null | undefined): SmpStatus | null {
  return useMounterStore((st) => (host ? (st.byHost[hostOf(host)] ?? null) : null));
}

/** The first Kraken Mount that gives a game the sce_sys files a package keeps only inside it
 *  (save_data.png, the artwork, the trophy and NP files): before it, a game mounted from a
 *  package Convert made (they keep those files there, as every debug package does) could lose
 *  its save data and trophies. */
export const KRAKEN_MIN_VERSION = "0.2.1";

/** Its version when it is older than KRAKEN_MIN_VERSION, else null (newer, or not known). */
export function krakenOutdated(kraken: KrakenStatus | null | undefined): string | null {
  const v = kraken?.version?.trim().replace(/^v/i, "");
  if (!v) return null;
  return compareVersions(v, KRAKEN_MIN_VERSION) === -1 ? v : null;
}

// ─── Kraken Mount's games are never cleaned up (#894) ────────────────
//
// A `.pkg` Kraken Mount mounts IS the game: deleting it, moving it or "freeing" it takes the game
// away. What its API lists is kept, and so is every package file or folder directly in a folder it
// scans (one level deep), whether it runs now or not: when it is stopped those files are still the
// user's games. Mirrors `KrakenKeep` in engine/crates/ps5upload-core/src/console_cleanup.rs and
// `scr_kraken_keeps` in payload/src/cleanup_rules.c.

const USB = [0, 1, 2, 3, 4, 5, 6, 7];

/** Its default scan roots (engine `kraken::default_scan_roots`). */
export const KRAKEN_DEFAULT_ROOTS: readonly string[] = [
  "/data/homebrew",
  "/data/etaHEN/games",
  ...[0, 1].flatMap((n) => [`/mnt/ext${n}/homebrew`, `/mnt/ext${n}/etaHEN/games`]),
  ...USB.map((n) => `/mnt/usb${n}/homebrew`),
  ...USB.map((n) => `/mnt/usb${n}/etaHEN/games`),
  ...USB.map((n) => `/mnt/usb${n}`),
  "/mnt/ext0",
  "/mnt/ext1",
];

/** `/user/data` written as `/data`, without trailing slashes. */
function krakenNorm(path: string): string {
  let p = path.trim();
  if (p === "/user/data" || p.startsWith("/user/data/")) p = `/data${p.slice("/user/data".length)}`;
  p = p.replace(/\/+$/, "");
  return p || "/";
}

function isInside(path: string, dir: string): boolean {
  return path.length > dir.length && path.startsWith(dir) && path[dir.length] === "/";
}

/** Whether `path` (a file, or a folder when `isDir`) on the console is a game Kraken Mount uses:
 *  one its API lists, inside one, a folder holding one, or a `.pkg` or folder directly in a folder
 *  it scans. `kraken` is the engine's snapshot; `undefined` (not known: no snapshot, an older
 *  engine) counts its default roots, as if it were there. `null` or a snapshot saying it never
 *  ran here counts only what its API lists. */
export function krakenKeepsPath(
  path: string,
  kraken: KrakenStatus | null | undefined,
  isDir = false,
): boolean {
  const p = krakenNorm(path);
  if (!p.startsWith("/") || p === "/") return false;
  const games = (kraken?.games ?? []).map((g) => krakenNorm(g.path)).filter((g) => g !== "/");
  if (games.some((g) => g === p || isInside(p, g) || (isDir && isInside(g, p)))) return true;
  const present = kraken === undefined || !!kraken?.installed || !!kraken?.running;
  if (!present) return false;
  const own = kraken?.scan_roots ?? [];
  const roots = (own.length > 0 ? own : KRAKEN_DEFAULT_ROOTS).map(krakenNorm);
  const slash = p.lastIndexOf("/");
  if (slash <= 0) return false;
  const mountable = isDir || p.toLowerCase().endsWith(".pkg");
  return mountable && roots.includes(p.slice(0, slash));
}
