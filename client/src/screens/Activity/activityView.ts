import type { ActivityEntry } from "../../state/activityHistory";

export type ActivityTab = "tasks" | "history" | "stats";

/** The tab a `?tab=` value opens. Old links still land: the Timeline view is
 *  now part of History, and the Telemetry view (a copy of Hardware) is gone. */
export function parseActivityTab(raw: string | null): ActivityTab {
  if (raw === "history" || raw === "timeline") return "history";
  if (raw === "stats") return "stats";
  return "tasks";
}

/** What a running row's button really does:
 *  - "cancel": the operation is aborted (engine job cancel, FS_OP_CANCEL, or
 *    the bulk-op loop's cancel flag).
 *  - "stop-watching": the app stops following it; a download's engine job has
 *    no cancel, so it finishes on its own.
 *  - null: nothing the row can stop (e.g. a launch); no button. */
export function stopActionFor(
  entry: Pick<ActivityEntry, "kind" | "opId" | "addr">,
): "cancel" | "stop-watching" | null {
  if (entry.opId !== undefined && entry.addr) return "cancel";
  switch (entry.kind) {
    case "fs-delete":
    case "fs-paste-copy":
    case "fs-paste-move":
    case "upload":
    case "upload-dir":
    case "upload-reconcile":
    case "upload-queue":
      return "cancel";
    case "download":
      return "stop-watching";
    default:
      return null;
  }
}
