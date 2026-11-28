import { describe, expect, it } from "vitest";

import type { InstalledTitle } from "../../api/ps5";
import { asksAboutSource, sourceDetail, uninstallChoices } from "./uninstallChoice";

// The fallback with its {placeholders} filled, as the real tr does for English.
const tr = ((_k: string, vars: Record<string, string | number> | undefined, fb: string) =>
  fb.replace(/\{(\w+)\}/g, (_m, n: string) => String(vars?.[n] ?? `{${n}}`))) as never;

const title = (over: Partial<InstalledTitle>): InstalledTitle => ({
  titleId: "PPSA00001",
  titleName: "Game",
  origin: "registered",
  imageBacked: false,
  source: "/data/homebrew/Game",
  system: false,
  ...over,
});

const none = { kraken: false, smp: false };

describe("which Uninstall asks about the game's files", () => {
  it("a folder or disk-image game asks", () => {
    expect(asksAboutSource(title({}), none)).toBe(true);
    expect(asksAboutSource(title({ imageBacked: true, source: "/mnt/ps5upload/G/x" }), none)).toBe(true);
  });

  it("an installed package, a system title, and other mounters' titles just uninstall", () => {
    expect(asksAboutSource(title({ origin: "pkg", source: "" }), none)).toBe(false);
    expect(asksAboutSource(title({ source: "" }), none)).toBe(false);
    expect(asksAboutSource(title({ system: true }), none)).toBe(false);
    expect(asksAboutSource(title({}), { kraken: true, smp: false })).toBe(false);
    expect(asksAboutSource(title({}), { kraken: false, smp: true })).toBe(false);
  });
});

describe("the Uninstall choices", () => {
  it("a folder: Uninstall only first, then delete the folder with its path and size", () => {
    const c = uninstallChoices(
      { offer: true, kind: "folder", path: "/data/homebrew/Game", bytes: 3 * 1024 ** 3 },
      tr,
    );
    expect(c?.map((x) => x.value)).toEqual(["keep", "delete"]);
    expect(c?.[0].label).toBe("Uninstall only");
    expect(c?.[1].label).toBe("Uninstall and delete the game folder");
    expect(c?.[1].detail).toBe("/data/homebrew/Game (3.00 GiB)");
  });

  it("a disk image: offers to delete the image", () => {
    const c = uninstallChoices(
      { offer: true, kind: "image", path: "/mnt/ext1/Game.ffpkg", mount_point: "/mnt/ps5upload/Game", bytes: 1024 },
      tr,
    );
    expect(c?.[1].label).toBe("Uninstall and delete the disk image");
    expect(c?.[1].detail).toBe("/mnt/ext1/Game.ffpkg (1.00 KiB)");
  });

  it("a size the walk did not finish reads as at least", () => {
    expect(sourceDetail({ offer: true, kind: "folder", path: "/p", bytes: 2048, size_partial: true }, tr)).toBe(
      "/p (at least 2.00 KiB)",
    );
    expect(sourceDetail({ offer: true, kind: "folder", path: "/p" }, tr)).toBe("/p");
  });

  it("a source already gone or not safe to delete: no question, the plain uninstall", () => {
    expect(uninstallChoices({ offer: false, reason: "gone" }, tr)).toBeNull();
    expect(uninstallChoices({ offer: false, reason: "holds_other_game" }, tr)).toBeNull();
    expect(uninstallChoices({ offer: false, reason: "not_registered" }, tr)).toBeNull();
    expect(uninstallChoices({ offer: true, kind: "folder" }, tr)).toBeNull();
  });
});
