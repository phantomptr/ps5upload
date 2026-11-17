// A queue row that failed because something with its name is already where it goes on the PS5.
//
// Two engine reasons say so: `ava1_exists` (refused before any data moved) and
// `ava1_commit_exists` (every byte arrived, then the console would not put it in place). A plain
// Retry fails the same way, so the row offers Replace (delete what is there, then send again) and
// Upload somewhere else (a different folder) instead.
//
// After `ava1_commit_exists` a folder's finished tree is still in `<dest>.ava-part` (AVA1
// SPEC.md §15.5). Replace then deletes what is in the way and moves that tree into place, and the
// row runs again as a Resume, which finds every file already there and sends nothing: a big game
// is not sent twice. Anything else (a single file, an archive, no tree left) is sent again.

import type { QueueItem } from "../state/uploadQueue";
import { hostOf } from "./addr";
import { removeUploadPart } from "./uploadPart";

/** `before`: refused before anything was sent. `after`: sent, but not put in place. */
export type DestinationConflict = "before" | "after";

export function destinationConflictOf(
  reason: string | null | undefined,
): DestinationConflict | null {
  if (reason === "ava1_exists") return "before";
  if (reason === "ava1_commit_exists") return "after";
  return null;
}

type ConflictRow = Pick<QueueItem, "status" | "sourceKind" | "errorReason">;

/** Whether a failed row offers Replace and Upload somewhere else: an upload (not an install, not
 *  a copy from another console) that failed because its destination is taken. Pure. */
export function offersConflictActions(it: ConflictRow): boolean {
  if (it.status !== "failed") return false;
  if (it.sourceKind === "install" || it.sourceKind === "ps5") return false;
  return destinationConflictOf(it.errorReason) !== null;
}

/** Where the row goes in `folder`: the folder plus the name it had. Pure. */
export function destInFolder(oldDest: string, folder: string): string {
  const name = oldDest.replace(/\/+$/, "").split("/").pop() ?? "";
  const dir = folder.replace(/\/+$/, "") || "";
  return `${dir}/${name}`;
}

export interface ReplaceDeps {
  /** Throws unless `path` is a folder on the console. */
  listDir: (addr: string, path: string) => Promise<{ name: string }[]>;
  remove: (addr: string, path: string) => Promise<void>;
  move: (addr: string, from: string, to: string) => Promise<void>;
  forgetResumeId: (host: string, src: string, dest: string) => Promise<void>;
}

type ReplaceRow = Pick<
  QueueItem,
  "addr" | "sourceKind" | "sourcePath" | "resolvedDest" | "errorReason"
>;

/**
 * Clears the way for Replace: deletes what is at the row's destination, then either moves the
 * finished staged tree into place (`placed: true`; the caller re-runs the row as a Resume) or
 * removes any leftover part so the caller can send it all again. Throws when the destination
 * could not be deleted: nothing has changed for the row then.
 */
export async function clearForReplace(
  it: ReplaceRow,
  deps: ReplaceDeps,
): Promise<{ placed: boolean }> {
  const dest = it.resolvedDest.replace(/\/+$/, "");
  const slash = dest.lastIndexOf("/");
  if (!dest || slash < 0) throw new Error("no destination");
  const name = dest.slice(slash + 1);
  const parent = dest.slice(0, slash) || "/";
  const part = `${dest}.ava-part`;
  const names = new Set((await deps.listDir(it.addr, parent)).map((e) => e.name));

  const folder = it.sourceKind === "folder" || it.sourceKind === "game-folder";
  const tree =
    folder &&
    destinationConflictOf(it.errorReason) === "after" &&
    names.has(`${name}.ava-part`) &&
    (await deps.listDir(it.addr, part).then(
      () => true,
      () => false,
    ));

  if (names.has(name)) await deps.remove(it.addr, dest);

  if (tree) {
    try {
      await deps.move(it.addr, part, dest);
      return { placed: true };
    } catch {
      /* not moved: send it again below */
    }
  }
  await removeUploadPart(it.addr, dest, deps).catch(() => undefined);
  // A single file's job id is remembered by source and destination: that job ended refused, so
  // the next run must be a new one.
  if (!folder && it.sourceKind !== "archive") {
    await deps.forgetResumeId(hostOf(it.addr), it.sourcePath, it.resolvedDest).catch(() => undefined);
  }
  return { placed: false };
}
