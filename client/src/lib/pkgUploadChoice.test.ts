import { beforeEach, describe, expect, it } from "vitest";

import type { KrakenStatus } from "../api/ps5";
import {
  krakenCopyDestination,
  loadPkgChoice,
  PKG_CHOICE_KEY,
  pkgChoiceOf,
  savePkgChoice,
} from "./pkgUploadChoice";
import { buildUploadQueueItem, type UploadItemOptions } from "./uploadQueueItem";

// vitest's node env has no `window`; safeStorage reads `window.localStorage`. An in-memory stub,
// set after the imports so the modules load as they do in node.
const stored = new globalThis.Map<string, string>();
const localStorage = {
  getItem: (k: string) => (stored.has(k) ? (stored.get(k) as string) : null),
  setItem: (k: string, v: string) => void stored.set(k, String(v)),
  removeItem: (k: string) => void stored.delete(k),
};
(globalThis as { window?: unknown }).window = { localStorage };

const kraken = (scan_paths: string[], scan_roots: string[]): KrakenStatus =>
  ({
    installed: true,
    running: true,
    api: true,
    version: "0.2.0",
    waiting: null,
    games: [],
    title_ids: [],
    ampr: null,
    settings: { inject_ampr: false, ampr_auto_update: false, scan_paths },
    scan_roots,
    errors: [],
  }) as KrakenStatus;

const opts: UploadItemOptions = {
  addr: "192.168.0.5:9113",
  destinationVolume: "/mnt/ext1",
  destinationSubpath: "homebrew",
  archiveIntoSubfolder: true,
  reconcileMode: "fast",
  strategy: "overwrite",
  excludes: [],
  mountAfterUpload: false,
  mountReadOnly: true,
  registerAfterUpload: true,
  installAfterUpload: true,
  deletePkgAfterInstall: true,
  pkgDir: "/data/pkg",
  nonce: "n",
  now: 0,
};

describe("the remembered .pkg choice", () => {
  beforeEach(() => localStorage.removeItem(PKG_CHOICE_KEY));

  it("is install until copy is chosen, and stays chosen", () => {
    expect(loadPkgChoice()).toBe("install");
    savePkgChoice("copy");
    expect(localStorage.getItem(PKG_CHOICE_KEY)).toBe("copy");
    expect(loadPkgChoice()).toBe("copy");
    savePkgChoice("install");
    expect(loadPkgChoice()).toBe("install");
  });

  it("reads anything else stored as install", () => {
    localStorage.setItem(PKG_CHOICE_KEY, "bogus");
    expect(loadPkgChoice()).toBe("install");
  });
});

describe("pkgChoiceOf", () => {
  it("tells install from copy by the source kind, only for packages", () => {
    expect(pkgChoiceOf({ kind: "pkg", path: "/d/Game.pkg" })).toBe("install");
    expect(pkgChoiceOf({ kind: "file", path: "/d/Game.PKG" })).toBe("copy");
    expect(pkgChoiceOf({ kind: "file", path: "/d/game.fpkg" })).toBe("copy");
    expect(pkgChoiceOf({ kind: "file", path: "/d/notes.txt" })).toBeNull();
    expect(pkgChoiceOf({ kind: "image", path: "/d/game.ffpkg" })).toBeNull();
    expect(pkgChoiceOf(null)).toBeNull();
  });
});

describe("a .pkg into the queue", () => {
  const base = { meta: null, wrappedHint: null, zipInfo: null };

  it("installs as a pkg source: staged in the package library", () => {
    const item = buildUploadQueueItem(
      { ...base, kind: "pkg", path: "/d/Game.pkg", pkgInfo: { contentId: "UP0000-PPSA01234_00-GAME000000000000", title: "Game", category: "gd", totalBytes: 10 } },
      null,
      opts,
    );
    expect(item.sourceKind).toBe("pkg");
    expect(item.resolvedDest.startsWith("/data/pkg/")).toBe(true);
    expect(item.installAfterUpload).toBe(true);
  });

  it("copied, is a plain file in the destination folder: no staging, no install, no delete", () => {
    const item = buildUploadQueueItem({ ...base, kind: "file", path: "/d/Game.pkg" }, null, opts);
    expect(item.sourceKind).toBe("file");
    expect(item.resolvedDest).toBe("/mnt/ext1/homebrew/Game.pkg");
    expect(item.installAfterUpload).toBeUndefined();
    expect(item.deletePkgAfterInstall).toBeUndefined();
  });
});

describe("krakenCopyDestination", () => {
  it("is nothing when Kraken Mount's roots are unknown", () => {
    expect(krakenCopyDestination("/data", null, ["/data"])).toBeNull();
  });

  it("is the drive's homebrew folder with Kraken Mount's default roots", () => {
    const k = kraken([], ["/data/homebrew", "/data/etaHEN/games"]);
    expect(krakenCopyDestination(null, k, ["/data"])).toEqual({ volume: "/data", subpath: "homebrew" });
    expect(krakenCopyDestination("/mnt/usb0", k, ["/data", "/mnt/usb0"])).toEqual({
      volume: "/mnt/usb0",
      subpath: "homebrew",
    });
  });

  it("takes the user's own scan folder on the chosen drive, else the first on a connected drive", () => {
    const k = kraken(["/mnt/ext1/games/pkgs", "/data/mine"], ["/mnt/ext1/games/pkgs", "/data/mine"]);
    expect(krakenCopyDestination("/data", k, ["/data", "/mnt/ext1"])).toEqual({ volume: "/data", subpath: "mine" });
    // No root on usb0: the first root on a connected drive.
    expect(krakenCopyDestination("/mnt/usb0", k, ["/data", "/mnt/ext1", "/mnt/usb0"])).toEqual({
      volume: "/mnt/ext1",
      subpath: "games/pkgs",
    });
    // None of its roots is on a connected drive.
    const away = kraken(["/mnt/ext1/games"], ["/mnt/ext1/games"]);
    expect(krakenCopyDestination("/data", away, ["/data"])).toBeNull();
  });
});
