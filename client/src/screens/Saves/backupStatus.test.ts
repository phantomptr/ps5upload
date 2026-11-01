import { describe, expect, it } from "vitest";
import { backupAgo, backupKey, stampMs } from "./backupStatus";

describe("Backed up … line", () => {
  const now = Date.UTC(2026, 10, 1, 12, 0, 0);
  it("reads a backup stamp as UTC, its tag ignored", () => {
    expect(stampMs("2026-11-01_100000")).toBe(Date.UTC(2026, 10, 1, 10, 0, 0));
    expect(stampMs("2026-11-01_100000_snap")).toBe(Date.UTC(2026, 10, 1, 10, 0, 0));
    expect(stampMs("nope")).toBeNull();
  });
  it("says how long ago, in the app's language", () => {
    expect(backupAgo("2026-11-01_100000", now, "en")).toBe("2 hours ago");
    expect(backupAgo("2026-10-31_120000", now, "en")).toBe("yesterday");
    expect(backupAgo("2026-11-01_115950", now, "en")).toBe("this minute");
    expect(backupAgo("bad", now, "en")).toBeNull();
    expect(backupAgo("2026-11-01_100000", now, "de")).toBe("vor 2 Stunden");
  });
  it("keys a game and user", () => {
    expect(backupKey("ppsa01650", 0x10000001)).toBe("PPSA01650:10000001");
    expect(backupKey("PPSA01650", -1)).toBe("PPSA01650:ffffffff");
  });
});
