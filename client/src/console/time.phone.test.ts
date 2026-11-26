// #928 on a phone or a PC: the browser knows the viewer's zone, and that is the one shown.
// UTC-3, no daylight saving. Set before any Date use.
(globalThis as unknown as { process: { env: Record<string, string> } }).process.env.TZ = "America/Recife";

import { afterEach, describe, expect, it } from "vitest";

import { backupName, parseBackupName } from "./saves/model";
import { formatDate, formatDateTime, setConsoleZone } from "./time";

const MADE = Date.UTC(2026, 10, 25, 10, 25, 59);

afterEach(() => setConsoleZone(null, true));

describe("times in a browser that knows its zone (#928)", () => {
  it("shows the viewer's local time", () => {
    expect(new Date(MADE).getTimezoneOffset()).toBe(180);
    expect(formatDateTime(MADE, "pt-BR")).toBe("25/11/2026, 07:25:59");
    expect(formatDate(Date.UTC(2026, 9, 21, 1, 30), "pt-BR")).toBe("20/10/2026");
  });

  it("is never moved by the console's zone", () => {
    setConsoleZone(9 * 60, true);
    expect(formatDateTime(MADE, "pt-BR")).toBe("25/11/2026, 07:25:59");
  });

  it("reads a UTC-named backup as the same moment the console made it", () => {
    expect(backupName(new Date(MADE))).toBe("2026-11-25_10-25-59Z");
    expect(formatDateTime(parseBackupName("2026-11-25_10-25-59Z")!.when, "pt-BR")).toBe("25/11/2026, 07:25:59");
  });

  it("still reads an unmarked name as this device's local time, as it was written here", () => {
    expect(parseBackupName("2026-11-25_07-25-59")?.when).toBe(MADE);
  });
});
