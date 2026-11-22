import { describe, expect, it } from "vitest";

import type { CleanupApplyStatus, CleanupItem } from "../api/cleanup";
import {
  driveParts,
  groupItems,
  groupState,
  initialSelection,
  isRemovable,
  parentDir,
  selectionTotals,
  setGroup,
  summarizeApply,
  toggle,
} from "./cleanupModel";

function item(over: Partial<CleanupItem> & Pick<CleanupItem, "id" | "cat">): CleanupItem {
  return {
    path: `/data/x${over.id}.pkg`,
    drive: "internal",
    state: "game_installed",
    bytes: 100,
    alloc_bytes: 128,
    files: 1,
    is_dir: false,
    mtime: 0,
    default: false,
    action: "delete",
    ...over,
  };
}

const items: CleanupItem[] = [
  item({ id: 1, cat: "A", action: "sony", alloc_bytes: 70e9, path: "/user/patch/PPSA1", default: false }),
  item({ id: 2, cat: "B", action: "sony", alloc_bytes: 10e9, default: true }),
  item({ id: 3, cat: "C", alloc_bytes: 5e9, default: true }),
  item({ id: 4, cat: "C", alloc_bytes: 9e9, drive: "usb0", default: false }),
  item({ id: 5, cat: "G", action: "none", alloc_bytes: 100e9, default: true }),
  item({ id: 6, cat: "D", alloc_bytes: 0, bytes: 42, default: true }),
];

describe("cleanup model", () => {
  it("groups in category order, largest first, without empty groups", () => {
    const g = groupItems(items);
    expect(g.map((x) => x.cat)).toEqual(["A", "B", "C", "D", "G"]);
    expect(g[2].items.map((i) => i.id)).toEqual([4, 3]);
    expect(g[2].bytes).toBe(14e9);
    // no space figure: its size counts
    expect(g[3].bytes).toBe(42);
  });

  it("ticks the defaults it can remove, never a 'worth a look' item", () => {
    expect([...initialSelection(items)].sort()).toEqual([2, 3, 6]);
    expect(isRemovable(items[4])).toBe(false);
    // Kraken Mount's games (#894): listed, never ticked, even if an older helper says "delete"
    const kept = item({ id: 9, cat: "C", default: true, kept: "kraken" });
    expect(isRemovable(kept)).toBe(false);
    expect(initialSelection([kept]).size).toBe(0);
    expect(selectionTotals([kept], new Set([9])).count).toBe(0);
  });

  it("totals the selection per drive and flags downgrades", () => {
    const t = selectionTotals(items, new Set([1, 3, 4, 5]));
    expect(t.count).toBe(3);
    expect(t.bytes).toBe(84e9);
    expect(t.byDrive).toEqual([
      { drive: "internal", bytes: 75e9 },
      { drive: "usb0", bytes: 9e9 },
    ]);
    expect(t.downgrades.map((i) => i.id)).toEqual([1]);
  });

  it("selects and clears a whole group", () => {
    const c = groupItems(items).find((g) => g.cat === "C")!;
    expect(groupState(c, new Set([3]))).toBe("some");
    const all = setGroup(c, new Set([1]), true);
    expect(groupState(c, all)).toBe("all");
    expect(all.has(1)).toBe(true);
    expect(groupState(c, setGroup(c, all, false))).toBe("none");
    expect(toggle(new Set([1]), 1).size).toBe(0);
  });

  it("names the items an apply removed, skipped and failed", () => {
    const st: CleanupApplyStatus = {
      ok: true,
      apply_id: 9,
      scan_id: 1,
      state: "done",
      current: 0,
      phase: "done",
      total: 3,
      done: [3],
      skipped: [{ id: 2, reason: "changed" }],
      failed: [{ id: 1, reason: "sony_refused", code: "0x80990001" }, { id: 99, reason: "x" }],
      freed_bytes: 5e9,
    };
    const s = summarizeApply(st, items);
    expect(s.removed.map((i) => i.id)).toEqual([3]);
    expect(s.skipped[0].reason).toBe("changed");
    expect(s.failed).toHaveLength(1);
    expect(s.failed[0].code).toBe("0x80990001");
    expect(s.freed).toBe(5e9);
  });

  it("splits drive tokens and paths", () => {
    expect(driveParts("ext1")).toEqual({ kind: "ext", n: 1 });
    expect(driveParts("usb3")).toEqual({ kind: "usb", n: 3 });
    expect(driveParts("internal").kind).toBe("internal");
    expect(parentDir("/data/pkg/a.pkg")).toBe("/data/pkg");
  });
});
