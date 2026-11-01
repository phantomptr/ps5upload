import { describe, expect, it } from "vitest";
import type { ProfileUser, SaveEntry } from "../../api/ps5";
import { canCopyToOtherUser, copyTargets, localUserCount } from "./copyTargets";

const A = 0x10000001;
const B = 0x20000002;
const C = 0x30000003;
const user = (uid: number, username: string): ProfileUser => ({ uid, uid_hex: uid.toString(16), username });
const save = (user_id: number, title_id = "PPSA01650", kind: SaveEntry["kind"] = "ps5"): SaveEntry => ({
  title_id,
  user_id,
  path: `/user/home/${user_id.toString(16)}/savedata_prospero/${title_id}`,
  size: 1,
  mtime: 1,
  kind,
});

describe("Copy to another user: who it can go to", () => {
  const users = [user(A, "Ann"), user(B, "Ben"), user(C, "Cy")];

  it("lists the other users, those with a save of the game first and pickable", () => {
    const saves = [save(A), save(C), save(B, "PPSA09999")];
    const t = copyTargets(users, saves, "ppsa01650", A);
    expect(t.map((x) => [x.name, !!x.save])).toEqual([
      ["Cy", true],
      ["Ben", false],
    ]);
    expect(t[0].save?.path).toContain(C.toString(16));
  });

  it("a PS4 save of the game is not a target", () => {
    const t = copyTargets(users, [save(A), save(B, "PPSA01650", "ps4")], "PPSA01650", A);
    expect(t.find((x) => x.uid === B)?.save).toBeNull();
  });

  it("an owner the profile list missed still shows, unnamed", () => {
    const t = copyTargets([user(A, "Ann")], [save(A), save(B)], "PPSA01650", A);
    expect(t).toEqual([{ uid: B, name: null, save: expect.objectContaining({ user_id: B }) }]);
  });

  it("a single-user console hides the action, and emulator saves never offer it", () => {
    expect(canCopyToOtherUser(save(A), [user(A, "Ann")], [save(A)])).toBe(false);
    expect(canCopyToOtherUser(save(A), users, [save(A)])).toBe(true);
    expect(canCopyToOtherUser(save(A, "PPSA99008", "switch"), users, [])).toBe(false);
    expect(localUserCount([user(A, "Ann")], [save(A)])).toBe(1);
    expect(localUserCount([user(A, "Ann")], [save(A), save(B)])).toBe(2);
  });
});
