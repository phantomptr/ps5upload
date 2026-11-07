import { describe, expect, it } from "vitest";

import { shownUnder } from "./SaveBackupsList";

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
