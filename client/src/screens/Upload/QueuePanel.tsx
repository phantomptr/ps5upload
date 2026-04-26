import { useEffect } from "react";
import {
  ArrowDown,
  ArrowUp,
  Loader2,
  CheckCircle2,
  XCircle,
  CircleDashed,
  Play,
  Square,
  Trash2,
  RotateCcw,
  ListOrdered,
  X,
} from "lucide-react";

import { Button } from "../../components";
import { useTr } from "../../state/lang";
import {
  useUploadQueueStore,
  type QueueItem,
  type QueueItemStatus,
} from "../../state/uploadQueue";

/** Inline queue panel rendered below the single-shot upload UI on the
 *  Upload screen. Visible only when the queue has items OR while the
 *  user is hydrating from disk so a perpetual blank slot doesn't waste
 *  space on first launch. */
export function QueuePanel() {
  const tr = useTr();
  const items = useUploadQueueStore((s) => s.items);
  const continueOnFailure = useUploadQueueStore((s) => s.continueOnFailure);
  const running = useUploadQueueStore((s) => s.running);
  const loaded = useUploadQueueStore((s) => s.loaded);
  const hydrate = useUploadQueueStore((s) => s.hydrate);
  const start = useUploadQueueStore((s) => s.start);
  const stop = useUploadQueueStore((s) => s.stop);
  const clear = useUploadQueueStore((s) => s.clear);
  const remove = useUploadQueueStore((s) => s.remove);
  const moveUp = useUploadQueueStore((s) => s.moveUp);
  const moveDown = useUploadQueueStore((s) => s.moveDown);
  const retryFailed = useUploadQueueStore((s) => s.retryFailed);
  const setContinueOnFailure = useUploadQueueStore(
    (s) => s.setContinueOnFailure,
  );

  // Hydrate once on mount. The store exposes a `loaded` flag so we
  // don't re-hydrate on screen re-mount; subsequent visits read from
  // the in-memory state set by the first hydrate.
  useEffect(() => {
    if (!loaded) void hydrate();
  }, [loaded, hydrate]);

  if (items.length === 0) return null;

  const pendingCount = items.filter((i) => i.status === "pending").length;
  const failedCount = items.filter((i) => i.status === "failed").length;
  const doneCount = items.filter((i) => i.status === "done").length;

  return (
    <section className="mb-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5">
      <header className="mb-4 flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-2 text-sm font-semibold">
          <ListOrdered size={14} />
          <span>{tr("queue_title", undefined, "Upload queue")}</span>
          <span className="text-xs font-normal text-[var(--color-muted)]">
            ·{" "}
            {tr(
              "queue_count",
              {
                total: items.length,
                done: doneCount,
                pending: pendingCount,
                failed: failedCount,
              },
              `${items.length} total · ${doneCount} done · ${pendingCount} pending · ${failedCount} failed`,
            )}
          </span>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <label className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
            <input
              type="checkbox"
              checked={continueOnFailure}
              onChange={(e) => setContinueOnFailure(e.target.checked)}
              disabled={running}
              className="h-3.5 w-3.5"
            />
            {tr(
              "queue_continue_on_failure",
              undefined,
              "Continue on failure",
            )}
          </label>

          {failedCount > 0 && !running && (
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<RotateCcw size={12} />}
              onClick={retryFailed}
            >
              {tr("queue_retry_failed", undefined, "Retry failed")}
            </Button>
          )}

          {running ? (
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<Square size={12} />}
              onClick={stop}
            >
              {tr("queue_stop", undefined, "Stop")}
            </Button>
          ) : (
            <Button
              variant="primary"
              size="sm"
              leftIcon={<Play size={12} />}
              onClick={() => void start()}
              disabled={pendingCount === 0}
            >
              {tr("queue_start", undefined, "Start")}
            </Button>
          )}

          <Button
            variant="ghost"
            size="sm"
            leftIcon={<Trash2 size={12} />}
            onClick={clear}
            disabled={running}
            title={tr(
              "queue_clear_tooltip",
              undefined,
              "Remove every item from the queue (including completed ones)",
            )}
          >
            {tr("queue_clear", undefined, "Clear all")}
          </Button>
        </div>
      </header>

      <ul className="grid gap-2">
        {items.map((item) => (
          <QueueRow
            key={item.id}
            item={item}
            running={running}
            onMoveUp={() => moveUp(item.id)}
            onMoveDown={() => moveDown(item.id)}
            onRemove={() => remove(item.id)}
          />
        ))}
      </ul>
    </section>
  );
}

