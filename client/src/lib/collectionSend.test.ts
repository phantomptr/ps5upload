import { describe, expect, it } from "vitest";
import type { CollectionLocation } from "../api/collection";
import { bestSendable, collectionSendItem, sendKind } from "./collectionSend";

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
