// #928 on the console's own browser: its clock reads UTC whatever zone the PS5 is set to. Set
// before any Date use.
(globalThis as unknown as { process: { env: Record<string, string> } }).process.env.TZ = "UTC";

import { afterEach, describe, expect, it } from "vitest";

import { backupName, parseBackupName } from "./saves/model";
import { formatDate, formatDateTime, setConsoleZone } from "./time";

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

  it("uses the console's zone on the console only, not on another device", () => {
    setConsoleZone(RECIFE, false);
    expect(formatDateTime(MADE, "pt-BR")).toBe("25/11/2026, 10:25:59");
  });
});
