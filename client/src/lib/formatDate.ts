import { useLangStore } from "../state/lang";
import { formatDuration as formatDurationIn } from "./format";

/**
 * Dates and times in the app's language, not the operating system's.
 *
 * A bare `toLocaleString()` formats with the system locale, so someone who
 * runs the app in Japanese on an English Windows saw Japanese labels next to
 * "10/9/2026, 3:04:05 PM". Every date on screen goes through here instead.
 *
 * Explicit fields rather than `dateStyle`/`timeStyle`: the oldest WebViews we
 * ship to (safari13 target) don't know those options.
 */
export type DateFormat = "datetime" | "date" | "time" | "time-short";

const OPTIONS: Record<DateFormat, Intl.DateTimeFormatOptions> = {
  datetime: { year: "numeric", month: "short", day: "numeric", hour: "numeric", minute: "2-digit" },
  date: { year: "numeric", month: "short", day: "numeric" },
  time: { hour: "numeric", minute: "2-digit", second: "2-digit" },
  "time-short": { hour: "numeric", minute: "2-digit" },
};

const cache = new Map<string, Intl.DateTimeFormat>();

function formatter(lang: string, format: DateFormat): Intl.DateTimeFormat {
  const key = `${lang}|${format}`;
  let f = cache.get(key);
  if (!f) {
    let locale: string | undefined = lang;
    try {
      if (Intl.DateTimeFormat.supportedLocalesOf([lang]).length === 0) locale = undefined;
    } catch {
      locale = undefined;
    }
    f = new Intl.DateTimeFormat(locale, OPTIONS[format]);
    cache.set(key, f);
  }
  return f;
}

/** Format a Date or epoch milliseconds. Invalid input gives "". */
export function formatDate(
  value: Date | number,
  format: DateFormat = "datetime",
  lang: string = useLangStore.getState().lang,
): string {
  const d = typeof value === "number" ? new Date(value) : value;
  if (Number.isNaN(d.getTime())) return "";
  return formatter(lang, format).format(d);
}

/** `formatDate` bound to the current language; re-renders on a switch. */
export function useFormatDate(): (value: Date | number, format?: DateFormat) => string {
  const lang = useLangStore((s) => s.lang);
  return (value, format = "datetime") => formatDate(value, format, lang);
}

const numberFormats = new Map<string, Intl.NumberFormat>();

/** A count in the app's language ("1.234" in German), not the operating system's. */
export function formatNumber(
  n: number,
  lang: string = useLangStore.getState().lang,
): string {
  let f = numberFormats.get(lang);
  if (!f) {
    try {
      f = new Intl.NumberFormat(lang);
    } catch {
      f = new Intl.NumberFormat();
    }
    numberFormats.set(lang, f);
  }
  return f.format(n);
}

/** A length of time in the app's language: "1h 5m" in English, "1 ч 5 м" in Russian. */
export function formatDuration(
  sec: number,
  lang: string = useLangStore.getState().lang,
): string {
  return formatDurationIn(sec, lang);
}

function relativeFormat(lang: string): Intl.RelativeTimeFormat | null {
  try {
    return new Intl.RelativeTimeFormat(lang, { numeric: "auto" });
  } catch {
    try {
      return new Intl.RelativeTimeFormat("en", { numeric: "auto" });
    } catch {
      return null; // A WebView older than RelativeTimeFormat.
    }
  }
}

/** "57 days ago", "3 hours ago", … for an age in seconds, in the app's language. */
export function relativeAge(
  secs: number,
  lang: string = useLangStore.getState().lang,
): string {
  const rtf = relativeFormat(lang);
  const steps: Array<[Intl.RelativeTimeFormatUnit, number]> = [
    ["day", 86400],
    ["hour", 3600],
    ["minute", 60],
  ];
  for (const [unit, size] of steps) {
    if (secs >= size) {
      const n = Math.floor(secs / size);
      return rtf ? rtf.format(-n, unit) : `-${n} ${unit}`;
    }
  }
  const n = Math.max(1, Math.floor(secs));
  return rtf ? rtf.format(-n, "second") : `-${n} s`;
}

/**
 * How long ago `ms` was, for a recent-activity line: "now", "5 minutes ago", "yesterday", and the
 * date once it is over a week old (nobody wants to work out "240 hours ago"). Empty for 0.
 */
export function formatAgo(
  ms: number,
  lang: string = useLangStore.getState().lang,
): string {
  if (ms <= 0) return "";
  const diff = Date.now() - ms;
  if (diff >= 7 * 86_400_000) return formatDate(ms, "date", lang);
  if (diff < 60_000) {
    const rtf = relativeFormat(lang);
    return rtf ? rtf.format(0, "second") : formatDate(ms, "time-short", lang);
  }
  return relativeAge(Math.floor(diff / 1000), lang);
}
