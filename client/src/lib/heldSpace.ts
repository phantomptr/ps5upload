/**
 * "Space no file uses" in Clean up (#979): which drives the check is for, and what its answer says.
 */
import type { SpaceCheck } from "../api/cleanup";
import type { Volume } from "../api/ps5";
import { driveName, type CleanupT } from "./cleanupText";
import { formatBytes } from "./format";

/** M.2 and USB drives (`/mnt/ext1`, `/mnt/usb0`): the engine refuses internal storage, where the
 *  console keeps folders the helper cannot read. */
export function checkableDrives(volumes: readonly Volume[]): Volume[] {
  return volumes.filter(
    (v) => /^\/mnt\/(ext|usb)\d+$/.test(v.path) && v.fs_type !== "nullfs" && !v.is_placeholder && v.total_bytes > 0,
  );
}

/** The drive's name as Clean up says it ("M.2 drive 2", "USB drive 1"). */
export function heldDriveName(t: CleanupT, path: string): string {
  return driveName(t, path.replace(/^\/mnt\//, ""));
}

/** The line a check's answer shows, and whether it found space that needs the explanation. */
export function heldSpaceLine(t: CleanupT, r: SpaceCheck): { text: string; found: boolean } {
  const drive = heldDriveName(t, r.path);
  const size = formatBytes(r.unaccounted_bytes);
  switch (r.verdict) {
    case "clear":
      return { text: t("cleanup_held_clear", { drive }), found: false };
    case "held":
      return { text: t("cleanup_held_found", { drive, size }), found: true };
    case "partial":
      return { text: t("cleanup_held_partial", { drive, size, count: r.skipped }), found: false };
    default:
      return { text: t("cleanup_held_busy", { drive }), found: false };
  }
}
