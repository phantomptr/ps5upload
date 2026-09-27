import { beforeEach, describe, expect, it } from "vitest";

import type { Volume } from "../api/ps5";
import {
  INTERNAL_PKG_DIR,
  libraryDirs,
  pkgDirOnVolume,
  resolvePkgStorage,
  usePkgStorageStore,
} from "./pkgStorage";

const vol = (path: string, over: Partial<Volume> = {}): Volume => ({
  path,
  fs_type: "exfatfs",
  total_bytes: 500e9,
  free_bytes: 400e9,
  writable: true,
  ...over,
});
const internal = vol("/data", { fs_type: "nullfs" });
const usb = vol("/mnt/usb0");
const m2 = vol("/mnt/ext1", { fs_type: "ufs" });

describe("pkgDirOnVolume", () => {
  it("keeps internal storage where it always was", () => {
    expect(pkgDirOnVolume(null)).toBe("/user/data/ps5upload/pkg_library");
    expect(pkgDirOnVolume(null)).toBe(INTERNAL_PKG_DIR);
  });

  it("puts packages in a ps5upload folder on another drive", () => {
    expect(pkgDirOnVolume("/mnt/usb0")).toBe("/mnt/usb0/ps5upload/pkg_library");
    expect(pkgDirOnVolume("/mnt/ext1/")).toBe("/mnt/ext1/ps5upload/pkg_library");
  });

  it("treats the internal mount points as internal", () => {
    expect(pkgDirOnVolume("/data")).toBe(INTERNAL_PKG_DIR);
    expect(pkgDirOnVolume("/user")).toBe(INTERNAL_PKG_DIR);
  });
});

describe("resolvePkgStorage", () => {
  it("uses internal storage when nothing is chosen", () => {
    expect(resolvePkgStorage(null, [internal, usb])).toEqual({
      dir: INTERNAL_PKG_DIR,
      volume: null,
      fellBack: false,
    });
  });

  it("uses the chosen drive when it is there and writable", () => {
    expect(resolvePkgStorage("/mnt/usb0", [internal, usb])).toEqual({
      dir: "/mnt/usb0/ps5upload/pkg_library",
      volume: "/mnt/usb0",
      fellBack: false,
    });
  });

  it("falls back to internal when the chosen drive is unplugged", () => {
    expect(resolvePkgStorage("/mnt/usb0", [internal, m2])).toEqual({
      dir: INTERNAL_PKG_DIR,
      volume: null,
      fellBack: true,
    });
  });

  it("falls back to internal when the chosen drive is read-only or a placeholder", () => {
    expect(resolvePkgStorage("/mnt/usb0", [internal, vol("/mnt/usb0", { writable: false })]).fellBack).toBe(true);
    expect(resolvePkgStorage("/mnt/usb0", [internal, vol("/mnt/usb0", { is_placeholder: true })]).fellBack).toBe(true);
  });

  it("falls back to internal when the drives could not be read", () => {
    expect(resolvePkgStorage("/mnt/usb0", null)).toEqual({
      dir: INTERNAL_PKG_DIR,
      volume: null,
      fellBack: true,
    });
  });
});

describe("libraryDirs", () => {
  it("scans internal plus every writable storage drive, so switching hides nothing", () => {
    const image = vol("/mnt/ps5upload/Game", { source_image: "/data/x.exfat" });
    const ro = vol("/mnt/usb1", { writable: false });
    expect(libraryDirs([internal, vol("/user"), usb, m2, image, ro])).toEqual([
      INTERNAL_PKG_DIR,
      "/mnt/usb0/ps5upload/pkg_library",
      "/mnt/ext1/ps5upload/pkg_library",
    ]);
  });

  it("is just internal when the drives are unknown", () => {
    expect(libraryDirs(null)).toEqual([INTERNAL_PKG_DIR]);
  });
});

describe("usePkgStorageStore", () => {
  beforeEach(() => usePkgStorageStore.setState({ defaults: {} }));

  it("remembers a default drive per console", () => {
    const s = usePkgStorageStore.getState();
    s.setDefault("192.168.86.99:9113", "/mnt/usb0");
    s.setDefault("192.168.86.100", "/mnt/ext1");
    expect(usePkgStorageStore.getState().defaultFor("192.168.86.99")).toBe("/mnt/usb0");
    expect(usePkgStorageStore.getState().defaultFor("192.168.86.100:9114")).toBe("/mnt/ext1");
    expect(usePkgStorageStore.getState().defaultFor("10.0.0.1")).toBeNull();
  });

  it("choosing internal clears the choice", () => {
    const s = usePkgStorageStore.getState();
    s.setDefault("192.168.86.99", "/mnt/usb0");
    s.setDefault("192.168.86.99", null);
    expect(usePkgStorageStore.getState().defaultFor("192.168.86.99")).toBeNull();
    s.setDefault("192.168.86.99", "/data");
    expect(usePkgStorageStore.getState().defaultFor("192.168.86.99")).toBeNull();
  });
});

describe("pkgMkdirChain", () => {
  it("creates the ps5upload folder and every level down to a package's folder on a drive", async () => {
    const { pkgMkdirChain } = await import("./pkgStorage");
    expect(pkgMkdirChain("/mnt/usb0/ps5upload/pkg_library/updates/abc")).toEqual([
      "/mnt/usb0/ps5upload",
      "/mnt/usb0/ps5upload/pkg_library",
      "/mnt/usb0/ps5upload/pkg_library/updates",
      "/mnt/usb0/ps5upload/pkg_library/updates/abc",
    ]);
  });

  it("does the same under internal storage", async () => {
    const { pkgMkdirChain } = await import("./pkgStorage");
    expect(pkgMkdirChain(INTERNAL_PKG_DIR)).toEqual([
      "/user/data/ps5upload",
      "/user/data/ps5upload/pkg_library",
    ]);
  });

  it("leaves a path outside any package library alone", async () => {
    const { pkgMkdirChain } = await import("./pkgStorage");
    expect(pkgMkdirChain("/data/games")).toEqual(["/data/games"]);
  });
});

describe("volumeOfPkgPath", () => {
  it("names the drive a package lives on, and nothing for internal storage", async () => {
    const { volumeOfPkgPath } = await import("./pkgStorage");
    expect(volumeOfPkgPath("/mnt/usb0/ps5upload/pkg_library/updates/a/X.pkg")).toBe("/mnt/usb0");
    expect(volumeOfPkgPath(`${INTERNAL_PKG_DIR}/X.pkg`)).toBeNull();
    expect(volumeOfPkgPath("/data/games/X.pkg")).toBeNull();
  });
});
