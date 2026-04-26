import { useEffect, useState } from "react";
import {
  Activity as ActivityIcon,
  Loader2,
  CheckCircle2,
  XCircle,
  StopCircle,
  Trash2,
} from "lucide-react";

import { PageHeader, Button, EmptyState } from "../../components";
import { useTr } from "../../state/lang";
import {
  useActivityHistoryStore,
  type ActivityEntry,
  type ActivityOutcome,
} from "../../state/activityHistory";
import { formatBytes, formatDuration } from "../../lib/format";
import { fsOpCancel } from "../../api/ps5";
import { useFsBulkOpStore, useFsDownloadOpStore } from "../../state/fsBulkOp";
import { useTransferStore } from "../../state/transfer";

/**
 * Cross-screen log of past + current operations. Reads from the
 * persistent `activityHistory` store (last 100 entries, kept in
 * localStorage). Lets the user answer "what just happened" without
 * re-tracing through engine logs.
 *
 * Layout: in-flight entries on top with live elapsed/progress, past
 * entries below sorted newest-first. A Clear button wipes history
 * (with confirm).
 */
export default function ActivityScreen() {
  const tr = useTr();
  const entries = useActivityHistoryStore((s) => s.entries);
  const clear = useActivityHistoryStore((s) => s.clear);
  const [confirmClear, setConfirmClear] = useState(false);

  const running = entries.filter((e) => e.outcome === "running");
  const past = entries.filter((e) => e.outcome !== "running");

  return (
    <div className="p-6">
      <PageHeader
        icon={ActivityIcon}
        title={tr("activity_title", undefined, "Activity")}
        description={tr(
          "activity_description",
          undefined,
          "Your last 100 operations across uploads, downloads, and file management. Persisted across app restarts.",
        )}
        right={
          entries.length > 0 ? (
            <Button
              variant="ghost"
              size="sm"
              leftIcon={<Trash2 size={12} />}
              onClick={() => setConfirmClear(true)}
            >
              {tr("activity_clear", undefined, "Clear history")}
            </Button>
          ) : null
        }
      />

      {entries.length === 0 && (
        <EmptyState
          icon={ActivityIcon}
          size="hero"
          title={tr("activity_empty_title", undefined, "No activity yet")}
          message={tr(
            "activity_empty_message",
            undefined,
            "Uploads, downloads, and file system operations show up here as you trigger them.",
          )}
        />
      )}

      {running.length > 0 && (
        <section className="mb-6">
          <header className="mb-3 flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
            <Loader2 size={13} className="animate-spin" />
            {tr("activity_running", undefined, "Running now")}
            <span className="text-[10px]">· {running.length}</span>
          </header>
          <ul className="space-y-2">
            {running.map((e) => (
              <ActivityRow key={e.id} entry={e} />
            ))}
          </ul>
        </section>
      )}

      {past.length > 0 && (
        <section>
          <header className="mb-3 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
            {tr("activity_past", undefined, "Recent")}
            <span className="ml-2 text-[10px]">· {past.length}</span>
          </header>
          <ul className="space-y-2">
            {past.map((e) => (
              <ActivityRow key={e.id} entry={e} />
            ))}
          </ul>
        </section>
      )}

      {confirmClear && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
          onClick={() => setConfirmClear(false)}
        >
          <div
            role="alertdialog"
            aria-modal="true"
            className="w-full max-w-md rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5"
            onClick={(e) => e.stopPropagation()}
          >
            <header className="mb-2 text-sm font-semibold">
              {tr("activity_clear_confirm_title", undefined, "Clear all activity?")}
            </header>
            <p className="mb-4 text-xs text-[var(--color-muted)]">
              {tr(
                "activity_clear_confirm_body",
                undefined,
                "Removes the saved history of past operations. In-flight operations are not affected.",
              )}
            </p>
            <div className="flex items-center justify-end gap-2">
              <Button
                variant="ghost"
                size="sm"
                onClick={() => setConfirmClear(false)}
              >
                {tr("cancel", undefined, "Cancel")}
              </Button>
              <Button
                variant="danger"
                size="sm"
                onClick={() => {
                  clear();
                  setConfirmClear(false);
                }}
                autoFocus
              >
                {tr("activity_clear", undefined, "Clear history")}
              </Button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function ActivityRow({ entry }: { entry: ActivityEntry }) {
  const tr = useTr();
  const isRunning = entry.outcome === "running";
  // For finished rows we have a fixed end timestamp (pure subtract).
  // For running rows we tick `now` every second so the elapsed
  // counter advances; this also drives the speed/percent re-renders.
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!isRunning) return;
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [isRunning]);
  const elapsedMs =
    entry.endedAtMs !== null
      ? entry.endedAtMs - entry.startedAtMs
      : now - entry.startedAtMs;
  const speed =
    entry.bytes && entry.bytes > 0 && elapsedMs > 0
      ? (entry.bytes * 1000) / elapsedMs
      : 0;
  const pct =
    entry.totalBytes && entry.totalBytes > 0 && entry.bytes
      ? Math.min(100, (entry.bytes / entry.totalBytes) * 100)
      : null;

  const borderClass =
    entry.outcome === "running"
      ? "border-[var(--color-accent)]"
      : entry.outcome === "done"
        ? "border-[var(--color-good)]"
        : entry.outcome === "failed"
          ? "border-[var(--color-bad)]"
          : "border-[var(--color-warn)]";

  // Stop dispatch — pick the appropriate cancel mechanism based on
  // entry.kind. For ops with an op_id stored on the entry (Library
  // moves, FS pastes), call the payload's FS_OP_CANCEL directly via
  // the addr we stashed at start time; that's the only path that
  // actually interrupts the in-flight RPC. For job-based ops
  // (uploads, downloads, FS bulk delete), dispatch to the relevant
  // store's cancel/reset action — those stop *watching* the engine
  // job (engine-side cancel API for transfer jobs is future work).
  const handleStop = async () => {
    if (entry.opId !== undefined && entry.addr) {
      try {
        await fsOpCancel(entry.addr, entry.opId);
      } catch {
        // Best-effort. The local store-side cancel below will still
        // fire if applicable.
      }
    }
    if (
      entry.kind === "fs-delete" ||
      entry.kind === "fs-paste-copy" ||
      entry.kind === "fs-paste-move"
    ) {
      useFsBulkOpStore.getState().requestCancel();
    } else if (entry.kind === "download") {
      useFsDownloadOpStore.getState().requestStop();
    } else if (
      entry.kind === "upload" ||
      entry.kind === "upload-dir" ||
      entry.kind === "upload-reconcile"
    ) {
      useTransferStore.getState().reset();
    }
    // library-* ops are component-local; the fsOpCancel call above
    // is the only useful action — the screen's poller will see the
    // cancelled error from fsCopy and clean up its own state.
  };

  return (
    <li
      className={`rounded-md border bg-[var(--color-surface-2)] p-3 text-xs ${borderClass}`}
    >
      <div className="mb-1 flex items-center gap-2">
        <OutcomeIcon outcome={entry.outcome} />
        <span className="font-medium">{entry.label}</span>
        {entry.files !== undefined && entry.files > 1 && (
          <span className="rounded-full bg-[var(--color-surface-3)] px-1.5 py-0.5 text-[10px] text-[var(--color-muted)]">
            {tr(
              "activity_files_badge",
              { count: entry.files },
              `${entry.files} files`,
            )}
          </span>
        )}
        <span className="ml-auto text-[10px] text-[var(--color-muted)]">
          {formatRelative(entry.startedAtMs, tr)} · {formatDuration(elapsedMs / 1000)}
        </span>
        {isRunning && (
          <button
            type="button"
            onClick={() => void handleStop()}
            className="rounded-md border border-[var(--color-border)] px-2 py-0.5 text-[10px] hover:bg-[var(--color-surface-3)]"
            title={tr(
              "activity_stop_tooltip",
              undefined,
              entry.opId !== undefined
                ? "Cancel the in-flight operation"
                : "Stop watching this operation (engine job may continue server-side)",
            )}
          >
            {tr("fs_download_stop", undefined, "Stop")}
          </button>
        )}
      </div>

      {/* From / To paths on their own lines for readability — game
          paths are long and a single break-all line wraps awkwardly
          at the end. Falls back to the legacy single `detail` line
          for entries created before fromPath/toPath were tracked. */}
      {(entry.fromPath || entry.toPath) ? (
        <div className="mb-1 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 font-mono text-[11px] text-[var(--color-muted)]">
          {entry.fromPath && (
            <>
              <span className="text-[var(--color-muted)]">
                {tr("activity_from", undefined, "From")}
              </span>
              <span className="break-all">{entry.fromPath}</span>
            </>
          )}
          {entry.toPath && (
            <>
              <span className="text-[var(--color-muted)]">
                {tr("activity_to", undefined, "To")}
              </span>
              <span className="break-all">{entry.toPath}</span>
            </>
          )}
        </div>
      ) : entry.detail ? (
        <div className="mb-1 break-all font-mono text-[11px] text-[var(--color-muted)]">
          {entry.detail}
        </div>
      ) : null}

      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-0.5 text-[11px] text-[var(--color-muted)]">
        {entry.bytes !== undefined && entry.bytes > 0 && (
          <span>
            {formatBytes(entry.bytes)}
            {entry.totalBytes !== undefined && entry.totalBytes > 0 && (
              <> / {formatBytes(entry.totalBytes)}</>
            )}
            {pct !== null && ` (${pct.toFixed(0)}%)`}
          </span>
        )}
        {speed > 0 && isRunning && <span>{formatBytes(speed)}/s</span>}
        {!isRunning && entry.bytes !== undefined && entry.bytes > 0 && speed > 0 && (
          <span>
            {tr(
              "activity_avg_speed",
              undefined,
              `avg ${formatBytes(speed)}/s`,
            )}
          </span>
        )}
        <span>
          {entry.outcome === "running"
            ? tr("activity_outcome_running", undefined, "Running")
            : entry.outcome === "done"
              ? tr("activity_outcome_done", undefined, "Done")
              : entry.outcome === "failed"
                ? tr("activity_outcome_failed", undefined, "Failed")
                : tr("activity_outcome_stopped", undefined, "Stopped")}
        </span>
      </div>
      {entry.error && (
        // For terminal entries, the error is a failure detail (red).
        // For still-running entries, treat it as an informational
        // hint about why progress data isn't appearing — yellow,
        // not red. The Library move's poller writes the
        // "unsupported_frame" warning into this field while the op
        // is in flight; without distinct styling the user sees a
        // red error on a row that's still happily running, which
        // is more confusing than helpful.
        <div
          className={`mt-1 text-[11px] ${
            isRunning
              ? "text-[var(--color-warn)]"
              : "text-[var(--color-bad)]"
          }`}
        >
          {entry.error}
        </div>
      )}
    </li>
  );
}

function OutcomeIcon({ outcome }: { outcome: ActivityOutcome }) {
  if (outcome === "running")
    return <Loader2 size={13} className="animate-spin text-[var(--color-accent)]" />;
  if (outcome === "done")
    return <CheckCircle2 size={13} className="text-[var(--color-good)]" />;
  if (outcome === "failed")
    return <XCircle size={13} className="text-[var(--color-bad)]" />;
  return <StopCircle size={13} className="text-[var(--color-warn)]" />;
}

/**
 * Localized relative time. Caller passes the active `tr` since this
 * helper is invoked from the row body — JSX render path, no context
 * to thread. Day-or-older entries fall through to the locale's
 * `toLocaleString` which the browser already adapts to the user's
 * preferred language.
 */
function formatRelative(
  ms: number,
  tr: (
    key: string,
    vars?: Record<string, string | number>,
    fallback?: string,
  ) => string,
): string {
  const diff = Date.now() - ms;
  if (diff < 60_000) return tr("activity_just_now", undefined, "just now");
  if (diff < 3_600_000) {
    const m = Math.floor(diff / 60_000);
    return tr("activity_minutes_ago", { count: m }, `${m}m ago`);
  }
  if (diff < 86_400_000) {
    const h = Math.floor(diff / 3_600_000);
    return tr("activity_hours_ago", { count: h }, `${h}h ago`);
  }
  const d = new Date(ms);
  return d.toLocaleString();
}
