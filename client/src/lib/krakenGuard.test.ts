import { afterEach, describe, expect, it, vi } from "vitest";

import type { KrakenStatus, SmpStatus } from "../api/ps5";
import { krakenGuard, krakenKeptPaths, resetKrakenGuardCache } from "./krakenGuard";
import { krakenKeepsPath, useMounterStore } from "./mounter";

function kraken(p: Partial<KrakenStatus> = {}): KrakenStatus {
  return {
    installed: true,
    running: true,
    api: true,
    version: "0.2.1",
    waiting: null,
    games: [],
    title_ids: [],
    ampr: null,
    settings: { inject_ampr: true, ampr_auto_update: true, scan_paths: [] },
    scan_roots: [],
    errors: [],
    ...p,
  };
}

function snapshot(k: KrakenStatus | undefined): SmpStatus {
  return {
    installed: false,
    running: false,
    config_ini: null,
    autotune_ini: null,
    debug_log_tail: null,
    mounted_images: [],
    errors: [],
    kraken: k,
  };
}

afterEach(() => {
  resetKrakenGuardCache();
  useMounterStore.setState({ byHost: {} });
});

describe("krakenKeepsPath (#894)", () => {
  it("keeps what its API lists, inside it, and a folder holding it", () => {
    const k = kraken({
      games: [
        { title_id: "PPSA01234", title_name: "", path: "/user/data/mine/Game.pkg", kind: "pkg", status: "ready" },
        { title_id: "PPSA05555", title_name: "", path: "/mnt/usb1/x/PPSA05555", kind: "folder", status: "duplicate" },
      ],
      scan_roots: ["/mnt/usb0/games"],
    });
    expect(krakenKeepsPath("/data/mine/Game.pkg", k)).toBe(true);
    expect(krakenKeepsPath("/mnt/usb1/x/PPSA05555/eboot.bin", k)).toBe(true);
    expect(krakenKeepsPath("/mnt/usb1/x", k, true)).toBe(true);
    expect(krakenKeepsPath("/mnt/usb1/x/other.pkg", k)).toBe(false);
  });

  it("keeps a package or folder directly in a folder it scans, one level deep", () => {
    const k = kraken({ scan_roots: ["/data/homebrew", "/mnt/usb0"] });
    expect(krakenKeepsPath("/data/homebrew/Game.pkg", k)).toBe(true);
    expect(krakenKeepsPath("/user/data/homebrew/GAME.PKG", k)).toBe(true);
    expect(krakenKeepsPath("/mnt/usb0/PPSA01234-app", k, true)).toBe(true);
    expect(krakenKeepsPath("/mnt/usb0/old/Game.pkg", k)).toBe(false);
    expect(krakenKeepsPath("/mnt/usb0/Game.pkg.part", k)).toBe(false);
    expect(krakenKeepsPath("/mnt/usb0/game.exfat", k)).toBe(false);
    expect(krakenKeepsPath("/data/ps5upload/pkg_library/Game.pkg", k)).toBe(false);
  });

  it("counts its folders while it is stopped, and its default ones when nothing is known", () => {
    const stopped = kraken({ running: false, api: false, scan_roots: [] });
    expect(krakenKeepsPath("/mnt/usb3/etaHEN/games/Game.pkg", stopped)).toBe(true);
    expect(krakenKeepsPath("/data/homebrew/Game.pkg", undefined)).toBe(true);
    expect(krakenKeepsPath("/mnt/ext1/Game.pkg", undefined)).toBe(true);
    // a console it never ran on: its folders are just folders
    const never = kraken({ installed: false, running: false, api: false });
    expect(krakenKeepsPath("/data/homebrew/Game.pkg", never)).toBe(false);
    expect(krakenKeepsPath("/data/homebrew/Game.pkg", null)).toBe(false);
  });
});

describe("krakenGuard", () => {
  it("reads the console once and answers many paths", async () => {
    const read = vi.fn(async () => snapshot(kraken({ scan_roots: ["/data/homebrew"] })));
    const keeps = await krakenGuard("192.168.1.9", read);
    expect(keeps("/data/homebrew/a.pkg")).toBe(true);
    expect(keeps("/data/elsewhere/a.pkg")).toBe(false);
    await krakenGuard("192.168.1.9", read);
    expect(read).toHaveBeenCalledTimes(1);
  });

  it("keeps its default folders when the console cannot be asked", async () => {
    const keeps = await krakenGuard("192.168.1.9", async () => {
      throw new Error("down");
    });
    expect(keeps("/mnt/usb0/homebrew/a.pkg")).toBe(true);
    expect(keeps("/data/ps5upload/pkg_library/a.pkg")).toBe(false);
  });
});

describe("krakenKeptPaths (Files' delete warning)", () => {
  it("does not ask the console for paths that cannot be its games", async () => {
    const read = vi.fn(async () => snapshot(kraken()));
    const kept = await krakenKeptPaths("192.168.1.9", [{ path: "/data/notes.txt", isDir: false }], read);
    expect(kept).toEqual([]);
    expect(read).not.toHaveBeenCalled();
  });

  it("uses the last snapshot when there is one", async () => {
    useMounterStore.getState().put("192.168.1.9", snapshot(kraken({ scan_roots: ["/mnt/usb0/mine"] })));
    const read = vi.fn(async () => snapshot(undefined));
    const kept = await krakenKeptPaths(
      "192.168.1.9",
      [
        { path: "/mnt/usb0/mine/a.pkg", isDir: false },
        { path: "/mnt/usb0/homebrew/b.pkg", isDir: false },
      ],
      read,
    );
    expect(kept).toEqual(["/mnt/usb0/mine/a.pkg"]);
    expect(read).not.toHaveBeenCalled();
  });

  it("asks the console when a path could be in its default folders", async () => {
    const read = vi.fn(async () => snapshot(kraken({ scan_roots: [] })));
    const kept = await krakenKeptPaths("192.168.1.9", [{ path: "/data/homebrew/PPSA01234", isDir: true }], read);
    expect(kept).toEqual(["/data/homebrew/PPSA01234"]);
    expect(read).toHaveBeenCalledTimes(1);
  });
});
