import { describe, expect, it } from "vitest";
import type { SaveEntry } from "../../api/ps5";
import { consoleTargets, isEmulatorSave } from "./CopyToConsoleDialog";

const save = (over: Partial<SaveEntry>): SaveEntry =>
  ({ title_id: "PPSA1", user_id: 5, path: "/user/home/5/savedata_prospero/PPSA1", size: 1, mtime: 1, kind: "ps5", ...over }) as SaveEntry;

describe("where a save can go on another console", () => {
  it("a PS5 save: every user there, those with a save of the game first and only those pickable", () => {
    const t = consoleTargets(
      save({}),
      [save({ user_id: 9, path: "/user/home/9/savedata_prospero/PPSA1" })],
      [
        { uid: 8, uid_hex: "8", username: "Bob" },
        { uid: 9, uid_hex: "9", username: "Ann" },
      ],
      "same",
    );
    expect(t.map((x) => [x.name, !!x.save])).toEqual([
      ["Ann", true],
      ["Bob", false],
    ]);
  });
  it("the same user number on the other console is a choice too: it's another console", () => {
    const t = consoleTargets(save({}), [save({})], [{ uid: 5, uid_hex: "5", username: "Ann" }], "same");
    expect(t.map((x) => x.name)).toEqual(["Ann"]);
  });
  it("an emulator's save: the folders of that emulator there, else the same place", () => {
    const e = save({ kind: "switch", path: "/data/eden/profile-a/game" });
    expect(isEmulatorSave(e)).toBe(true);
    expect(consoleTargets(e, [], [], "same")).toEqual([{ uid: 5, name: "same", save: e }]);
    const there = save({ kind: "switch", user_id: 2, path: "/data/eden/profile-b/game" });
    const other = save({ kind: "ps2", path: "/x" });
    expect(consoleTargets(e, [there, other], [], "same")).toEqual([{ uid: 2, name: "game", save: there }]);
  });
  it("a PS4 save isn't an emulator's", () => {
    expect(isEmulatorSave(save({ kind: "ps4" }))).toBe(false);
  });
});
