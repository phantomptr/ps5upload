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
