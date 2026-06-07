import { useEffect, useState } from "react";
import {
  ArrowDown,
  ArrowUp,
  Loader2,
  CheckCircle2,
  XCircle,
  CircleDashed,
  ChevronRight,
  Play,
  Square,
  Trash2,
  RotateCcw,
  ListOrdered,
  X,
} from "lucide-react";

import { Button } from "../../components";
import { humanizeJobErrorReason } from "../../api/ps5";
import { hostOf } from "../../lib/addr";
import { formatBytes, formatDuration } from "../../lib/format";
import { MAX_AUTO_RECOVER_ATTEMPTS } from "../../lib/uploadRecovery";
import { useTr } from "../../state/lang";
import { useConsoleLabel } from "../../state/roster";
import {
  useUploadQueueStore,
  type QueueItem,
  type QueueItemStatus,
} from "../../state/uploadQueue";
import { useTransferStore } from "../../state/transfer";

/** One console's slice of the queue, in first-seen order. */
interface ConsoleGroup {
  host: string;
  items: QueueItem[];
}

/** Partition the flat queue into per-console groups, preserving the order
 *  each console first appears so the layout is stable as items run/finish. */
function groupByConsole(items: QueueItem[]): ConsoleGroup[] {
  const groups: ConsoleGroup[] = [];
  const idx = new Map<string, number>();
  for (const it of items) {
    const h = hostOf(it.addr);
    let gi = idx.get(h);
    if (gi === undefined) {
      gi = groups.length;
      idx.set(h, gi);
      groups.push({ host: h, items: [] });
    }
    groups[gi].items.push(it);
  }
  return groups;
}

/** Inline queue panel rendered below the single-shot upload UI on the
 *  Upload screen. Visible only when the queue has items OR while the
 *  user is hydrating from disk so a perpetual blank slot doesn't waste
 *  space on first launch.
 *
 *  The queue is GROUPED BY CONSOLE: each PS5 with queued work gets its
 *  own collapsible section with its own Start/Stop, and the consoles
 *  upload in parallel. The top-level Start all / Stop all drives every
 *  console at once. This is what lets a queue holding games for 3
 *  different consoles actually upload to all 3 — and reorder one
 *  console's list while another console is mid-upload. */
