// Card ④ Queue: several games converted one after another on this computer; each finished
// package is handed to the console queue when it should install. What each game becomes, and
// what happens after, is the one choice made under ③.

import { ArrowDown, ArrowUp, FolderPlus, ListPlus, X } from "lucide-react";

import { Button, Card } from "../../components";
import { buildExtension, type ConvertItem } from "../../state/convertQueue";
import { useTr } from "../../state/lang";

export interface QueueCardProps {
  items: ConvertItem[];
  running: boolean;
  /** Add the games in one folder (or the folder, when it is a game). */
  onScanFolder: () => void;
  /** Tick several game folders in one picker. */
  onPickFolders: () => void;
  /** Tick several images or archives in one picker. */
  onPickFiles: () => void;
  onStart: () => void;
  onStop: () => void;
  onRemove: (id: string) => void;
  onMove: (id: string, delta: -1 | 1) => void;
  onClearFinished: () => void;
}

function nameOf(source: string): string {
  return decodeURIComponent(source.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? source);
}

export function QueueCard(p: QueueCardProps) {
  const tr = useTr();
  const statusText: Record<ConvertItem["status"], string> = {
    pending: tr("cq_pending", undefined, "Waiting"),
    running: tr("cq_running", undefined, "Converting"),
    installing: tr("cq_installing", undefined, "Installing on the PS5"),
    handed: tr("cq_handed", undefined, "Sent to the console's queue"),
    done: tr("cq_done", undefined, "Done"),
    failed: tr("cq_failed", undefined, "Failed"),
  };
  const pending = p.items.filter((i) => i.status === "pending").length;
  const finished = p.items.some((i) => i.status === "done" || i.status === "failed" || i.status === "handed");
  return (
    <Card>
      <div className="flex flex-col gap-3">
        <div className="flex items-center justify-between gap-2">
          <div className="text-sm font-medium">{tr("cq_title", undefined, "④ Queue")}</div>
          <div className="flex gap-2">
            {p.running ? (
              <Button size="sm" onClick={p.onStop}>
                {tr("cq_stop", undefined, "Stop after this one")}
              </Button>
            ) : pending > 0 ? (
              <Button size="sm" variant="primary" onClick={p.onStart}>
                {tr("cq_start", { count: pending }, "Convert {count} queued")}
              </Button>
            ) : null}
            {finished && (
              <Button size="sm" variant="ghost" onClick={p.onClearFinished}>
                {tr("cq_clear", undefined, "Clear finished")}
              </Button>
            )}
          </div>
        </div>

        <div className="-mt-1 text-xs text-[var(--color-muted)]">
          {tr(
            "cq_uses_choices",
            undefined,
            "Each game is made, and then kept, installed or uploaded, as chosen under ③.",
          )}
        </div>

        <div className="flex flex-wrap gap-2">
          <Button size="sm" variant="ghost" leftIcon={<ListPlus size={14} />} onClick={p.onScanFolder}>
            {tr("batch_scan", undefined, "Add games from a folder…")}
          </Button>
          <Button size="sm" variant="ghost" leftIcon={<FolderPlus size={14} />} onClick={p.onPickFolders}>
            {tr("cq_pick_folders", undefined, "Pick game folders…")}
          </Button>
          <Button size="sm" variant="ghost" leftIcon={<ListPlus size={14} />} onClick={p.onPickFiles}>
            {tr("cq_pick_files", undefined, "Pick packages, images or archives…")}
          </Button>
        </div>

        {p.items.length > 0 && (
          <ul className="divide-y divide-[var(--color-border)] rounded-[var(--radius-card)] border border-[var(--glass-edge)]">
            {p.items.map((i, idx) => (
              <li key={i.id} className="flex items-center gap-2 px-3 py-2 text-sm">
                <div className="min-w-0 flex-1">
                  <div className="flex min-w-0 items-center gap-1.5">
                    <span className="truncate font-medium" title={i.source}>
                      {nameOf(i.source)}
                    </span>
                    <span
                      className="shrink-0 rounded bg-[var(--color-surface-3)] px-1.5 py-px font-mono text-[10px] text-[var(--color-muted)]"
                      data-testid="cq-ext"
                    >
                      {i.build?.kind === "folder" ? tr("cq_ext_folder", undefined, "folder") : buildExtension(i.build)}
                    </span>
                  </div>
                  <div
                    className={`truncate text-xs ${i.status === "failed" ? "text-[var(--color-bad)]" : "text-[var(--color-muted)]"}`}
                  >
                    {statusText[i.status]}
                    {i.error ? ` — ${i.error}` : i.packagePath ? ` — ${i.packagePath}` : ""}
                  </div>
                </div>
                {i.status === "pending" && (
                  <>
                    <button
                      type="button"
                      disabled={idx === 0}
                      onClick={() => p.onMove(i.id, -1)}
                      aria-label={tr("queue_move_up", undefined, "Move up")}
                      className="rounded-full p-1.5 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] disabled:opacity-30"
                    >
                      <ArrowUp size={14} />
                    </button>
                    <button
                      type="button"
                      disabled={idx === p.items.length - 1}
                      onClick={() => p.onMove(i.id, 1)}
                      aria-label={tr("queue_move_down", undefined, "Move down")}
                      className="rounded-full p-1.5 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] disabled:opacity-30"
                    >
                      <ArrowDown size={14} />
                    </button>
                  </>
                )}
                {i.status !== "running" && i.status !== "installing" && (
                  <button
                    type="button"
                    onClick={() => p.onRemove(i.id)}
                    aria-label={tr("queue_remove", undefined, "Remove from queue")}
                    className="rounded-full p-1.5 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)]"
                  >
                    <X size={14} />
                  </button>
                )}
              </li>
            ))}
          </ul>
        )}
      </div>
    </Card>
  );
}
