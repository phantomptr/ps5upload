import { describe, expect, it } from "vitest";

import {
  findConsoleInstall,
  findDisplacedInstall,
  installRefusal,
  registerRefusedText,
  titleIdFromParamJson,
  type InstallProbe,
} from "./consoleInstall";
import { humanizePs5Error } from "./humanizeError";

/** A console with exactly these paths (folders are listed too). */
function consoleWith(paths: string[], fails = false): InstallProbe & { asked: string[] } {
  const set = new Set(paths);
  const asked: string[] = [];
  return {
    asked,
    exists: async (p: string) => {
      asked.push(p);
      if (fails) throw new Error("timed out");
      return set.has(p);
    },
  };
}

// #884: Among Us (PPSA03595) installed on the M.2 SSD; Play on a dump of it rewrote the PS5's
// record of the game.
const M2_INSTALL = [
  "/mnt/ext1/user/app/PPSA03595/app.pkg",
  "/mnt/ext1/user/app/PPSA03595/app.json",
];

describe("findConsoleInstall", () => {
  it("finds the reported M.2 install", async () => {
    await expect(findConsoleInstall(consoleWith(M2_INSTALL), "PPSA03595")).resolves.toEqual({
      where: "m2",
      pkg: "/mnt/ext1/user/app/PPSA03595/app.pkg",
    });
  });

  it("finds internal and USB extended storage installs", async () => {
    await expect(
      findConsoleInstall(consoleWith(["/user/app/CUSA00900/app.pkg"]), "cusa00900"),
    ).resolves.toMatchObject({ where: "internal" });
    await expect(
      findConsoleInstall(consoleWith(["/mnt/ext0/user/app/CUSA07842/app.pkg"]), "CUSA07842"),
    ).resolves.toMatchObject({ where: "usb" });
  });

  it("is null for a folder registration of ours and for another title", async () => {
    const ours = consoleWith([
      "/user/app/PPSA17221/sce_sys",
      "/user/app/PPSA17221/mount.lnk",
      "/mnt/ext1/user/app/PPSA99999/app.pkg",
    ]);
    await expect(findConsoleInstall(ours, "PPSA17221")).resolves.toBeNull();
  });

  it("never builds a path from something that isn't a title id", async () => {
    const c = consoleWith([]);
    await expect(findConsoleInstall(c, "../../etc")).resolves.toBeNull();
    expect(c.asked).toEqual([]);
  });

  it("rejects when the console can't say: 'can't tell' is not 'not installed'", async () => {
    await expect(findConsoleInstall(consoleWith([], true), "PPSA03595")).rejects.toThrow("timed out");
  });
});

describe("installRefusal", () => {
  it("reads the helper's and the app's refusals", () => {
    expect(
      installRefusal("payload rejected APP_REGISTER(/mnt/usb0/d/PPSA03595): register_title_installed:m2"),
    ).toEqual({ op: "register", where: "m2" });
    expect(installRefusal("payload rejected APP_UNREGISTER: unregister_title_installed:usb")).toEqual({
      op: "unregister",
      where: "usb",
    });
    expect(installRefusal("register_title_installed:internal (/user/app/X/app.pkg)")).toEqual({
      op: "register",
      where: "internal",
    });
    expect(installRefusal("register_src_missing_eboot")).toBeNull();
  });

  it("is worded for people, naming where the game is", () => {
    const text = humanizePs5Error("payload rejected APP_REGISTER(/mnt/usb0/d): register_title_installed:m2");
    expect(text).toContain("M.2 storage");
    expect(text).toContain("home screen");
    expect(text).not.toContain("register_title_installed");
    expect(registerRefusedText("m2", "Among Us")).toMatch(/^Among Us is already installed on M\.2 storage\./);
  });
});

describe("titleIdFromParamJson", () => {
  it("reads titleId", () => {
    expect(titleIdFromParamJson('{"titleId":"PPSA03595","contentId":"x"}')).toBe("PPSA03595");
    expect(titleIdFromParamJson('{"titleId":"nope"}')).toBeNull();
    expect(titleIdFromParamJson("not json")).toBeNull();
  });
});

describe("findDisplacedInstall", () => {
  it("recognizes the reported aftermath", async () => {
    const after = consoleWith([
      ...M2_INSTALL,
      "/user/app/PPSA03595/sce_sys",
      "/user/app/PPSA03595/icon0.png",
    ]);
    await expect(findDisplacedInstall(after, "PPSA03595")).resolves.toEqual({
      titleId: "PPSA03595",
      where: "m2",
      pkg: "/mnt/ext1/user/app/PPSA03595/app.pkg",
      stagedFiles: ["/user/app/PPSA03595/sce_sys", "/user/app/PPSA03595/icon0.png"],
    });
  });

  it("says nothing about a healthy install", async () => {
    await expect(findDisplacedInstall(consoleWith(M2_INSTALL), "PPSA03595")).resolves.toBeNull();
    await expect(
      findDisplacedInstall(
        consoleWith(["/user/app/PPSA03595/app.pkg", "/user/app/PPSA03595/sce_sys", "/user/app/PPSA03595/icon0.png"]),
        "PPSA03595",
      ),
    ).resolves.toBeNull();
    // sce_sys alone next to an M.2 install is not PS5Upload's signature.
    await expect(
      findDisplacedInstall(consoleWith([...M2_INSTALL, "/user/app/PPSA03595/sce_sys"]), "PPSA03595"),
    ).resolves.toBeNull();
    // A folder game of ours with no package anywhere.
    await expect(
      findDisplacedInstall(
        consoleWith(["/user/app/PPSA17221/sce_sys", "/user/app/PPSA17221/mount.lnk"]),
        "PPSA17221",
      ),
    ).resolves.toBeNull();
  });
});
