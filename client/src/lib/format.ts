/**
 * Shared formatters for byte sizes and durations. Identical
 * implementations existed inline in 7+ screens; extracting them here
 * is the prerequisite for any cross-screen UI (the OperationBar and
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

export function formatDuration(sec: number): string {
  if (!isFinite(sec) || sec < 0) return "—";
  if (sec < 60) return `${Math.ceil(sec)}s`;
  const m = Math.floor(sec / 60);
  const s = Math.floor(sec % 60);
  if (m < 60) return `${m}m ${s}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}
