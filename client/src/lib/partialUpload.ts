// A single-file upload writes `<destination>.ava-part` on the console and renames it when it
// is complete. When the drive fills up the partial file is kept so Retry resumes it, but it
// holds its whole size: a user who gives up (or wants to free space another way) has to know
// it is there to delete it.

import type { QueueItem } from "../state/uploadQueue";

const NO_SPACE = new Set(["ava1_no_space", "preflight_insufficient_space"]);

/** The partial file a failed single-file upload left on the console, or null. */
export function partialUploadPath(
  it: Pick<QueueItem, "status" | "sourceKind" | "resolvedDest" | "errorReason">,
): string | null {
  if (it.status !== "failed" || !it.errorReason || !NO_SPACE.has(it.errorReason)) return null;
  if (it.sourceKind !== "file" && it.sourceKind !== "image") return null;
  return it.resolvedDest ? `${it.resolvedDest}.ava-part` : null;
}
