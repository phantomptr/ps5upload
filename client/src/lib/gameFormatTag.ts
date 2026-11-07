// The small format tag the Files list shows beside a game image, package or game folder. Read
// from the name alone, the same extensions the rest of the app knows (lib/archiveDefault
// PACKAGE_EXTS, lib/spacePlan GAME_IMAGES): nothing is opened to decide.

export type GameFormatTag = "exfat" | "ffpfsc" | "ffpfs" | "ufs2" | "pkg" | "folder";

/** A folder named after a title id (`PPSA01342`, `CUSA12345-app`, `Game [PPSA01342]`): how dumps
 *  and uploads are named, so the Files list can tell a game folder from any other. */
const TITLE_FOLDER = /(?:CUSA|PPSA|PCSA)\d{5}/;

export function gameFormatTag(name: string, isDir: boolean): GameFormatTag | null {
  if (isDir) return TITLE_FOLDER.test(name) ? "folder" : null;
  const m = /\.(pkg|exfat|ffpfsc|ffpkg|ffpfs)$/i.exec(name);
  switch (m?.[1].toLowerCase()) {
    case "pkg":
      return "pkg";
    case "exfat":
      return "exfat";
    case "ffpfsc":
      return "ffpfsc";
    case "ffpfs":
      return "ffpfs";
    // A .ffpkg is a UFS2 image (ShadowMount+ mounts it as UFS).
    case "ffpkg":
      return "ufs2";
    default:
      return null;
  }
}

/** What the tag says. Format names are not translated; "folder" is (the caller passes it). */
export function gameFormatLabel(tag: GameFormatTag, folderWord: string): string {
  switch (tag) {
    case "exfat":
      return "exFAT";
    case "ffpfsc":
      return "FFPFSC";
    case "ffpfs":
      return "FFPFS";
    case "ufs2":
      return "UFS2";
    case "pkg":
      return "PKG";
    case "folder":
      return folderWord;
  }
}
