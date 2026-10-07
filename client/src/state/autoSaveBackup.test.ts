import { describe, expect, it } from "vitest";
import { clampKeep, savesNeedingBackup, SETTLE_SECS } from "./autoSaveBackup";
import type { SaveEntry } from "../api/ps5";

const NOW = 2_000_000_000;
const save = (path: string, mtime: number): SaveEntry => ({
  title_id: "CUSA00900",
  user_id: 0x1234,
  path,
  size: 1,
  mtime,
  kind: "ps4",
});

describe("automatic save backup", () => {
  it("backs up a save that changed since its last backup, and one never backed up", () => {
    const a = save("/a", NOW - 3600);
    const b = save("/b", NOW - 3600);
    const c = save("/c", NOW - 3600);
    const done = { "ps5|/a": NOW - 3600, "ps5|/b": NOW - 7200 };
    expect(savesNeedingBackup("ps5", [a, b, c], done, NOW).map((e) => e.path)).toEqual(["/b", "/c"]);
  });

  it("waits for a save that was written a moment ago", () => {
    const fresh = save("/a", NOW - (SETTLE_SECS - 1));
    const settled = save("/b", NOW - SETTLE_SECS);
    expect(savesNeedingBackup("ps5", [fresh, settled], {}, NOW).map((e) => e.path)).toEqual(["/b"]);
  });

  it("keeps each console's record apart, and ignores a save with no time", () => {
    const a = save("/a", NOW - 3600);
    expect(savesNeedingBackup("other", [a], { "ps5|/a": NOW - 3600 }, NOW)).toHaveLength(1);
    expect(savesNeedingBackup("ps5", [save("/z", 0)], {}, NOW)).toHaveLength(0);
  });

  it("keeps between 1 and 50 versions", () => {
    expect(clampKeep(0)).toBe(1);
    expect(clampKeep(500)).toBe(50);
    expect(clampKeep(undefined)).toBe(5);
    expect(clampKeep(3.4)).toBe(3);
  });
});
