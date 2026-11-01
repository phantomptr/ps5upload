// Save Data's "Backed up 2 h ago" line.

/** The key of a game and user in the newest-backup map. */
export function backupKey(titleId: string, userId: number): string {
  return `${titleId.toUpperCase()}:${(userId >>> 0).toString(16).padStart(8, "0")}`;
}

/** A backup stamp (`YYYY-MM-DD_HHMMSS`, UTC) as Unix ms; null when it isn't one. */
export function stampMs(stamp: string): number | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})_(\d{2})(\d{2})(\d{2})/.exec(stamp);
  if (!m) return null;
  const [, y, mo, d, h, mi, s] = m.map(Number);
  return Date.UTC(y, mo - 1, d, h, mi, s);
}

/** How long ago `stamp` was, in the app's language ("2 hours ago", "yesterday"); null when the
 *  stamp can't be read. */
export function backupAgo(stamp: string, now: number, lang: string): string | null {
  const at = stampMs(stamp);
  if (at === null) return null;
  const secs = Math.round((at - now) / 1000);
  let rtf: Intl.RelativeTimeFormat;
  try {
    rtf = new Intl.RelativeTimeFormat(lang, { numeric: "auto" });
  } catch {
    rtf = new Intl.RelativeTimeFormat("en", { numeric: "auto" });
  }
  const abs = Math.abs(secs);
  if (abs < 60) return rtf.format(0, "minute");
  if (abs < 3600) return rtf.format(Math.round(secs / 60), "minute");
  if (abs < 86400) return rtf.format(Math.round(secs / 3600), "hour");
  if (abs < 86400 * 30) return rtf.format(Math.round(secs / 86400), "day");
  if (abs < 86400 * 365) return rtf.format(Math.round(secs / (86400 * 30)), "month");
  return rtf.format(Math.round(secs / (86400 * 365)), "year");
}
