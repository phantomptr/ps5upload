import { describe, expect, it } from "vitest";
import { groupsInBackup, matchUser, usersWithSaves, type BackupManifest } from "./consoleBackup";

const manifest: BackupManifest = {
  format: 1,
  app_version: "11.3.0",
  made_at_ms: 0,
  host: "h",
  complete: true,
  groups: [
    { id: "settings", bytes: 0, items: 0, folders: [{ path: "/data/etaHEN", bytes: 0, files: 0 }] },
    { id: "other", bytes: 0, items: 0, folders: [] },
  ],
  saves: [
    { title_id: "A", user_id: 5, path: "", file: "", bytes: 0 },
    { title_id: "B", user_id: 5, path: "", file: "", bytes: 0 },
    { title_id: "C", user_id: 6, path: "", file: "", bytes: 0 },
  ],
  users: [{ uid: 5, name: "Ann" }],
  failed: [],
};

describe("what a backup holds", () => {
  it("offers only groups with something in them", () => {
    expect(groupsInBackup(manifest)).toEqual(["saves", "settings"]);
  });
  it("names each user with saves once", () => {
    expect(usersWithSaves(manifest)).toEqual([
      { uid: 5, name: "Ann" },
      { uid: 6, name: "" },
    ]);
  });
});

describe("matching a backup's user to one here", () => {
  const here = [
    { uid: 9, username: "ann " },
    { uid: 6, username: "Bob" },
  ];
  it("takes the same number first", () => {
    expect(matchUser({ uid: 6, name: "Ann" }, here)).toBe(6);
  });
  it("then the same name, as a reset console has new numbers", () => {
    expect(matchUser({ uid: 5, name: "Ann" }, here)).toBe(9);
  });
  it("and doesn't guess otherwise", () => {
    expect(matchUser({ uid: 5, name: "Cid" }, here)).toBeNull();
    expect(matchUser({ uid: 5, name: "" }, here)).toBeNull();
  });
});
