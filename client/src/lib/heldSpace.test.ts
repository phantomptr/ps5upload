import { describe, expect, it } from "vitest";

import type { SpaceCheck } from "../api/cleanup";
import type { Volume } from "../api/ps5";
import { checkableDrives, heldSpaceLine } from "./heldSpace";

const vol = (path: string, fs_type: string, extra: Partial<Volume> = {}): Volume => ({
  path,
  fs_type,
  total_bytes: 2_000_372_695_040,
  free_bytes: 1_986_485_026_816,
  writable: true,
  ...extra,
});

/** Echoes the key and its values, so a test sees which line was chosen and what filled it. */
const t = (key: string, vars?: Record<string, string | number>) =>
  vars ? `${key} ${Object.entries(vars).map(([k, v]) => `${k}=${v}`).join(" ")}` : key;

const report = (over: Partial<SpaceCheck>): SpaceCheck => ({
  path: "/mnt/ext1",
  fs_type: "bfs",
  total_bytes: 2_000_372_695_040,
  used_bytes: 18_194_825_216,
  files_bytes: 13_882_425_344,
  files: 1,
  dirs: 7,
  skipped: 0,
  unaccounted_bytes: 4_312_399_872,
  verdict: "held",
  elapsed_ms: 900,
  ...over,
});

describe("space no file uses (#979)", () => {
  it("is offered for M.2 and USB drives, never internal storage or a second view of a drive", () => {
    const drives = checkableDrives([
      vol("/data", "nullfs"),
      vol("/mnt/ext1", "bfs"),
      vol("/mnt/usb0", "exfatfs"),
      vol("/mnt/original_m2", "nullfs"),
      vol("/mnt/ext0", "tmpfs", { is_placeholder: true }),
      vol("/mnt/ps5upload/game", "exfatfs"),
    ]);
    expect(drives.map((d) => d.path)).toEqual(["/mnt/ext1", "/mnt/usb0"]);
  });

  it("says how much is held and asks for the explanation only then", () => {
    const held = heldSpaceLine(t, report({}));
    expect(held.found).toBe(true);
    expect(held.text).toContain("cleanup_held_found");
    expect(held.text).toContain("drive=cw_cleanup_drive_ext n=2");

    const clear = heldSpaceLine(t, report({ verdict: "clear", unaccounted_bytes: 5 << 20 }));
    expect(clear).toEqual({ text: "cleanup_held_clear drive=cw_cleanup_drive_ext n=2", found: false });
  });

  it("does not call space lost when folders were skipped or the drive changed", () => {
    const partial = heldSpaceLine(t, report({ verdict: "partial", skipped: 3 }));
    expect(partial.found).toBe(false);
    expect(partial.text).toContain("cleanup_held_partial");
    expect(partial.text).toContain("count=3");
    expect(heldSpaceLine(t, report({ verdict: "busy" })).text).toContain("cleanup_held_busy");
  });
});