function QueueRow({
  item,
  running,
  onMoveUp,
  onMoveDown,
  onRemove,
}: {
  item: QueueItem;
  running: boolean;
  onMoveUp: () => void;
  onMoveDown: () => void;
  onRemove: () => void;
}) {
  const tr = useTr();
  const pct =
    item.totalBytes > 0
      ? Math.max(0, Math.min(100, (item.bytesSent / item.totalBytes) * 100))
      : 0;
  const isActive = item.status === "running";
  // Lock reorder + remove while the runner is touching this row;
  // mutating the array under the runner's iterator would surprise
  // both the user (item disappears mid-upload) and the engine (jobId
  // drift on a shifted index).
  const lockRow = isActive;

  return (
    <li
      className={`rounded-md border p-3 text-sm transition-colors ${
        item.status === "failed"
          ? "border-[var(--color-bad)]"
          : item.status === "done"
            ? "border-[var(--color-good)]"
            : isActive
              ? "border-[var(--color-accent)]"
              : "border-[var(--color-border)]"
      } bg-[var(--color-surface)]`}
    >
      <div className="flex items-start gap-3">
        <StatusIcon status={item.status} />
        <div className="min-w-0 flex-1">
          <div className="truncate font-medium">{item.displayName}</div>
          <div className="mt-0.5 truncate font-mono text-[11px] text-[var(--color-muted)]">
            → {item.resolvedDest}
          </div>
          <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-[11px] text-[var(--color-muted)]">
            <span>
              {tr(
                `queue_strategy_${item.strategy}`,
                undefined,
                item.strategy === "resume" ? "Resume" : "Overwrite",
              )}
            </span>
            {item.excludes.length > 0 && (
              <span>
                {tr(
                  "queue_excludes",
                  { count: item.excludes.length },
                  `${item.excludes.length} exclude${
                    item.excludes.length === 1 ? "" : "s"
                  }`,
                )}
              </span>
            )}
            {item.mountAfterUpload && (
              <span>
                {tr(
                  "queue_will_mount",
                  undefined,
                  "mount after upload",
                )}
              </span>
            )}
            {item.mountedAt && (
              <span className="font-mono text-[var(--color-accent)]">
                {tr(
                  "queue_mounted_at",
                  { mount: item.mountedAt },
                  `mounted at ${item.mountedAt}`,
                )}
              </span>
            )}
          </div>
        </div>

        <div className="flex shrink-0 items-center gap-1">
          <button
            type="button"
            onClick={onMoveUp}
            disabled={lockRow || running}
            title={tr("queue_move_up", undefined, "Move up")}
            className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] disabled:opacity-30"
          >
            <ArrowUp size={14} />
          </button>
          <button
            type="button"
            onClick={onMoveDown}
            disabled={lockRow || running}
            title={tr("queue_move_down", undefined, "Move down")}
            className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] disabled:opacity-30"
          >
            <ArrowDown size={14} />
          </button>
          <button
            type="button"
            onClick={onRemove}
            disabled={lockRow}
            title={tr("queue_remove", undefined, "Remove from queue")}
            className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-bad)] hover:text-[var(--color-accent-contrast)] disabled:opacity-30"
          >
            <X size={14} />
          </button>
        </div>
      </div>

      {isActive && (
        <div className="mt-2">
          <div className="mb-1 flex items-baseline justify-between text-[11px] text-[var(--color-muted)]">
            <span>
              {formatBytes(item.bytesSent)} / {formatBytes(item.totalBytes)}
            </span>
            <span className="tabular-nums">{pct.toFixed(0)}%</span>
          </div>
          <div className="h-1.5 w-full overflow-hidden rounded-full bg-[var(--color-surface-3)]">
            <div
              className="h-full bg-[var(--color-accent)] transition-[width] duration-300"
              style={{ width: `${pct}%` }}
            />
          </div>
        </div>
      )}

      {item.status === "failed" && item.error && (
        <div className="mt-2 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface-2)] p-2 text-[11px] text-[var(--color-bad)]">
          {item.error}
        </div>
      )}
    </li>
  );
}

function StatusIcon({ status }: { status: QueueItemStatus }) {
  switch (status) {
    case "pending":
      return (
        <CircleDashed
          size={18}
          className="mt-0.5 shrink-0 text-[var(--color-muted)]"
        />
      );
    case "running":
      return (
        <Loader2
          size={18}
          className="mt-0.5 shrink-0 animate-spin text-[var(--color-accent)]"
        />
      );
    case "done":
      return (
        <CheckCircle2
          size={18}
          className="mt-0.5 shrink-0 text-[var(--color-good)]"
        />
      );
    case "failed":
      return (
        <XCircle
          size={18}
          className="mt-0.5 shrink-0 text-[var(--color-bad)]"
        />
      );
  }
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v.toFixed(v >= 100 ? 0 : v >= 10 ? 1 : 2)} ${units[i]}`;
}
