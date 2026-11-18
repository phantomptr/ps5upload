import { describe, expect, it, vi } from "vitest";

import type { TrophyFixResult } from "../../api/ps5";
import {
  activationTrophyNote,
  applyTrophies,
  checkTrophies,
  isGameRunning,
  undoTrophies,
  type TrophyRepairApi,
} from "./trophyRepairFlow";

const PATH = "trophy2/nobackup/data/NPWR20561_00/TRPTITLE.DAT";

function result(over: Partial<TrophyFixResult>): TrophyFixResult {
  return {
    ok: true,
    uid: 0x1ea2f4da,
    dry_run: true,
    old: null,
    new: "0x99aabbccddeeff01",
    files: [],
    more: false,
    backup: "",
    scanned: 0,
    changed: 0,
    already: 0,
    left: 0,
    failed: 0,
    ...over,
  };
}

function api(fix: TrophyRepairApi["fix"], restore?: TrophyRepairApi["restore"]): TrophyRepairApi {
  return { fix, restore: restore ?? vi.fn() };
}

describe("Repair trophies", () => {
  it("checks first with a dry run and lists only the files that would change", async () => {
    const fix = vi.fn(async (dry: boolean) =>
      result({
        dry_run: dry,
        changed: 1,
        already: 1,
        files: [
          { path: PATH, state: "would_change", from: "0x0000000000000000", anchor: true, extra: 0 },
          { path: "trophy2/x/OTHER.DAT", state: "unknown_format", from: "0x0", anchor: false, extra: 1 },
        ],
      }),
    );
    const step = await checkTrophies(api(fix));
    expect(fix).toHaveBeenCalledTimes(1);
    expect(fix).toHaveBeenCalledWith(true);
    expect(step).toEqual({
      kind: "preview",
      count: 1,
      more: false,
      files: [expect.objectContaining({ path: PATH })],
    });
  });

  it("says nothing needs changing when the check finds nothing", async () => {
    const step = await checkTrophies(api(async () => result({ already: 3 })));
    expect(step).toEqual({ kind: "nothing" });
  });

  it("applies with dry_run false and keeps the backup's name for an undo", async () => {
    const fix = vi.fn(async () => result({ dry_run: false, changed: 2, backup: "20261117-120000" }));
    const step = await applyTrophies(api(fix));
    expect(fix).toHaveBeenCalledWith(false);
    expect(step).toEqual({ kind: "done", changed: 2, failed: 0, backup: "20261117-120000" });
  });

  it("puts the backup back", async () => {
    const restore = vi.fn(async () => ({ restored: 2, failed: 0 }));
    const step = await undoTrophies(api(vi.fn(), restore), "20261117-120000");
    expect(restore).toHaveBeenCalledWith("20261117-120000");
    expect(step).toEqual({ kind: "undone", restored: 2, failed: 0 });
  });

  // Update, start a game to see whether trophies work, then Undo: the PS5 refuses while the game
  // is open. The error kept no backup name and offered only Back, to a check that finds nothing
  // to change: the undo could not be made at all.
  it("an undo that was refused keeps its backup, so it can be made again", async () => {
    const restore = vi
      .fn<TrophyRepairApi["restore"]>()
      .mockRejectedValueOnce(new Error("game_running: close PPSA01922 on the PS5 first"))
      .mockResolvedValueOnce({ restored: 2, failed: 0 });
    const a = api(vi.fn(), restore);
    const refused = await undoTrophies(a, "20261117-120000");
    expect(refused).toMatchObject({ kind: "error", gameRunning: true, backup: "20261117-120000" });
    const again = await undoTrophies(a, (refused as { backup: string }).backup);
    expect(again).toEqual({ kind: "undone", restored: 2, failed: 0 });
    // An error of the check or the update has no undo to offer.
    const other = await applyTrophies(api(async () => Promise.reject(new Error("no_such_user"))));
    expect((other as { backup?: string }).backup).toBeUndefined();
  });

  it("an undo that left files behind says so and can be made again", async () => {
    const step = await undoTrophies(api(vi.fn(), async () => ({ restored: 1, failed: 2 })), "b1");
    expect(step).toEqual({ kind: "undone", restored: 1, failed: 2, backup: "b1" });
  });

  it("tells a running game apart from other errors", async () => {
    const step = await applyTrophies(
      api(async () => {
        throw new Error("game_running: close PPSA01922 on the PS5 first");
      }),
    );
    expect(step).toMatchObject({ kind: "error", gameRunning: true });
    expect(isGameRunning("payload rejected TROPHY_FIX: no_such_user")).toBe(false);
  });
});

describe("activationTrophyNote", () => {
  it("is quiet when nothing was asked of the trophy files or nothing changed", () => {
    expect(activationTrophyNote(null)).toBeNull();
    expect(activationTrophyNote(undefined)).toBeNull();
    expect(activationTrophyNote({ changed: 0, failed: 0, backup: "" })).toBeNull();
  });

  it("reports the files updated", () => {
    expect(activationTrophyNote({ changed: 3, failed: 0, backup: "x" })).toEqual({
      kind: "updated",
      changed: 3,
      failed: 0,
    });
  });

  it("reports a skip for a running game and an error", () => {
    expect(activationTrophyNote({ skipped: "game_running", title_id: "PPSA01922" })).toEqual({
      kind: "game_running",
    });
    expect(activationTrophyNote({ error: "trophy_fix_unsupported: old helper" })).toEqual({
      kind: "error",
      message: "trophy_fix_unsupported: old helper",
    });
  });
});
