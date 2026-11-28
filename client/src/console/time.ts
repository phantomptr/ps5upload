/**
 * Dates and times on the clock of whoever is looking at the page (#928).
 *
 * A browser shows a time in its own time zone, and on a phone or a PC that is the viewer's. The
 * PS5's own browser has none: its clock reads UTC whatever zone the console is set to, so a save
 * backed up at 07:25 in Recife (UTC-3) was listed as 10:25. There the zone comes from the console
 * (/api/status `tz_min`, minutes ahead of UTC, summer time included), and every date the page
 * shows goes through here.
 *
 * The console's zone is used only on the console's own browser, and only while that browser has
 * no zone of its own: a browser that knows its zone is never second-guessed.
 *
 * #978: the helper now takes the zone from the console's own clock conversion first (the
 * registry alone gave UTC on a Slim on 13.60), and Settings › Time zone can set it by hand; either
 * way it arrives here as `tz_min`.
 */
import { isPs5Browser } from "./fullscreen";
import { getLanguage } from "./strings";
import { onConsoleBrowser } from "./ui/PhoneHandoff";

/** No real zone is further from UTC than +14:00 / -12:00. */
const MAX_ZONE_MIN = 14 * 60;

let consoleZoneMin: number | null = null;

/**
 * What /api/status gave as the console's zone (null or absent: it could not tell, or an older
 * helper). `here`: the page runs in the console's own browser.
 */
export function setConsoleZone(min: number | null | undefined, here: boolean = isPs5Browser() || onConsoleBrowser()): void {
  consoleZoneMin =
    here && typeof min === "number" && Number.isInteger(min) && Math.abs(min) <= MAX_ZONE_MIN ? min : null;
}

/** Whether this browser reads UTC all year: it has no zone (or is really on UTC, which shows the same). */
function browserHasNoZone(year: number): boolean {
  return new Date(year, 0, 1).getTimezoneOffset() === 0 && new Date(year, 6, 1).getTimezoneOffset() === 0;
}

/**
 * A Date whose own fields (and toLocaleString) read `ms` on the viewer's clock. It is `ms` itself
 * unless the console's zone stands in for a browser without one.
 */
function onViewerClock(ms: number): Date {
  const d = new Date(ms);
  if (consoleZoneMin == null || !browserHasNoZone(d.getFullYear())) return d;
  return new Date(ms + consoleZoneMin * 60_000);
}

/** Date and time of `ms` (since 1970, UTC) for the viewer, in the page's language. */
export function formatDateTime(ms: number, lang: string = getLanguage()): string {
  return onViewerClock(ms).toLocaleString(lang);
}

/** The date of `ms` for the viewer, in the page's language. */
export function formatDate(ms: number, lang: string = getLanguage()): string {
  return onViewerClock(ms).toLocaleDateString(lang);
}

/**
 * Every offset a real time zone uses, in minutes ahead of UTC (#978: Settings › Time zone steps
 * through these, so a half- or quarter-hour zone is one step and nobody scrolls past 96 values).
 */
export const ZONE_OFFSETS: readonly number[] = [
  -720, -660, -600, -570, -540, -480, -420, -360, -300, -240, -210, -180, -150, -120, -60, 0, 60, 120, 180, 210, 240,
  270, 300, 330, 345, 360, 390, 420, 480, 525, 540, 570, 600, 630, 660, 720, 765, 780, 825, 840,
];

/** The next real offset after `min` (dir 1) or before it (dir -1); the ends stay where they are. */
export function stepZone(min: number, dir: 1 | -1): number {
  if (dir > 0) return ZONE_OFFSETS.find((z) => z > min) ?? ZONE_OFFSETS[ZONE_OFFSETS.length - 1];
  for (let i = ZONE_OFFSETS.length - 1; i >= 0; i--) if (ZONE_OFFSETS[i] < min) return ZONE_OFFSETS[i];
  return ZONE_OFFSETS[0];
}

/** -180 → "UTC−03:00", 345 → "UTC+05:45", 0 → "UTC". */
export function formatZone(min: number): string {
  if (min === 0) return "UTC";
  const a = Math.abs(min);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `UTC${min < 0 ? "−" : "+"}${pad(Math.floor(a / 60))}:${pad(a % 60)}`;
}

/** The time of day `ms` reads in the zone `min` minutes ahead of UTC (hours and minutes). */
export function timeInZone(ms: number, min: number, lang: string = getLanguage()): string {
  return new Date(ms + min * 60_000).toLocaleTimeString(lang, { timeZone: "UTC", hour: "2-digit", minute: "2-digit" });
}
