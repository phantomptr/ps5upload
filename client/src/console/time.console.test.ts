// #928 on the console's own browser: its clock reads UTC whatever zone the PS5 is set to. Set
// before any Date use.
(globalThis as unknown as { process: { env: Record<string, string> } }).process.env.TZ = "UTC";

import { afterEach, describe, expect, it } from "vitest";

import { backupName, parseBackupName } from "./saves/model";
import { formatDate, formatDateTime, formatZone, setConsoleZone, stepZone, timeInZone, ZONE_OFFSETS } from "./time";

// A backup made at 07:25:59 on a console in Recife (UTC-3): 10:25:59 UTC.
const MADE = Date.UTC(2026, 10, 25, 10, 25, 59);
const RECIFE = -180;

afterEach(() => setConsoleZone(null, true));

describe("times on the PS5's own browser (#928)", () => {
  it("has a browser that reads UTC, as the PS5's does", () => {
    expect(new Date(MADE).getTimezoneOffset()).toBe(0);
  });

  it("shows the console's time once the helper says the zone", () => {
    setConsoleZone(RECIFE, true);
    expect(formatDateTime(MADE, "pt-BR")).toBe("25/11/2026, 07:25:59");
    expect(formatDateTime(MADE, "en-GB")).toBe("25/11/2026, 07:25:59");
  });

  it("showed UTC before, and still does when the helper cannot tell the zone", () => {
    expect(formatDateTime(MADE, "pt-BR")).toBe("25/11/2026, 10:25:59");
    setConsoleZone(undefined, true); // an older helper: no tz_min
    expect(formatDateTime(MADE, "pt-BR")).toBe("25/11/2026, 10:25:59");
    setConsoleZone(99999, true);
    expect(formatDateTime(MADE, "pt-BR")).toBe("25/11/2026, 10:25:59");
  });

  it("a backup made before this fix (named in UTC by this browser) shows the right time too", () => {
    setConsoleZone(RECIFE, true);
    const old = parseBackupName("2026-11-25_10-25-59");
    expect(old?.when).toBe(MADE);
    expect(formatDateTime(old!.when, "pt-BR")).toBe("25/11/2026, 07:25:59");
  });

  it("names a new backup in UTC, marked, and reads it back to the same moment", () => {
    setConsoleZone(RECIFE, true);
    const name = backupName(new Date(MADE));
    expect(name).toBe("2026-11-25_10-25-59Z");
    expect(parseBackupName(name)).toEqual({ name, when: MADE, auto: false });
    expect(parseBackupName(backupName(new Date(MADE), true))).toEqual({ name: "2026-11-25_10-25-59Z_auto", when: MADE, auto: true });
    expect(formatDateTime(parseBackupName(name)!.when, "pt-BR")).toBe("25/11/2026, 07:25:59");
  });

  it("moves the date with the zone (last played, a file's date)", () => {
    const late = Date.UTC(2026, 9, 21, 1, 30);
    expect(formatDate(late, "pt-BR")).toBe("21/10/2026");
    setConsoleZone(RECIFE, true);
    expect(formatDate(late, "pt-BR")).toBe("20/10/2026");
    setConsoleZone(9 * 60, true); // Tokyo
    expect(formatDate(Date.UTC(2026, 9, 20, 16), "en-GB")).toBe("21/10/2026");
  });

  it("#978: the report's backup, 03:01:53 UTC, reads 00:01:53 once the zone arrives (by hand or from the console)", () => {
    const made = Date.UTC(2026, 10, 27, 3, 1, 53);
    const name = backupName(new Date(made));
    expect(name).toBe("2026-11-27_03-01-53Z");
    expect(formatDateTime(parseBackupName(name)!.when, "pt-BR")).toBe("27/11/2026, 03:01:53"); // the bug
    setConsoleZone(RECIFE, true);
    expect(formatDateTime(parseBackupName(name)!.when, "pt-BR")).toBe("27/11/2026, 00:01:53");
  });

  it("names and steps through the offsets real zones use", () => {
    expect(formatZone(-180)).toBe("UTC−03:00");
    expect(formatZone(345)).toBe("UTC+05:45");
    expect(formatZone(-570)).toBe("UTC−09:30");
    expect(formatZone(0)).toBe("UTC");
    expect(stepZone(-180, -1)).toBe(-210);
    expect(stepZone(-180, 1)).toBe(-150);
    expect(stepZone(330, 1)).toBe(345);
    expect(stepZone(-720, -1)).toBe(-720);
    expect(stepZone(840, 1)).toBe(840);
    expect(stepZone(-100, 1)).toBe(-60); // off the map: the next real one
    expect(ZONE_OFFSETS.every((z) => z % 15 === 0 && z >= -720 && z <= 840)).toBe(true);
    expect(timeInZone(Date.UTC(2026, 10, 27, 3, 1, 53), -180, "en-GB")).toBe("00:01");
    expect(timeInZone(Date.UTC(2026, 10, 27, 3, 1, 53), 345, "en-GB")).toBe("08:46");
  });

  it("uses the console's zone on the console only, not on another device", () => {
    setConsoleZone(RECIFE, false);
    expect(formatDateTime(MADE, "pt-BR")).toBe("25/11/2026, 10:25:59");
  });
});