export function QueuePanel() {
  const tr = useTr();
  const items = useUploadQueueStore((s) => s.items);
  const continueOnFailure = useUploadQueueStore((s) => s.continueOnFailure);
  const running = useUploadQueueStore((s) => s.running);
  const runningHosts = useUploadQueueStore((s) => s.runningHosts);
  const loaded = useUploadQueueStore((s) => s.loaded);
  const hydrate = useUploadQueueStore((s) => s.hydrate);
  const start = useUploadQueueStore((s) => s.start);
  const stop = useUploadQueueStore((s) => s.stop);
  const startHost = useUploadQueueStore((s) => s.startHost);
  const stopHost = useUploadQueueStore((s) => s.stopHost);
  const clear = useUploadQueueStore((s) => s.clear);
  const remove = useUploadQueueStore((s) => s.remove);
  const moveUp = useUploadQueueStore((s) => s.moveUp);
  const moveDown = useUploadQueueStore((s) => s.moveDown);
  const retryFailed = useUploadQueueStore((s) => s.retryFailed);
  const setContinueOnFailure = useUploadQueueStore(
    (s) => s.setContinueOnFailure,
  );
  // (2.11.0) Mutual-exclusion with the Upload-screen one-shot
  // transfer. The PS5 payload's transfer port is single-client,
  // so a queue Start while a one-shot is in flight would block at
  // the socket and the UI would show two "running" things. Gate
  // the Start buttons on transferInFlight; the Upload screen's
  // Upload button does the symmetric disable on `queueRunning`.
  // Any one-shot upload active on ANY console. One-shots are per-console now
  // (phasesByHost), but the queue Start button stays conservatively gated on
  // "any one-shot in flight" — the per-console transfer-port collision is
  // handled inside the queue runner, and this keeps the existing safe UX.
  const transferInFlight = useTransferStore((s) =>
    Object.values(s.phasesByHost).some(
      (p) => p.kind === "starting" || p.kind === "running",
    ),
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

  const groups = groupByConsole(items);
  // Single-console queues don't need a per-console sub-header — the
  // top-level Start already targets that one console. Only fan out the
  // grouped chrome once there's more than one console in play.
  const multiConsole = groups.length > 1;

  return (
    <section className="mb-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5">
      <header className="mb-4 flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-2 text-sm font-semibold">
          <ListOrdered size={14} />
          <span>{tr("queue_title", undefined, "Upload queue")}</span>
          {multiConsole && (
            <span className="rounded bg-[var(--color-surface-3)] px-1.5 py-0.5 text-xs font-medium text-[var(--color-muted)]">
              {tr(
                "queue_console_total",
                { count: groups.length },
                `${groups.length} consoles`,
              )}
            </span>
          )}
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
              "{total} total · {done} done · {pending} pending · {failed} failed",
            )}
          </span>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <label className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
            <input
              type="checkbox"
              checked={continueOnFailure}
              onChange={(e) => setContinueOnFailure(e.target.checked)}
              className="h-3.5 w-3.5"
            />
            {tr("queue_continue_on_failure", undefined, "Continue on failure")}
          </label>

          {failedCount > 0 && (
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
              {multiConsole
                ? tr("queue_stop_all", undefined, "Stop all")
                : tr("queue_stop", undefined, "Stop")}
            </Button>
          ) : (
            <Button
              variant="primary"
              size="sm"
              leftIcon={<Play size={12} />}
              onClick={() => void start()}
              disabled={pendingCount === 0 || transferInFlight}
              title={
                transferInFlight
                  ? tr(
                      "queue_disabled_oneshot_in_flight",
                      undefined,
                      "A one-shot upload is in flight — wait for it to finish before starting the queue.",
                    )
                  : undefined
              }
            >
              {multiConsole
                ? tr("queue_start_all", undefined, "Start all")
                : tr("queue_start", undefined, "Start")}
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

      <div className="grid gap-4">
        {groups.map((g) => (
          <ConsoleGroup
            key={g.host}
            host={g.host}
            items={g.items}
            hostRunning={!!runningHosts[g.host]}
            transferInFlight={transferInFlight}
            showHeader={multiConsole}
            onStartHost={() => void startHost(g.host)}
            onStopHost={() => stopHost(g.host)}
            onMoveUp={moveUp}
            onMoveDown={moveDown}
            onRemove={remove}
          />
        ))}
      </div>
    </section>
  );
}

/** One console's section: a header naming the PS5 with its own Start/Stop
 *  + counts, then that console's queued rows. Collapsible so a queue with
 *  several consoles stays scannable. */
function ConsoleGroup({
  host,
  items,
  hostRunning,
  transferInFlight,
  showHeader,
  onStartHost,
  onStopHost,
  onMoveUp,
  onMoveDown,
  onRemove,
}: {
  host: string;
  items: QueueItem[];
  hostRunning: boolean;
  transferInFlight: boolean;
  showHeader: boolean;
  onStartHost: () => void;
  onStopHost: () => void;
  onMoveUp: (id: string) => void;
  onMoveDown: (id: string) => void;
  onRemove: (id: string) => void;
}) {
  const tr = useTr();
  const label = useConsoleLabel(host);
  const [collapsed, setCollapsed] = useState(false);

  const runningN = items.filter((i) => i.status === "running").length;
  const pending = items.filter((i) => i.status === "pending").length;
  const done = items.filter((i) => i.status === "done").length;
  const failed = items.filter((i) => i.status === "failed").length;

  const rows = (
    <ul className="grid gap-2">
      {items.map((item) => (
        <QueueRow
          key={item.id}
          item={item}
          onMoveUp={() => onMoveUp(item.id)}
          onMoveDown={() => onMoveDown(item.id)}
          onRemove={() => onRemove(item.id)}
        />
      ))}
    </ul>
  );

  // Single-console queue: render the rows bare (the top-level header
  // already names the only console in play).
  if (!showHeader) return rows;

  return (
    <div
      className={`rounded-md border ${
        hostRunning
          ? "border-[var(--color-accent)]"
          : "border-[var(--color-border)]"
      } bg-[var(--color-surface)]`}
    >
      <div className="flex flex-wrap items-center justify-between gap-2 px-3 py-2">
        <button
          type="button"
          onClick={() => setCollapsed((c) => !c)}
          className="flex min-w-0 items-center gap-2 text-left"
          title={
            collapsed
              ? tr("queue_group_expand", undefined, "Show this console's queue")
              : tr("queue_group_collapse", undefined, "Hide this console's queue")
          }
        >
          <ChevronRight
            size={14}
            className={`shrink-0 text-[var(--color-muted)] transition-transform ${
              collapsed ? "" : "rotate-90"
            }`}
          />
          <span aria-hidden>🖥</span>
          <span className="truncate text-sm font-semibold">{label}</span>
          <span className="shrink-0 font-mono text-xs text-[var(--color-muted)]">
            {host}
          </span>
        </button>

        <div className="flex items-center gap-3">
          <span className="text-xs text-[var(--color-muted)]">
            {tr(
              "queue_group_summary",
              { running: runningN, pending, done, failed },
              `${runningN} uploading · ${pending} queued · ${done} done${
                failed ? ` · ${failed} failed` : ""
              }`,
            )}
          </span>
          {hostRunning ? (
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<Square size={12} />}
              onClick={onStopHost}
            >
              {tr("queue_stop", undefined, "Stop")}
            </Button>
          ) : (
            <Button
              variant="primary"
              size="sm"
              leftIcon={<Play size={12} />}
              onClick={onStartHost}
              disabled={pending === 0 || transferInFlight}
              title={
                transferInFlight
                  ? tr(
                      "queue_disabled_oneshot_in_flight",
                      undefined,
                      "A one-shot upload is in flight — wait for it to finish before starting the queue.",
                    )
                  : pending === 0
                    ? tr(
                        "queue_group_nothing_pending",
                        undefined,
                        "Nothing queued for this console",
                      )
                    : undefined
              }
            >
              {tr("queue_start", undefined, "Start")}
            </Button>
          )}
        </div>
      </div>

      {!collapsed && <div className="px-3 pb-3">{rows}</div>}
    </div>
  );
}

