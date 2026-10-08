import { describe, expect, it } from "vitest";
import type { Volume } from "../api/ps5";
import { planMove, quickDestinations, volumeOf } from "./moveTo";

const vol = (path: string, free: number, likely = free): Volume => ({
  path,
  fs_type: "ufs",
  total_bytes: free * 2,
  free_bytes: free,
  allocatable_bytes: free,
  likely_fits_bytes: likely,
  writable: true,
});
const vols = [vol("/data", 100, 80), vol("/mnt/ext0", 500), vol("/mnt/ext0/sub", 10)];

describe("volumeOf", () => {
  it("picks the longest mount that contains the path", () => {
    expect(volumeOf("/data/homebrew/a", vols)?.path).toBe("/data");
    expect(volumeOf("/mnt/ext0/x", vols)?.path).toBe("/mnt/ext0");
    expect(volumeOf("/mnt/ext0/sub/y", vols)?.path).toBe("/mnt/ext0/sub");
    expect(volumeOf("/mnt/ext00/z", vols)).toBeNull();
  });
});

describe("planMove", () => {
  it("is a rename on one drive, with nothing to copy", () => {
    const p = planMove([{ path: "/data/homebrew/a", size: 999 }], "/data/games", vols);
    expect(p.crossDrive).toBe(false);
    expect(p.bytes).toBe(0);
    expect(p.fits).toBe("yes");
  });

  it("refuses only what certainly won't fit, and warns when the PS5 may hold space back", () => {
    const to = (size: number) =>
      planMove([{ path: "/mnt/ext0/a", size }], "/data/homebrew", vols).fits;
    expect(to(50)).toBe("yes");
    expect(to(90)).toBe("tight");
    expect(to(101)).toBe("no");
  });

  it("catches a folder moved into itself, and a move to where it already is", () => {
    expect(planMove([{ path: "/data/a", size: 1 }], "/data/a/b", vols).intoItself).toBe(true);
    expect(planMove([{ path: "/data/a", size: 1 }], "/data", vols).alreadyThere).toBe(true);
    expect(planMove([{ path: "/data/a", size: 1 }], "/data/ab", vols).intoItself).toBe(false);
  });
});

it("offers each drive's homebrew folder", () => {
  expect(quickDestinations([vol("/data", 1), vol("/mnt/usb0/", 1)]).map((d) => d.path)).toEqual([
    "/data/homebrew",
    "/mnt/usb0/homebrew",
  ]);
});
