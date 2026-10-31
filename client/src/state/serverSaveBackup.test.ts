import { describe, expect, it } from "vitest";

import { clampHours, isChosen, toggleChosen } from "./serverSaveBackup";
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
