import { describe, expect, it } from "vitest";

import type { Volume } from "../api/ps5";
import { planConsoleSend } from "./consoleSend";

const vol = (path: string, free: number): Volume =>
  ({ path, fs_type: "exfatfs", total_bytes: free * 2, free_bytes: free, writable: true }) as Volume;

describe("planConsoleSend", () => {
  const items = [
    { path: "/data/homebrew/A-app", name: "A-app", size: 0, isDir: true },
    { path: "/data/homebrew/b.ffpfsc", name: "b.ffpfsc", size: 4_000, isDir: false },
  ];

  it("puts each item in the folder picked on the other console", () => {
    const p = planConsoleSend(items, "/mnt/ext0/homebrew", [vol("/mnt/ext0", 1e9)], []);
    expect(p.dests).toEqual(["/mnt/ext0/homebrew/A-app", "/mnt/ext0/homebrew/b.ffpfsc"]);
    expect(p.fits).toBe("yes");
  });

  it("names what is already there and will be replaced", () => {
    const p = planConsoleSend(items, "/data/homebrew", [vol("/data", 1e9)], ["b.ffpfsc", "other"]);
    expect(p.replacing).toEqual(["b.ffpfsc"]);
  });

  it("refuses files that certainly do not fit, and says so before anything runs", () => {
    const p = planConsoleSend(items, "/data/homebrew", [vol("/data", 1_000)], []);
    expect(p.fits).toBe("no");
  });

  it("cannot judge the size of a folder it has not measured", () => {
    const p = planConsoleSend([items[0]], "/data", [vol("/data", 10)], []);
    expect(p.fits).toBe("unknown");
    expect(p.bytes).toBe(0);
  });

  it("joins the root without a double slash", () => {
    expect(planConsoleSend([items[1]], "/", [], []).dests).toEqual(["/b.ffpfsc"]);
  });
});
