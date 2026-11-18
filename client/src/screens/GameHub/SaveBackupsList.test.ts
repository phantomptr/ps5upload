import { describe, expect, it } from "vitest";

import { backupPlace, mergeBackups, shownUnder } from "./SaveBackupsList";

describe("backups on the PS5's own drive", () => {
  const b = (stamp: string) => ({
    file: `T_user00000001_${stamp}.zip`,
    title_id: "T",
    user_id: 1,
    stamp,
    size: 1,
  });

  it("lists the copies on the PS5 drive with the others, marked, the local one first on a tie", () => {
    const merged = mergeBackups(
      [b("2026-11-17_101500")],
      [],
      [b("2026-11-17_101500"), b("2026-11-01_000000")],
      "usb",
    );
    expect(merged.map((x) => [x.stamp, backupPlace(x), x.console ?? null])).toEqual([
      ["2026-11-17_101500", "local", null],
      ["2026-11-17_101500", "console", "usb"],
      ["2026-11-01_000000", "console", "usb"],
    ]);
    // The same file here and on the PS5 gets two keys.
    expect(new Set(merged.map((x) => `${backupPlace(x)}:${x.file}`)).size).toBe(3);
    expect(mergeBackups([], [], [b("2026-11-01_000000")])[0].console).toBe("ext");
  });
});

describe("shownUnder", () => {
  const save = { user_id: 0x1000_0001, kind: "ps5" as const };
  const users = [{ uid: 0x1000_0001 }, { uid: 0x1000_0002 }];

  it("shows a save's own backups, and not another user's who is still on the console", () => {
    expect(shownUnder(save, users, 0x1000_0001)).toBe(true);
    expect(shownUnder(save, users, 0x1000_0002)).toBe(false);
  });

  // Discord: after a reset (or a crash and signing in again) the user's ID is new, and the
  // automatic backups made under the old one showed nowhere, so they could not be restored.
  it("shows a backup of a user this console no longer has under a PS5 save", () => {
    expect(shownUnder(save, users, 0x1abc_def0)).toBe(true);
    // A user ID above 2^31 (negative as an i32 on the console) is the same user.
    expect(shownUnder({ user_id: -1, kind: "ps5" }, [{ uid: 0xffff_ffff }], 0xffff_ffff)).toBe(true);
  });

  it("keeps an emulator's or unknown console's backups to their own user", () => {
    expect(shownUnder({ user_id: 0, kind: "ps2" }, users, 1)).toBe(false);
    expect(shownUnder(save, [], 0x1abc_def0)).toBe(false);
  });
});
