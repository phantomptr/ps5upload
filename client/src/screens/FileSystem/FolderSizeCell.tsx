// A folder's Size cell in the Files listing (#974): the size the engine already knows, with how old
// it is, or where its measuring stands. A folder nobody measured shows "—", as before.

import { Loader2 } from "lucide-react";

import type { FolderSize, MeasureQueue } from "../../api/folderSizes";
import { formatBytes } from "../../lib/format";
import { formatAgo, formatDate } from "../../lib/formatDate";
import type { Translator } from "../../state/lang";

export type FolderSizeState =
  | { kind: "unknown" }
  | { kind: "known"; size: FolderSize }
  | { kind: "queued" }
  | { kind: "running" }
  | { kind: "paused" };

const norm = (p: string) => p.replace(/\/+$/, "") || "/";

/** Where the folder at `path` stands: being measured, waiting, paused, known or unknown. */
export function folderSizeState(
  path: string,
  known: FolderSize | undefined,
  queue: MeasureQueue,
): FolderSizeState {
  const p = norm(path);
  const running = queue.running !== null && norm(queue.running.path) === p;
  const queued = queue.queued.some((q) => norm(q.path) === p);
  if ((running || queued) && queue.paused) return { kind: "paused" };
  if (running) return { kind: "running" };
  if (queued) return { kind: "queued" };
  return known ? { kind: "known", size: known } : { kind: "unknown" };
}

/** The value a size sort uses for a folder: its known size, or undefined (sorted last). */
export function knownFolderBytes(
  children: Record<string, FolderSize> | undefined,
  name: string,
): number | undefined {
  return children?.[name]?.bytes;
}

/** The tooltip of a known size: when, from where, that it does not update by itself. */
export function folderSizeTitle(size: FolderSize, tr: Translator, now = Date.now()): string {
  const when = formatDate(Math.min(size.measured_at_ms, now), "datetime");
  const from =
    size.source === "space"
      ? tr("fs_size_from_space", undefined, "Counted by What takes space?")
      : size.source === "transfer"
        ? tr("fs_size_from_transfer", undefined, "Counted from a PS5Upload transfer into it")
        : size.source === "cleanup"
          ? tr("fs_size_from_cleanup", undefined, "Counted by a Clean up scan")
          : tr("fs_size_from_measured", undefined, "Measured by PS5Upload");
  const lines = [
    `${from}, ${when}.`,
    size.stored
      ? tr("fs_size_stored", undefined, "Space it takes on the drive.")
      : tr("fs_size_logical", undefined, "Total size of its files."),
  ];
  if (size.files !== null)
    lines.push(tr("fs_size_files", { count: size.files }, "{count} files."));
  if (size.partial)
    lines.push(
      tr(
        "fs_size_partial",
        undefined,
        "Some folders inside could not be read, so it may be larger.",
      ),
    );
  lines.push(
    tr(
      "fs_size_not_live",
      undefined,
      "This size does not update by itself. Use Measure size to count it again.",
    ),
  );
  return lines.join("\n");
}

/** The cell itself. `className` carries the row's own layout classes. */
export function FolderSizeCell({
  state,
  tr,
  className = "",
}: {
  state: FolderSizeState;
  tr: Translator;
  className?: string;
}) {
  const base = `shrink-0 whitespace-nowrap text-xs text-[var(--color-muted)] tabular-nums ${className}`;
  switch (state.kind) {
    case "unknown":
      return (
        <span className={base} data-testid="fs-folder-size" data-state="unknown">
          —
        </span>
      );
    case "known":
      return (
        <span
          className={base}
          data-testid="fs-folder-size"
          data-state="known"
          title={folderSizeTitle(state.size, tr)}
        >
          <span className="text-[var(--color-text)]" dir="ltr">
            {state.size.partial ? "≥ " : ""}
            {formatBytes(state.size.bytes)}
          </span>
          <span className="opacity-70"> · {formatAgo(state.size.measured_at_ms)}</span>
        </span>
      );
    case "running":
      return (
        <span
          className={`${base} inline-flex items-center gap-1`}
          data-testid="fs-folder-size"
          data-state="running"
        >
          <Loader2 size={11} className="animate-spin" aria-hidden />
          {tr("fs_size_measuring", undefined, "Measuring…")}
        </span>
      );
    case "queued":
      return (
        <span className={`${base} opacity-70`} data-testid="fs-folder-size" data-state="queued">
          {tr("fs_size_waiting", undefined, "Waiting…")}
        </span>
      );
    case "paused":
      return (
        <span
          className={`${base} opacity-70`}
          data-testid="fs-folder-size"
          data-state="paused"
          title={tr(
            "fs_size_paused_hint",
            undefined,
            "Measuring waits while a transfer or install runs on this console.",
          )}
        >
          {tr("fs_size_paused", undefined, "Paused")}
        </span>
      );
  }
}