function QueueRow({
  item,
  onMoveUp,
  onMoveDown,
  onRemove,
}: {
  item: QueueItem;
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
  // Between auto-recovery attempts: the prior attempt failed for a
  // recoverable reason and the runner is waiting out a backoff / re-deploying
  // the payload before resuming. Status is still "running" so the row reads as
  // in-flight, but we swap the (now-zeroed) progress bar for a recovery banner.
  const isRecovering = isActive && !!item.recovering;
  // Finalize phase: all shards on the wire, engine waiting on PS5
  // commit. Drives a different chip + suppresses the stale ETA — see
  // the speed/eta render below. Gate on totalBytes > 0 so a row whose
  // stat is still pending (totalBytes === 0 on the first tick) doesn't
  // false-positive as finalized.
  const isFinalizing =
    isActive && item.totalBytes > 0 && item.bytesSent >= item.totalBytes;
  // Show ETA only when we have a real total + a real rate; otherwise
  // the readout would print "ETA Infinity" or "ETA 0s" right at the
  // start of a transfer where the smoother hasn't seen two samples yet.
  const remainingBytes = Math.max(0, item.totalBytes - item.bytesSent);
  const etaSec =
    item.bytesPerSec > 0 && remainingBytes > 0
      ? remainingBytes / item.bytesPerSec
      : null;
  // Lock reorder + remove ONLY while THIS row is the one actively
  // uploading — mutating the array under the runner's iterator would
  // surprise both the user (item disappears mid-upload) and the engine
  // (jobId drift on a shifted index). Crucially this is now per-ROW, not
  // a whole-queue lock: you can freely reorder a console's PENDING jobs
  // while another job (even on the same console) is uploading.
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
          <div className="mt-0.5 truncate font-mono text-xs text-[var(--color-muted)]">
            → {item.resolvedDest}
          </div>
          <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-xs text-[var(--color-muted)]">
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
                {tr("queue_will_mount", undefined, "mount after upload")}
              </span>
            )}
            {item.mountedAt && (
              <span className="font-mono text-[var(--color-accent)]">
                {tr(
                  "queue_mounted_at",
                  { mount: item.mountedAt },
                  "mounted at {mount}",
                )}
              </span>
            )}
          </div>
        </div>

        <div className="flex shrink-0 items-center gap-1">
          <button
            type="button"
            onClick={onMoveUp}
            disabled={lockRow}
            title={tr("queue_move_up", undefined, "Move up")}
            className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] disabled:opacity-30"
          >
            <ArrowUp size={14} />
          </button>
          <button
            type="button"
            onClick={onMoveDown}
            disabled={lockRow}
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

      {isRecovering && (
        <div className="mt-2 flex items-center gap-2 rounded-md bg-[var(--color-warn)]/10 px-2 py-1.5 text-xs text-[var(--color-warn)]">
          <RotateCcw size={12} className="shrink-0 animate-spin" />
          <span>
            {tr(
              "queue_recovering",
              {
                attempt: item.recoverAttempt ?? 1,
                max: MAX_AUTO_RECOVER_ATTEMPTS,
              },
              `Connection lost — re-deploying payload & resuming (${
                item.recoverAttempt ?? 1
              }/${MAX_AUTO_RECOVER_ATTEMPTS})…`,
            )}
          </span>
        </div>
      )}

      {isActive && !isRecovering && (
        <div className="mt-2">
          <div className="mb-1 flex flex-wrap items-baseline justify-between gap-x-3 text-xs text-[var(--color-muted)]">
            <span>
              {formatBytes(item.bytesSent)} / {formatBytes(item.totalBytes)}
              {/* Speed + ETA are honest signals only while bytes are
                  still moving. Once the row pegs at 100%, the engine
                  is waiting on the PS5's commit (drain ACKs + COMMIT
                  ACK across potentially tens-of-thousands of inodes)
                  and the saved bytesPerSec is a stale figure that no
                  longer reflects reality — same pattern as the
                  single-shot Upload banner. Replace with a
                  Finalizing… chip instead. */}
              {!isFinalizing && item.bytesPerSec > 0 && (
                <>
                  {" · "}
                  <span className="tabular-nums">
                    {formatBytes(item.bytesPerSec)}/s
                  </span>
                  {etaSec !== null && (
                    <>
                      {" · "}
                      <span className="tabular-nums">
                        {tr(
                          "queue_eta",
                          { eta: formatDuration(etaSec) },
                          "ETA {eta}",
                        )}
                      </span>
                    </>
                  )}
                </>
              )}
              {isFinalizing && (
                <>
                  {" · "}
                  <span
                    className="rounded-full bg-[var(--color-warn)]/15 px-1.5 py-0.5 text-xs font-medium text-[var(--color-warn)]"
                    title={tr(
                      "queue_phase_finalizing_hint",
                      undefined,
                      "All bytes are on the PS5; it's committing the file index. Large file counts (10k+) routinely take many minutes here — don't close the app.",
                    )}
                  >
                    {item.filesFinalizingTotal > 0
                      ? tr(
                          "queue_phase_finalizing_with_counter",
                          {
                            done: item.filesFinalized.toLocaleString(),
                            total: item.filesFinalizingTotal.toLocaleString(),
                          },
                          `Finalizing on PS5 — ${item.filesFinalized.toLocaleString()} / ${item.filesFinalizingTotal.toLocaleString()}`,
                        )
                      : tr(
                          "queue_phase_finalizing",
                          undefined,
                          "Finalizing on PS5",
                        )}
                  </span>
                </>
              )}
            </span>
            <span className="tabular-nums">{pct.toFixed(0)}%</span>
          </div>
          <div className="h-1.5 w-full overflow-hidden rounded-full bg-[var(--color-surface-3)]">
            <div
              className="h-full bg-[var(--color-accent)] transition-[width] duration-300"
              style={{ width: `${pct}%` }}
            />
          </div>
          {isFinalizing && (
            // Always-visible explainer under the bar. The pill itself
            // ("Finalizing on PS5") is short enough to fit on the
            // progress line, but the actionable "don't close the app"
            // sentence is what actually prevents the force-quit that
            // started this whole bug. Tooltip-hidden hints don't get
            // read on a Tauri desktop app — promote it.
            <div className="mt-1 text-xs text-[var(--color-warn)]">
              {tr(
                "queue_phase_finalizing_hint",
                undefined,
                "All bytes are on the PS5; it's committing the file index. Large file counts (10k+) routinely take many minutes here — don't close the app.",
              )}
            </div>
          )}
        </div>
      )}

      {item.status === "done" && item.bytesPerSec > 0 && (
        <div className="mt-2 text-xs text-[var(--color-muted)]">
          {formatBytes(item.bytesSent)}
          {" · "}
          <span className="tabular-nums">
            {tr(
              "queue_avg_speed",
              { speed: `${formatBytes(item.bytesPerSec)}/s` },
              "{speed}/s avg",
            )}
          </span>
        </div>
      )}

      {item.status === "failed" && item.error && (
        <FailedRowErrorCard
          rawError={item.error}
          reason={item.errorReason}
          detail={item.errorDetail}
        />
      )}
    </li>
  );
}

