import { describe, expect, it } from "vitest";

import { buildFileListRows_forTest } from "./index";
import type { PlannedFile } from "../../api/ps5";

// Synthesise N planned files with stable per-index size so equality
// assertions on slices stay readable.
function fakeFiles(n: number): PlannedFile[] {
  const out: PlannedFile[] = [];
  for (let i = 0; i < n; i++) {
    out.push({ rel_path: `file-${i}.bin`, size: (i + 1) * 1024 } as PlannedFile);
  }
  return out;
}

describe("buildFileListRows_forTest", () => {
  it("returns [] for an empty list", () => {
    expect(buildFileListRows_forTest([], 0)).toEqual([]);
  });

  it("renders every row when total ≤ cap, in current → pending → done-rev order", () => {
    // 5 files, completed = 2 → current is index 2.
    // Expected order: [2 (current), 3, 4 (pending), 1, 0 (done-rev)].
    const rows = buildFileListRows_forTest(fakeFiles(5), 2, /* cap */ 200);
    expect(rows.map((r) => r.idx)).toEqual([2, 3, 4, 1, 0]);
  });

  it("windowed mode kicks in above the cap, keeps total rows ≤ cap", () => {
    // Default cap is 200. A 50,000-file upload at completed=25,000 was
    // the user-reported case that froze the UI.
    const rows = buildFileListRows_forTest(fakeFiles(50_000), 25_000);
    expect(rows.length).toBeLessThanOrEqual(200);
    // Current file must always be the first row so the "▶" indicator
    // stays pinned to the visible top of the panel.
    expect(rows[0]?.idx).toBe(25_000);
  });

  it("windowed mode leans toward pending over done (2:1 split of remaining slots)", () => {
    // ~199 remaining slots after the current row; ceil(199*2/3) = 133
    // pending, 66 done.
    const rows = buildFileListRows_forTest(fakeFiles(50_000), 25_000, 200);
    const currentIdx = rows[0]!.idx;
    const pending = rows.filter((r) => r.idx > currentIdx);
    const done = rows.filter((r) => r.idx < currentIdx);
    expect(pending.length).toBeGreaterThan(done.length);
    expect(pending.length + done.length + 1).toBe(rows.length);
  });

  it("windowed pending slice starts at completed+1 and is contiguous", () => {
    const rows = buildFileListRows_forTest(fakeFiles(50_000), 25_000, 200);
    const currentIdx = rows[0]!.idx;
    const pending = rows.filter((r) => r.idx > currentIdx);
    // Pending must be the immediate next-up window (25_001, 25_002, …),
    // not a random subset — UX contract: "what's coming up right now".
    pending.forEach((r, i) => expect(r.idx).toBe(currentIdx + 1 + i));
  });

  it("windowed done slice goes most-recent-first immediately below current", () => {
    const rows = buildFileListRows_forTest(fakeFiles(50_000), 25_000, 200);
    const currentIdx = rows[0]!.idx;
    const done = rows.filter((r) => r.idx < currentIdx);
    // Done must be most-recent-first (25_000-1, 25_000-2, …) for the
    // "scroll down to see what just finished" mental model.
    done.forEach((r, i) => expect(r.idx).toBe(currentIdx - 1 - i));
  });

  it("clamps completed to [0, total-1] so a stale snap doesn't crash the panel", () => {
    // Engine has been known to briefly report files_processing > N
    // during a packed-shard burst (the comment in transfer.ts:460
    // documents this). The panel must not throw or render garbage.
    expect(() => buildFileListRows_forTest(fakeFiles(5), 999)).not.toThrow();
    const rows = buildFileListRows_forTest(fakeFiles(5), 999);
    // Last index becomes current; "done" rows are 0..3, no pending.
    expect(rows[0]?.idx).toBe(4);
  });

  it("renders nothing past the end of the list when completed equals total", () => {
    // Engine reports completed === total during the bytes-done /
    // finalize phase. Don't emit a phantom "current" row past the end.
    const rows = buildFileListRows_forTest(fakeFiles(5), 5);
    // All five are "done"; no current, no pending. Order is rev-done.
    expect(rows.map((r) => r.idx)).toEqual([4, 3, 2, 1, 0]);
  });
});
