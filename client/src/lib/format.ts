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
  if (n < 1024) return `${Math.round(n)} B`;
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

const unitFormats = new Map<string, Intl.NumberFormat | null>();

/** "5 min"-style text for one unit in `lang`, or null when the WebView has no unit formatting. */
function unitText(n: number, unit: "hour" | "minute" | "second", lang: string): string | null {
  const key = `${lang}|${unit}`;
  let f = unitFormats.get(key);
  if (f === undefined) {
    try {
      f = new Intl.NumberFormat(lang, { style: "unit", unit, unitDisplay: "narrow" });
    } catch {
      f = null; // An unknown tag, or a WebView older than unit formatting.
    }
    unitFormats.set(key, f);
  }
  return f ? f.format(n) : null;
}

/**
 * A length of time as "1h 5m" / "2m 3s" / "4s". With no `lang` the units stay as those letters;
 * with one they are the language's own short units. Screens use the `formatDuration` in
 * `lib/formatDate.ts`, which passes the app's language (this module stays free of state so the
 * Minion page can import it).
 */
export function formatDuration(sec: number, lang?: string): string {
  if (!isFinite(sec) || sec < 0) return "—";
  const fmt = (n: number, unit: "hour" | "minute" | "second", letter: string) =>
    (lang ? unitText(n, unit, lang) : null) ?? `${n}${letter}`;
  if (sec < 60) return fmt(Math.ceil(sec), "second", "s");
  const m = Math.floor(sec / 60);
  const s = Math.floor(sec % 60);
  if (m < 60) return `${fmt(m, "minute", "m")} ${fmt(s, "second", "s")}`;
  const h = Math.floor(m / 60);
  return `${fmt(h, "hour", "h")} ${fmt(m % 60, "minute", "m")}`;
}
