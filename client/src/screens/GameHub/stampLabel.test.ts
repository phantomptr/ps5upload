// #928: backup stamps are UTC names; the label shows the device's local time.
// UTC-3 with no daylight saving (Brazil since 2019). Set before any Date use.
(globalThis as unknown as { process: { env: Record<string, string> } }).process.env.TZ = "America/Sao_Paulo";

import { describe, expect, it } from "vitest";

import { stampLabel } from "./SaveBackupsList";
import { stampDate } from "../Saves/FolderBackupChoice";

describe("backup times in the device's time zone (#928)", () => {
  it("shows 03:08 UTC as 00:08 local", () => {
    expect(new Date(Date.UTC(2026, 10, 25)).getTimezoneOffset()).toBe(180);
    const label = stampLabel("2026-11-25_030800", "en");
    expect(label).toBe("Nov 25, 2026, 12:08 AM");
    expect(label).not.toContain("UTC");
  });

  it("moves to the previous day when local midnight hasn't passed", () => {
    expect(stampLabel("2026-11-25_020000", "en")).toBe("Nov 24, 2026, 11:00 PM");
  });

  it("drops a snapshot suffix and uses the app's language", () => {
    expect(stampLabel("2026-11-25_030800_snap", "en")).toBe("Nov 25, 2026, 12:08 AM");
    expect(stampLabel("2026-11-25_150800", "de")).toBe("25. Nov. 2026, 12:08");
  });

  it("parses the stamp as UTC for the folder backups list too", () => {
    expect(stampDate("2026-11-25_030800")?.toISOString()).toBe("2026-11-25T03:08:00.000Z");
  });

  it("leaves anything that isn't a stamp as is", () => {
    expect(stampLabel("odd")).toBe("odd");
    expect(stampLabel("2026-11-25_0308")).toBe("2026-11-25_0308");
  });
});
