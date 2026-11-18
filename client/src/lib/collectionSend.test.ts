import { describe, expect, it } from "vitest";
import type { CollectionLocation } from "../api/collection";
import type { KrakenStatus } from "../api/ps5";
import {
  bestSendable,
  collectionSendItem,
  copyActions,
  krakenMountsCopy,
  packageSendItems,
  sendKind,
} from "./collectionSend";
import { krakenCopyDestination } from "./pkgUploadChoice";

const loc = (type: string, path: string, size: number, extra: Partial<CollectionLocation> = {}) =>
  ({
    root: "/g",
    container: "",
    name: path.split("/").pop()!,
    type,
    path,
    absolute_path: path,
    size_bytes: size,
    added_ts: 0,
    ...extra,
  }) as CollectionLocation;

const plan = { host: "10.0.0.5", volume: "/mnt/ext0", subpath: "homebrew", register: true };

describe("sendKind", () => {
  it("sends folders, images and archives, never packages", () => {
    expect(sendKind({ type: "folder" })).toBe("folder");
    expect(sendKind({ type: "mount.ffpfsc" })).toBe("image");
    expect(sendKind({ type: "rar" })).toBe("archive");
    expect(sendKind({ type: "pkg" })).toBeNull();
  });
});

describe("collectionSendItem", () => {
  it("sends a game folder to the chosen drive's homebrew and registers it", () => {
    const it = collectionSendItem({ title: "007" }, loc("folder", "/g/PPSA11386-app", 9), plan);
    expect(it.sourceKind).toBe("game-folder");
    expect(it.resolvedDest).toBe("/mnt/ext0/homebrew/PPSA11386-app");
    expect(it.registerAfterUpload).toBe(true);
    expect(it.addr).toBe("10.0.0.5");
    expect(it.displayName).toBe("007");
  });

  it("lets ShadowMount+ mount an image instead of the app", () => {
    const it = collectionSendItem({ title: "X" }, loc("mount.exfat", "/g/X.exfat", 9), plan);
    expect(it.sourceKind).toBe("image");
    expect(it.mountAfterUpload).toBe(false);
    expect(it.resolvedDest).toBe("/mnt/ext0/homebrew/X.exfat");
  });

  it("unpacks an archive into a folder named after it", () => {
    const it = collectionSendItem({ title: "Y" }, loc("7z", "/g/Y.7z", 9), plan);
    expect(it.sourceKind).toBe("archive");
    expect(it.resolvedDest).toBe("/mnt/ext0/homebrew/Y");
    expect(it.registerAfterUpload).toBe(false);
  });

  it("refuses a package", () => {
    expect(() => collectionSendItem({ title: "Z" }, loc("pkg", "/g/z.pkg", 1), plan)).toThrow();
  });
});

describe("bestSendable", () => {
  it("prefers a folder or image over an archive, then the largest, and skips broken copies", () => {
    const a = loc("zip", "/g/a.zip", 100);
    const b = loc("folder", "/g/b", 10);
    const c = loc("mount.exfat", "/g/c.exfat", 50);
    const broken = loc("folder", "/g/d", 999, { pkg: { error: "unreadable" } as never });
    expect(bestSendable([a, b, c, broken])?.absolute_path).toBe("/g/c.exfat");
    expect(bestSendable([a])?.absolute_path).toBe("/g/a.zip");
    expect(bestSendable([loc("pkg", "/g/p.pkg", 1)])).toBeNull();
  });
});

const pkg = (kind: string, extra: Partial<CollectionLocation> = {}, platform = "PS5") =>
  loc("pkg", `/g/Love is all around 2 - PPSA31909 - ${kind}.pkg`, 40, {
    pkg: { kind, platform, version: "1.03", content_id: "EP0000-PPSA31909_00-LOVE000000000000" },
    ...extra,
  });

