// A Collection copy (game folder, game image or archive) → an Upload queue item that sends it
// to the PS5. Packages install (see screens/GameHub/collectionInstall.ts), or go as a plain file
// to a folder Kraken Mount mounts them from (packageSendItems).

import type { CollectionGame, CollectionLocation } from "../api/collection";
import type { AddQueueItem } from "../state/uploadQueue";
import { consoleAddr, hostOf } from "./addr";
import { krakenRefusal } from "./mounter";
import { resolveUploadDest } from "./uploadDest";

/** What a copy is to Upload, or null for a package (installed, not uploaded). */
export type SendKind = "folder" | "image" | "archive";

export function sendKind(loc: Pick<CollectionLocation, "type">): SendKind | null {
  if (loc.type === "folder") return "folder";
  if (loc.type.startsWith("mount.")) return "image";
  if (loc.type === "zip" || loc.type === "7z" || loc.type === "rar") return "archive";
  return null;
}

/** The folder ShadowMount+ scans; where game images (and game folders) go by default. */
export const DEFAULT_SEND_SUBPATH = "homebrew";

export interface SendPlan {
  host: string;
  /** Drive on the PS5; `/data` when not chosen. */
  volume: string | null;
  /** Folder on that drive. */
  subpath: string;
  /** Game folders only: register the game on the PS5 once it is there. */
  register: boolean;
}

/** Where the copy will land on the PS5. */
export function sendDestination(loc: CollectionLocation, plan: SendPlan): string {
  return resolveUploadDest(
    plan.volume,
    plan.subpath,
    loc.absolute_path,
    sendKind(loc) === "archive",
    true,
  ).dest;
}

/** The queue item that sends `loc` as it is. */
export function collectionSendItem(
  game: Pick<CollectionGame, "title">,
  loc: CollectionLocation,
  plan: SendPlan,
): AddQueueItem {
  const kind = sendKind(loc);
  if (!kind) throw new Error("a package is installed, not uploaded");
  return {
    sourceKind: kind === "folder" ? "game-folder" : kind,
    sourcePath: loc.absolute_path,
    displayName: game.title,
    resolvedDest: sendDestination(loc, plan),
    addr: consoleAddr(hostOf(plan.host)),
    strategy: "overwrite",
    reconcileMode: "fast",
    excludes: [],
    rarPassword: null,
    // ShadowMount+ mounts images by itself; the app's own mount would hold the file open.
    mountAfterUpload: false,
    mountReadOnly: true,
    registerAfterUpload: kind === "folder" && plan.register,
    estimatedBytes: loc.size_bytes,
  };
}

/** The copy to send by default: a game folder or image over an archive, then the largest. */
export function bestSendable(locs: CollectionLocation[]): CollectionLocation | null {
  const rank = (l: CollectionLocation) => (sendKind(l) === "archive" ? 1 : 0);
  return (
    locs
      .filter((l) => sendKind(l) && l.pkg?.complete !== false && !l.pkg?.error)
      .sort((a, b) => rank(a) - rank(b) || b.size_bytes - a.size_bytes)[0] ?? null
  );
}

/** A whole, readable package copy: it can be installed, or sent to the PS5 as a plain file. */
export function packageUsable(loc: Pick<CollectionLocation, "type" | "pkg">): boolean {
  return loc.type === "pkg" && loc.pkg?.complete !== false && !loc.pkg?.error;
}

/** Kraken Mount would mount this package from its scan folder, as far as the collection knows: a
 *  PS5 game (not an update or DLC), in one file. Unknown platform or kind count as a PS5 game. */
export function krakenMountsCopy(loc: Pick<CollectionLocation, "type" | "pkg" | "parts">): boolean {
  if (!packageUsable(loc)) return false;
  const p = loc.pkg;
  if ((p?.kind ?? "base") !== "base") return false;
  return krakenRefusal({ platform: p?.platform ?? null, parts: 1 + (loc.parts?.length ?? 0) }) === null;
}

/** What a copy's row offers: its one button, and what the overflow menu adds. A package is
 *  installed, unless Kraken Mount runs on the console and would mount it from a folder: then it is
 *  sent there as a file, and Install moves to the menu. Either way the other one is in the menu. */
export function copyActions(
  loc: Pick<CollectionLocation, "type" | "pkg" | "parts">,
  opts: { host: boolean; kraken: boolean },
): { primary: "send" | "install" | null; menuSendFile: boolean; menuInstall: boolean } {
  const none = { primary: null, menuSendFile: false, menuInstall: false };
  if (!opts.host) return none;
  if (sendKind(loc) && loc.pkg?.complete !== false && !loc.pkg?.error)
    return { primary: "send", menuSendFile: false, menuInstall: false };
  if (!packageUsable(loc)) return none;
  if (opts.kraken && krakenMountsCopy(loc))
    return { primary: "send", menuSendFile: false, menuInstall: true };
  return { primary: "install", menuSendFile: true, menuInstall: false };
}

/** The upload queue items that send a package copy to the PS5 as plain files, straight into the
 *  plan's folder: no install, no package staging, nothing deleted after (as Upload's "Copy as a
 *  file"). A split package goes as all its parts, side by side. */
export function packageSendItems(
  game: Pick<CollectionGame, "title">,
  loc: CollectionLocation,
  plan: Pick<SendPlan, "host" | "volume" | "subpath">,
): AddQueueItem[] {
  if (!packageUsable(loc)) throw new Error("only a whole package is sent as a file");
  const files = [loc.absolute_path, ...(loc.parts ?? [])];
  return files.map((src, i) => ({
    sourceKind: "file",
    sourcePath: src,
    displayName: files.length > 1 ? `${game.title} (${i + 1}/${files.length})` : game.title,
    resolvedDest: resolveUploadDest(plan.volume, plan.subpath, src, false).dest,
    addr: consoleAddr(hostOf(plan.host)),
    strategy: "overwrite",
    reconcileMode: "fast",
    excludes: [],
    rarPassword: null,
    mountAfterUpload: false,
    mountReadOnly: true,
    registerAfterUpload: false,
    // Each part's own size is not known here: the whole copy's only for a single file.
    estimatedBytes: files.length > 1 ? undefined : loc.size_bytes,
  }));
}
