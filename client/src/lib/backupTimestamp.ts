// Timestamp string used to name a "Save to USB storage" backup folder:
// `<savePath>/<title_id>/<timestamp>/<title_id>.zip`. Sortable lexically
// and filesystem-safe on every target (PS5 exFAT/FAT32/UFS, host OSes).
//
// Pure function — `d` is passed in so tests don't depend on the wall
// clock (mirrors the `nowMs`-injection convention in pkgStagingPath.ts).

function pad(n: number, width = 2): string {
  return String(n).padStart(width, "0");
}

/** Local-time `YYYY-MM-DD_HHMMSS`, e.g. `2026-06-17_142530`. Local time
 *  (not UTC) so the folder name matches what the user would expect to
 *  see if they browse the USB drive themselves. */
export function backupTimestamp(d: Date = new Date()): string {
  const date = `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
  const time = `${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}`;
  return `${date}_${time}`;
}
