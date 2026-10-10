import { useLangStore } from "../state/lang";

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
