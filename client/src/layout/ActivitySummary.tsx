// The activity strip's summary line and the panel above it. Pure views over an ActivitySummary:
// the connected bar (ActivityBar / the StatusBar slot) feeds them.

import { AlertTriangle, Check, ChevronDown, ChevronUp, X } from "lucide-react";

import { KindIcon } from "../components/TaskList";
import { Spinner } from "../components/Spinner";
import { formatBytes, formatDuration } from "../lib/format";
import type { ActivitySummary, FinishedRow, ActivityRow } from "../state/activitySummary";
import { routeForTask } from "../state/activitySummary";
import { useTr } from "../state/lang";
import type { Task } from "../state/tasks";

const pctText = (pct: number | null) => (pct == null ? "" : ` ${Math.floor(pct)}%`);

export function ActivitySummaryLine({
  summary,
  open,
  onToggle,
}: {
  summary: ActivitySummary;
  open: boolean;
  onToggle: () => void;
}) {
  const tr = useTr();
  const count = summary.running.length;
  let body;
  if (count > 0) {
    body = (
      <>
        <Spinner size={11} tone="accent" />
        <span className="font-medium text-[var(--color-text)]">
          {tr("activity_running_count", { count }, `${count} running`)}
        </span>
        <span className="truncate">
          {summary.headline.map((h, i) => (
            <span key={i}>
              {" · "}
              {h.short}
              {pctText(h.pct)}
            </span>
          ))}
          {summary.more > 0 &&
            ` · ${tr("activity_more", { count: summary.more }, `+${summary.more} more`)}`}
        </span>
      </>
    );
  } else if (summary.flash) {
    const { label, outcome } = summary.flash;
    body =
      outcome === "done" ? (
        <>
          <Check size={11} className="text-[var(--color-good)]" aria-hidden />
          <span className="truncate">{tr("activity_flash_done", { label }, `${label} finished`)}</span>
        </>
      ) : (
        <>
          <X size={11} className="text-[var(--color-bad)]" aria-hidden />
          <span className="truncate">{tr("activity_flash_failed", { label }, `${label} failed`)}</span>
        </>
      );
  } else {
    body = <span>{tr("activity_nothing_running", undefined, "Nothing running")}</span>;
  }
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-expanded={open}
      aria-label={tr("activity_toggle", undefined, "Show activity")}
      className="flex min-w-0 max-w-[28rem] items-center gap-1.5 rounded px-1 hover:text-[var(--color-text)]"
    >
      {body}
      {summary.failedUnseen > 0 && (
        <span className="shrink-0 rounded-full bg-[var(--color-bad)] px-1.5 text-[0.625rem] font-medium text-white">
          {tr("activity_failed_badge", { count: summary.failedUnseen }, `${summary.failedUnseen} failed`)}
        </span>
      )}
      {open ? <ChevronDown size={11} aria-hidden /> : <ChevronUp size={11} aria-hidden />}
    </button>
  );
}

export interface ActivityPanelProps {
  summary: ActivitySummary;
  now: number;
  /** Open a screen: a job's own, or the Activity screen. */
  onOpen: (route: string) => void;
  onCancel: (task: Task) => void;
  onRetry: (task: Task) => void;
  canCancel: (task: Task) => boolean;
  canRetry: (task: Task) => boolean;
  /** The console a job targets, when there is more than one to tell apart. */
  consoleOf?: (task: Task) => string | null;
}

export function ActivityPanelView(props: ActivityPanelProps) {
  const tr = useTr();
  const { summary, onOpen } = props;
  return (
    <div className="max-h-[50vh] space-y-3 overflow-y-auto border-t border-[var(--color-border)] bg-[var(--color-surface-2)] px-4 py-3 text-xs">
      {summary.running.length === 0 ? (
        <p className="text-[var(--color-muted)]">
          {tr("activity_nothing_running", undefined, "Nothing running")}
        </p>
      ) : (
        <ul className="space-y-2">
          {summary.running.map((row) => (
            <RunningRow key={row.id} row={row} {...props} />
          ))}
        </ul>
      )}
      {summary.finished.length > 0 && (
        <div>
          <div className="mb-1 font-medium text-[var(--color-muted)]">
            {tr("activity_just_finished", undefined, "Just finished")}
          </div>
          <ul className="space-y-1">
            {summary.finished.map((row) => (
              <FinishedRowView key={row.id} row={row} {...props} />
            ))}
          </ul>
        </div>
      )}
      <button
        type="button"
        onClick={() => onOpen("/activity")}
        className="text-[var(--color-accent)] hover:underline"
      >
        {tr("activity_see_all", undefined, "See all activity")}
      </button>
    </div>
  );
}

