import { describe, expect, it } from "vitest";
import { isTarArchiveName, stripTarSuffix, tarSuffix } from "./tarNames";
import { archiveExtension, archiveFormat } from "../state/upload";
import { classifyScanEntry } from "./uploadBatch";
import { resolveUploadDest } from "./uploadDest";
import { archiveStem, isArchiveSource } from "../state/fpkgConversion";

describe("tar and .zst names", () => {
  it("knows every tar ending and a lone .zst, whole", () => {
    for (const [name, suffix] of [
      ["G.tar", ".tar"],
      ["G.TAR.ZST", ".tar.zst"],
      ["G.tzst", ".tzst"],
      ["G.tar.gz", ".tar.gz"],
      ["G.tgz", ".tgz"],
      ["G.tar.xz", ".tar.xz"],
      ["G.txz", ".txz"],
      ["G.tar.bz2", ".tar.bz2"],
      ["G.tbz2", ".tbz2"],
      ["G.tbz", ".tbz"],
      ["disc.exfat.zst", ".zst"],
    ]) {
      expect(tarSuffix(name), name).toBe(suffix);
      expect(isTarArchiveName(name), name).toBe(true);
      expect(archiveFormat(`/x/${name}`), name).toBe("tar");
      expect(archiveExtension(`/x/${name}`), name).toBe(suffix);
    }
    for (const name of ["G.gz", "G.xz", "G.bz2", "G.zip", "G.tar.7z", "notes.txt"]) {
      expect(isTarArchiveName(name), name).toBe(false);
    }
    expect(archiveExtension("/x/G.zip")).toBe(".zip");
    expect(archiveExtension("/x/G.pkg")).toBeNull();
    expect(stripTarSuffix("Game.tar.zst")).toBe("Game");
    expect(stripTarSuffix("disc.exfat.zst")).toBe("disc.exfat");
    expect(stripTarSuffix("Game.zip")).toBe("Game.zip");
  });

  it("is offered wherever a zip is: a folder scan, the upload folder name, Convert", () => {
    expect(classifyScanEntry("Game.tar.zst", false)).toBe("archive");
    expect(classifyScanEntry("Game.tgz", false)).toBe("archive");
    expect(resolveUploadDest("/data", "homebrew", "/d/MyGame.tar.zst", true, true).dest).toBe(
      "/data/homebrew/MyGame",
    );
    expect(resolveUploadDest("/data", "homebrew", "/d/MyGame.tgz", true, true).dest).toBe(
      "/data/homebrew/MyGame",
    );
    expect(isArchiveSource("/d/Game.tar.xz")).toBe(true);
    expect(archiveStem("/d/Game.tar.xz")).toBe("Game");
  });
});
