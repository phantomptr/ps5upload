// A Collection copy (game folder, game image or archive) → an Upload queue item that sends it
// to the PS5. Packages install instead (see screens/Collection/collectionInstall.ts).

import type { CollectionGame, CollectionLocation } from "../api/collection";
import type { AddQueueItem } from "../state/uploadQueue";
import { consoleAddr, hostOf } from "./addr";
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
