import { describe, expect, it } from "vitest";
import { backupStamp, inBackupFolder } from "./backupStamp";

describe("backupStamp", () => {
  it("names a backup by its local date and minute, sortable", () => {
    expect(backupStamp(new Date(2026, 9, 4, 8, 5))).toBe("20261004-0805");
    expect(backupStamp(new Date(2026, 11, 31, 23, 59))).toBe("20261231-2359");
  });
});

describe("inBackupFolder", () => {
  it("suggests a manual backup in the save backup folder, in that folder's own spelling", () => {
    expect(inBackupFolder("D:\\Pedro\\PS5\\Saves\\", "Game.zip")).toBe("D:\\Pedro\\PS5\\Saves\\Game.zip");
    expect(inBackupFolder("/home/me/saves/", "Game.zip")).toBe("/home/me/saves/Game.zip");
    expect(inBackupFolder("", "Game.zip")).toBe("Game.zip");
    expect(inBackupFolder(undefined, "Game.zip")).toBe("Game.zip");
  });
});
