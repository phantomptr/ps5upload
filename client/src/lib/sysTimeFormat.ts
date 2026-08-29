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

/** Wire shape returned by the `ps5_time_sync` Tauri command. */
export interface PsTimeSyncJson {
  ok: boolean;
  err_code: number;
  reason: string;
  prior_unix: number;
  new_unix: number;
  /** Engine heuristic: payload said ok but the clock never reached the
   *  target. */
  stub_no_op: boolean;
  /** The payload set the clock via settimeofday because the SCE call
   *  was absent, rejected, or a no-op. */
  used_fallback: boolean;
  /** The epoch the console was asked to adopt. */
  target_unix: number;
  /** "ntp" or "client". */
  source: string;
  /** Which NTP server answered, when source is "ntp". */
  ntp_server: string | null;
}

/** The four outcomes the card renders differently. */
export type SyncOutcome =
  | "synced"
  | "synced_fallback"
  | "stub_no_op"
  | "failed";

/**
 * Classify a sync result.
 *
 * `ok` alone is not enough to decide what to tell the user. A console
 * can report success without its clock moving, which must never be
 * shown as success — the user would walk away believing the problem is
 * fixed. And a success reached via settimeofday is worth distinguishing
 * because it has a visible consequence: that path moves the kernel wall
 * clock underneath ShellCore, so Sony's Settings UI may keep showing
 * the old time until it is reopened.
 */
export function classifySyncResult(r: PsTimeSyncJson): SyncOutcome {
  // Checked first: if the clock never reached the target, how we tried
  // to get there is not the story.
  if (r.stub_no_op) return "stub_no_op";
  if (!r.ok) return "failed";
  return r.used_fallback ? "synced_fallback" : "synced";
}

/** Human label for where a sync's target time came from. */
export function formatSyncSource(
  source: string,
  ntpServer: string | null,
): string {
  if (source === "ntp") return ntpServer ?? "NTP";
  return "this computer";
}