function RunningRow({ row, onOpen, onCancel, canCancel, consoleOf }: { row: ActivityRow } & ActivityPanelProps) {
  const tr = useTr();
  const { task, pct, staleMin } = row;
  const where = consoleOf?.(task);
  const figures: string[] = [];
  if (staleMin != null) {
    figures.push(tr("activity_stale", { min: staleMin }, `No update for ${staleMin} min`));
  } else {
    if (pct != null) figures.push(`${Math.floor(pct)}%`);
    if (task.rate && task.rate.bytesPerSec > 0) figures.push(`${formatBytes(task.rate.bytesPerSec)}/s`);
    if (task.eta != null && task.eta > 0) {
      const time = formatDuration(task.eta);
      figures.push(tr("activity_left", { time }, `${time} left`));
    }
  }
  return (
    <li>
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={() => onOpen(routeForTask(task))}
          className="flex min-w-0 flex-1 items-center gap-2 text-left hover:text-[var(--color-accent)]"
        >
          <KindIcon kind={task.kind} size={13} />
          <span className="truncate font-medium text-[var(--color-text)]">{task.label}</span>
          {where && <span className="shrink-0 text-[var(--color-muted)]">→ {where}</span>}
        </button>
        <span className={`shrink-0 ${staleMin != null ? "text-[var(--color-warn)]" : "text-[var(--color-muted)]"}`}>
          {[task.stage, ...figures].filter(Boolean).join(" · ")}
        </span>
        {canCancel(task) && (
          <button
            type="button"
            onClick={() => onCancel(task)}
            className="shrink-0 rounded border border-[var(--color-border)] px-1.5 py-0.5 hover:bg-[var(--color-surface-3)]"
          >
            {tr("activity_cancel", undefined, "Cancel")}
          </button>
        )}
      </div>
      <div className="mt-1 h-1 overflow-hidden rounded bg-[var(--color-surface-3)]">
        {pct != null ? (
          <div className="h-1 rounded bg-[var(--color-accent)]" style={{ width: `${Math.min(100, pct)}%` }} />
        ) : (
          <div className="h-1 w-1/3 animate-pulse rounded bg-[var(--color-accent)] opacity-60" />
        )}
      </div>
    </li>
  );
}

function agoText(ms: number, tr: ReturnType<typeof useTr>): string {
  const min = Math.floor(ms / 60_000);
  if (min < 1) return tr("activity_ago_now", undefined, "just now");
  if (min < 60) return tr("activity_ago_min", { min }, `${min} min ago`);
  const hours = Math.floor(min / 60);
  return tr("activity_ago_hours", { hours }, `${hours} h ago`);
}

function FinishedRowView({ row, onOpen, onRetry, canRetry }: { row: FinishedRow } & ActivityPanelProps) {
  const tr = useTr();
  const { task, outcome } = row;
  const icon =
    outcome === "done" ? (
      <Check size={12} className="shrink-0 text-[var(--color-good)]" aria-label={tr("activity_outcome_done", undefined, "Done")} />
    ) : outcome === "failed" ? (
      <X size={12} className="shrink-0 text-[var(--color-bad)]" aria-label={tr("activity_outcome_failed", undefined, "Failed")} />
    ) : (
      <AlertTriangle size={12} className="shrink-0 text-[var(--color-warn)]" aria-label={tr("activity_outcome_other", undefined, "Did not finish")} />
    );
  return (
    <li className="flex items-center gap-2">
      {icon}
      <button
        type="button"
        onClick={() => onOpen(routeForTask(task))}
        className="min-w-0 truncate text-left text-[var(--color-text)] hover:text-[var(--color-accent)]"
      >
        {task.label}
      </button>
      <span className="shrink-0 text-[var(--color-muted)]">{agoText(row.agoMs, tr)}</span>
      {outcome === "failed" && task.lastError?.message && (
        <span className="min-w-0 flex-1 truncate text-[var(--color-bad)]" title={task.lastError.message}>
          {task.lastError.message}
        </span>
      )}
      {canRetry(task) && (
        <button
          type="button"
          onClick={() => onRetry(task)}
          className="ms-auto shrink-0 rounded border border-[var(--color-border)] px-1.5 py-0.5 hover:bg-[var(--color-surface-3)]"
        >
          {tr("activity_retry", undefined, "Retry")}
        </button>
      )}
    </li>
  );
}
