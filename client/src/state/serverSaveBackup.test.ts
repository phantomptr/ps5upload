import { describe, expect, it } from "vitest";

import { clampHours, isChosen, toggleChosen, toggleUser } from "./serverSaveBackup";
import { mergeBackups } from "../screens/GameHub/SaveBackupsList";
import type { SaveBackupFile } from "../api/saveBackup";

describe("server save backup choices", () => {
  const all = ["10.0.0.1", "10.0.0.2", "10.0.0.3"];

  it("treats an empty list as everything chosen", () => {
    expect(isChosen([], "10.0.0.9")).toBe(true);
    expect(isChosen(["10.0.0.1"], "10.0.0.2")).toBe(false);
  });

  it("unticking one from all keeps the rest; ticking it back is all again", () => {
    const some = toggleChosen([], "10.0.0.2", false, all);
    expect(some).toEqual(["10.0.0.1", "10.0.0.3"]);
    expect(toggleChosen(some, "10.0.0.2", true, all)).toEqual([]);
  });

  it("never leaves nothing chosen", () => {
    expect(toggleChosen(["10.0.0.1"], "10.0.0.1", false, all)).toEqual(["10.0.0.1"]);
  });

  it("drops a console that is no longer in the list", () => {
    expect(toggleChosen(["10.0.0.1", "10.0.0.9"], "10.0.0.2", true, all)).toEqual(["10.0.0.1", "10.0.0.2"]);
  });

  it("a user ticked or unticked on one console leaves the other consoles' users alone", () => {
    // The engine's list names users of two consoles: A (1, 2, 3, shown) and B (9).
    const shownA = [1, 2, 3];
    expect(toggleUser([1, 2, 9], 2, false, shownA)).toEqual([1, 9]);
    expect(toggleUser([1, 9], 3, true, shownA)).toEqual([1, 9, 3]);
    // Every user of A ticked, B's still listed: stays a list (B's other users stay left out).
    expect(toggleUser([1, 2, 9], 3, true, shownA)).toEqual([1, 2, 9, 3]);
    // The last of A's users stays ticked.
    expect(toggleUser([1, 9], 1, false, shownA)).toEqual([1, 9]);
    // B's users only listed (none of A's): ticking one of A's adds it.
    expect(toggleUser([9], 2, true, shownA)).toEqual([9, 2]);
  });

  it("one console's users only: as before, every user ticked is \"every user\"", () => {
    expect(toggleUser([], 2, false, [1, 2, 3])).toEqual([1, 3]);
    expect(toggleUser([1, 3], 2, true, [1, 2, 3])).toEqual([]);
  });

  it("clamps the hours", () => {
    expect(clampHours(0)).toBe(1);
    expect(clampHours(1000)).toBe(168);
    expect(clampHours(Number.NaN)).toBe(6);
  });
});

describe("mergeBackups", () => {
  const b = (stamp: string): SaveBackupFile => ({
    file: `T_user00000001_${stamp}.zip`,
    title_id: "T",
    user_id: 1,
    stamp,
    size: 1,
  });

  it("lists local and server backups together, newest first, marking the server ones", () => {
    const merged = mergeBackups([b("2026-10-01_000000"), b("2026-10-03_000000")], [b("2026-10-02_000000")]);
    expect(merged.map((x) => [x.stamp, x.server === true])).toEqual([
      ["2026-10-03_000000", false],
      ["2026-10-02_000000", true],
      ["2026-10-01_000000", false],
    ]);
  });
});
