/**
 * Shared formatters for byte sizes and durations. Identical
 * implementations existed inline in 7+ screens; extracting them here
 * is the prerequisite for any cross-screen UI (the ActivityBar and
 * Activity tab both need consistent rendering, and we can't have one
 * banner saying "1.2 GiB" while another says "1234 MiB" for the same
 * number).
 *
 * Behavior intentionally matches what was already in
 * screens/FileSystem and screens/Library — those were the canonical
 * displays before this consolidation.
 */

export function formatBytes(n: number): string {
  if (!isFinite(n) || n < 0) return "—";
  if (n < 1024) return `${n} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  // Two decimals for tens, one for hundreds, zero for thousands —
  // keeps the displayed precision proportional to the magnitude
  // rather than wasting characters on trailing-zero noise.
  return `${v.toFixed(v >= 100 ? 0 : v >= 10 ? 1 : 2)} ${units[i]}`;
}

/**
 * Decimal (1000-based) byte formatter for storage-VOLUME capacity.
 *
 * Sony — like every drive vendor — labels storage in decimal units
 * (1 GB = 10^9 bytes), so the PS5 Settings "Console Storage" screen
 * and a USB drive's advertised size are both base-1000. `formatBytes`
 * above is base-1024 (GiB), which is right for file sizes but renders
 * ~7.4% smaller than Sony for the same byte count — making a volume
 * card disagree with the console it's describing. Use THIS for volume
 * total/free so the numbers line up with the PS5; keep `formatBytes`
 * for file sizes where GiB is the expected convention.
 */
export function formatStorageBytes(n: number): string {
  if (!isFinite(n) || n < 0) return "—";
  if (n < 1000) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1000;
  let i = 0;
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000;
    i += 1;
  }
  // Same magnitude-proportional precision as formatBytes.
  return `${v.toFixed(v >= 100 ? 0 : v >= 10 ? 1 : 2)} ${units[i]}`;
}

export function formatDuration(sec: number): string {
  if (!isFinite(sec) || sec < 0) return "—";
  if (sec < 60) return `${Math.ceil(sec)}s`;
  const m = Math.floor(sec / 60);
  const s = Math.floor(sec % 60);
  if (m < 60) return `${m}m ${s}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}