describe("copyActions", () => {
  const on = { host: true, kraken: true };
  const off = { host: true, kraken: false };

  it("installs a package when Kraken Mount does not run, with Send as a file in the menu", () => {
    expect(copyActions(pkg("base"), off)).toEqual({ primary: "install", menuSendFile: true, menuInstall: false });
  });

  it("sends a game's package when Kraken Mount runs, with Install in the menu", () => {
    expect(copyActions(pkg("base"), on)).toEqual({ primary: "send", menuSendFile: false, menuInstall: true });
  });

  it("does the same for a package on a saved server", () => {
    const remote = pkg("base", { absolute_path: "remote://shared-nas/games/Love.pkg" });
    expect(copyActions(remote, on).primary).toBe("send");
    expect(copyActions(remote, off)).toEqual({ primary: "install", menuSendFile: true, menuInstall: false });
  });

  it("keeps Install first for what Kraken Mount does not mount (update, DLC, PS4, split)", () => {
    for (const l of [
      pkg("patch"),
      pkg("dlc"),
      pkg("base", {}, "PS4"),
      pkg("base", { parts: ["/g/Love_1.pkg"] }),
    ]) {
      expect(krakenMountsCopy(l)).toBe(false);
      expect(copyActions(l, on)).toEqual({ primary: "install", menuSendFile: true, menuInstall: false });
    }
  });

  it("leaves folders, images and archives as they were", () => {
    const want = { primary: "send", menuSendFile: false, menuInstall: false };
    expect(copyActions(loc("folder", "/g/X", 1), on)).toEqual(want);
    expect(copyActions(loc("folder", "/g/X", 1), off)).toEqual(want);
    expect(copyActions(loc("mount.ffpfsc", "/g/X.ffpfsc", 1), on)).toEqual(want);
  });

  it("offers nothing without a console, or for a broken package", () => {
    const none = { primary: null, menuSendFile: false, menuInstall: false };
    expect(copyActions(pkg("base"), { host: false, kraken: true })).toEqual(none);
    expect(copyActions(pkg("base", { pkg: { complete: false } }), on)).toEqual(none);
    expect(copyActions(pkg("base", { pkg: { error: "bad header" } }), off)).toEqual(none);
  });
});

describe("packageSendItems", () => {
  const kraken = {
    settings: { inject_ampr: false, ampr_auto_update: false, scan_paths: [] },
    scan_roots: ["/data/homebrew", "/mnt/ext0/homebrew"],
  } as unknown as KrakenStatus;

  it("queues a plain file into Kraken Mount's folder: no install, no staging, nothing deleted", () => {
    const dest = krakenCopyDestination("/mnt/ext0", kraken, ["/data", "/mnt/ext0"]);
    expect(dest).toEqual({ volume: "/mnt/ext0", subpath: "homebrew" });
    const l = pkg("base");
    const items = packageSendItems({ title: "Love is all around 2" }, l, { host: "10.0.0.5", ...dest! });
    expect(items).toHaveLength(1);
    const [it] = items;
    expect(it.sourceKind).toBe("file");
    expect(it.sourcePath).toBe(l.absolute_path);
    expect(it.resolvedDest).toBe("/mnt/ext0/homebrew/Love is all around 2 - PPSA31909 - base.pkg");
    expect(it.addr).toBe("10.0.0.5");
    expect(it.installAfterUpload).toBeUndefined();
    expect(it.install).toBeUndefined();
    expect(it.deleteSourceAfterUpload).toBeUndefined();
    expect(it.deletePkgAfterInstall).toBeUndefined();
    expect(it.registerAfterUpload).toBe(false);
    expect(it.mountAfterUpload).toBe(false);
    expect(it.estimatedBytes).toBe(40);
  });

  it("sends a package on a saved server from there", () => {
    const l = pkg("patch", { absolute_path: "remote://shared-nas/games/Love v1.03.pkg" });
    const [it] = packageSendItems({ title: "Love" }, l, { host: "10.0.0.5", volume: null, subpath: "homebrew" });
    expect(it.sourcePath).toBe("remote://shared-nas/games/Love v1.03.pkg");
    expect(it.resolvedDest).toBe("/data/homebrew/Love v1.03.pkg");
  });

  it("sends every part of a split package side by side", () => {
    const l = loc("pkg", "/g/Big_0.pkg", 90, { parts: ["/g/Big_1.pkg", "/g/Big_2.pkg"], pkg: { kind: "base" } });
    const items = packageSendItems({ title: "Big" }, l, plan);
    expect(items.map((i) => i.resolvedDest)).toEqual([
      "/mnt/ext0/homebrew/Big_0.pkg",
      "/mnt/ext0/homebrew/Big_1.pkg",
      "/mnt/ext0/homebrew/Big_2.pkg",
    ]);
    expect(items.every((i) => i.sourceKind === "file")).toBe(true);
  });

  it("refuses a package that is not whole", () => {
    expect(() =>
      packageSendItems({ title: "X" }, pkg("base", { pkg: { complete: false } }), plan),
    ).toThrow();
  });
});
