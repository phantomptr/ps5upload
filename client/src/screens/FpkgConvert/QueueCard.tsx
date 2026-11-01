// Card ⑤ Queue: several games made one after another on this computer. Games are added under
// ① (several picked, or a folder of games) or with ④'s "Add this game to the queue"; this card
// only lists them. Each takes ②–④'s choices as they were when it was added. A game that should
// install or upload is handed to Upload & Install's queue once it is made: the transfer is
// that screen's, never a second one here.

import { useState } from "react";
import { ArrowDown, ArrowUp, Send, SlidersHorizontal, X } from "lucide-react";

import { Button, Card } from "../../components";
import { buildExtension, type ConvertItem, type ConvertItemChange } from "../../state/convertQueue";
import { useTr } from "../../state/lang";
import { QueueItemOptions } from "./QueueItemOptions";

export interface QueueCardProps {
  items: ConvertItem[];
  running: boolean;
  /** Open Upload & Install, where a made game installs or uploads. */
  onOpenUpload: () => void;
  onStart: () => void;
  onStop: () => void;
  onRemove: (id: string) => void;
  /** Change a waiting game's own choices. */
  onUpdate: (id: string, change: ConvertItemChange) => void;
  /** A console answers: installing and uploading can be chosen. */
  canInstall: boolean;
  host: string | null;
  onMove: (id: string, delta: -1 | 1) => void;
  onClearFinished: () => void;
}

function nameOf(source: string): string {
  return decodeURIComponent(source.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? source);
}

export function QueueCard(p: QueueCardProps) {
  const tr = useTr();
  // The waiting game whose choices are open.
  const [editing, setEditing] = useState<string | null>(null);
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
  const sent = p.items.some((i) => i.status === "handed" || i.status === "installing");
  // Nothing queued: no card. ① says how games get here.
  if (p.items.length === 0) return null;
  const send = tr("send_title_v2", undefined, "Upload & Install");
  return (
    <Card padded={false} className="p-3.5 sm:p-4">
      <div className="flex flex-col gap-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="text-sm font-medium">{tr("cq_title_v2", undefined, "⑤ Queue")}</div>
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

        <div className="-mt-1 text-xs text-[var(--color-muted)]" data-testid="cq-about">
          {tr(
            "cq_about",
            undefined,
            "Made one after another on this computer, each as ②, ③ and ④ were set when it was added. Add more under ①, even while the queue runs; change a waiting game with its options button.",
          )}{" "}
          {tr(
            "cq_then_send",
            { send },
            "A game set to install or upload goes on to {send}'s queue once it is made.",
          )}
        </div>

        {sent && (
          <div>
            <Button size="sm" variant="secondary" leftIcon={<Send size={14} />} onClick={p.onOpenUpload}>
              {tr("cq_open_send", { send }, "Open {send}")}
            </Button>
          </div>
        )}

        {p.items.length > 0 && (
          <ul className="divide-y divide-[var(--color-border)] rounded-[var(--radius-card)] border border-[var(--glass-edge)]">
            {p.items.map((i, idx) => (
              <li key={i.id} className="flex flex-col">
              <div className="flex items-center gap-2 px-3 py-2 text-sm">
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
                      onClick={() => setEditing(editing === i.id ? null : i.id)}
                      aria-label={tr("cq_options", undefined, "This game's choices")}
                      aria-expanded={editing === i.id}
                      title={tr("cq_options", undefined, "This game's choices")}
                      className={`rounded-full p-1.5 hover:bg-[var(--color-surface-3)] ${editing === i.id ? "text-[var(--color-accent-bright)]" : "text-[var(--color-muted)]"}`}
                    >
                      <SlidersHorizontal size={14} />
                    </button>
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
              </div>
              {editing === i.id && i.status === "pending" && (
                <QueueItemOptions
                  item={i}
                  canInstall={p.canInstall}
                  host={p.host}
                  onChange={(change) => p.onUpdate(i.id, change)}
                />
              )}
              </li>
            ))}
          </ul>
        )}
      </div>
    </Card>
  );
}
