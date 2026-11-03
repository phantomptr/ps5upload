// What to do with a .zip/.7z/.rar the user picked (#645): unpack it onto the PS5, or send the
// archive as it is. A game or a package archive is unpacked, as it always was; anything else
// (payloads, a jailbreak update, saves) is sent as it is, because whatever reads it on the PS5
// wants the archive file. When the listing is unknown nothing is chosen and the user decides.

import type { ZipInspect } from "../api/ps5";

/** "unpack" = extract onto the PS5; "as-is" = upload the archive file itself. */
export type ArchiveChoice = "unpack" | "as-is";

/** Packages and disk images: an archive holding one is unpacked. */
const PACKAGE_EXTS = [".pkg", ".ffpkg", ".exfat", ".ffpfs", ".ffpfsc"];
/** Files that mark a game, at the top of the archive or one folder down. */
const GAME_FILES = ["eboot.bin", "param.json", "param.sfo"];

/** True when this path (inside the archive) shows a game or a package. */
function marksGameOrPackage(path: string): boolean {
  const norm = path.replace(/\\/g, "/").replace(/^(\.\/|\/)+/, "").toLowerCase();
  const isDir = norm.endsWith("/");
  const parts = norm.split("/").filter(Boolean);
  if (parts.length === 0) return false;
  const last = parts[parts.length - 1];
  if (!isDir && PACKAGE_EXTS.some((ext) => last.endsWith(ext))) return true;
  // `sce_sys` as the first or second folder: `sce_sys/…` or `Game/sce_sys/…`.
  const sceSys = parts.indexOf("sce_sys");
  if (sceSys === 0 || sceSys === 1) {
    // A file named sce_sys is not the folder.
    if (isDir || parts.length > sceSys + 1) return true;
  }
  if (!isDir && parts.length <= 2 && GAME_FILES.includes(last)) return true;
  return false;
}

/** The default for an archive, from what its listing holds. Null = unknown: ask the user.
 *  `info` is the archive's inspection (null when it was not inspected or it failed). */
export function archiveDefault(
  info: Pick<ZipInspect, "game_root" | "entry_names" | "entry_names_complete"> | null | undefined,
): ArchiveChoice | null {
  if (!info) return null;
  // The engine found a game's sce_sys/param.json.
  if (info.game_root != null) return "unpack";
  const names = info.entry_names;
  if (!names) return null;
  if (names.some(marksGameOrPackage)) return "unpack";
  // A cut-off listing without a game or package in it may still hold one further on.
  return info.entry_names_complete ? "as-is" : null;
}
