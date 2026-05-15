/**
 * Pure-function helpers for the Hardware screen's System time card.
 *
 * Extracted out of the component file so they can be unit-tested in
 * isolation (the component itself wires Tauri invokes + React state,
 * which is messy to test; these helpers are deterministic).
 *
 * Keep these functions side-effect-free and test-friendly: no Date.now()
 * calls inside (the caller passes pcMs as an argument so tests can
 * pin time).
 */

/** Wire shape returned by the `ps5_time_get` Tauri command. Fields
 *  match the JSON the payload emits in TIME_GET_ACK; only `ok` is
 *  guaranteed present (the rest are zero/undefined when ok=false). */
export interface PsTimeJson {
  ok: boolean;
  err_code: number;
  year?: number;
  month?: number;
  day?: number;
  hour?: number;
  min?: number;
  sec?: number;
}

/** Convert a PsTimeJson into a JS Date (UTC). null if !ok, missing
 *  fields, or an impossible date (we delegate further range checking
 *  to JS Date itself, which is permissive — bad inputs land on a
 *  rollover date but still produce a Date object). */
export function psTimeToDate(t: PsTimeJson | null): Date | null {
  if (!t || !t.ok) return null;
  const y = t.year ?? 0;
  const mo = t.month ?? 0;
  const d = t.day ?? 0;
  const h = t.hour ?? 0;
  const mi = t.min ?? 0;
  const s = t.sec ?? 0;
  if (y < 1970 || mo < 1 || mo > 12 || d < 1) return null;
  return new Date(Date.UTC(y, mo - 1, d, h, mi, s));
}

/** Compact "YYYY-MM-DD HH:MM:SS UTC" formatter. Always UTC because
 *  PS5 stores system time in UTC; the on-console timezone offset is
 *  applied at display time inside Sony's UI, not in the stored value. */
export function formatUtcCompact(d: Date | null): string {
  if (!d) return "—";
  const yyyy = d.getUTCFullYear();
  const mm = String(d.getUTCMonth() + 1).padStart(2, "0");
  const dd = String(d.getUTCDate()).padStart(2, "0");
  const hh = String(d.getUTCHours()).padStart(2, "0");
  const mi = String(d.getUTCMinutes()).padStart(2, "0");
  const ss = String(d.getUTCSeconds()).padStart(2, "0");
  return `${yyyy}-${mm}-${dd} ${hh}:${mi}:${ss} UTC`;
}

/** Drift string. Positive (with + prefix) = PS5 is ahead of the PC;
 *  negative (with − prefix, a real minus sign U+2212 not a hyphen)
 *  = PS5 is behind. Granularity scales: seconds under a minute,
 *  M m S s under an hour, H h M m otherwise. */
export function formatDrift(ps5Date: Date | null, pcMs: number): string {
  if (!ps5Date) return "—";
  const diffSec = Math.round((ps5Date.getTime() - pcMs) / 1000);
  const ab = Math.abs(diffSec);
  const sign = diffSec >= 0 ? "+" : "−";
  if (ab < 60) return `${sign}${ab}s`;
  if (ab < 3600) {
    const m = Math.floor(ab / 60);
    const s = ab % 60;
    return `${sign}${m}m ${s}s`;
  }
  const h = Math.floor(ab / 3600);
  const m = Math.floor((ab % 3600) / 60);
  return `${sign}${h}h ${m}m`;
}