/** Layered error card: humanized hint first (if we recognize the
 *  payload's `error_reason`), then the payload's `detail` string,
 *  then a collapsed `<details>` carrying the raw error chain for
 *  power-user debugging. Falls back to plain raw-error rendering when
 *  no structured fields are present (engine-internal failures, older
 *  payloads). */
function FailedRowErrorCard({
  rawError,
  reason,
  detail,
}: {
  rawError: string;
  reason: string | null;
  detail: string | null;
}) {
  const tr = useTr();
  const humanized = humanizeJobErrorReason(reason ?? undefined);
  return (
    <div className="mt-2 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface-2)] p-2 text-xs text-[var(--color-bad)]">
      {humanized ? (
        <>
          <div className="font-medium">{humanized}</div>
          {detail && (
            <div className="mt-1 text-xs text-[var(--color-muted)]">
              {detail}
            </div>
          )}
          <details className="mt-1 cursor-pointer">
            <summary className="text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]">
              {tr("queue_raw_error", "raw error")}
            </summary>
            <code className="mt-1 block whitespace-pre-wrap break-all font-mono text-xs text-[var(--color-muted)]">
              {rawError}
              {reason && `\n[reason: ${reason}]`}
            </code>
          </details>
        </>
      ) : (
        <code className="block whitespace-pre-wrap break-all font-mono text-xs">
          {rawError}
        </code>
      )}
    </div>
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
        <XCircle size={18} className="mt-0.5 shrink-0 text-[var(--color-bad)]" />
      );
  }
}
