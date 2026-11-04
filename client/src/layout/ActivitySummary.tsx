// The activity strip's summary line and the panel it opens. Pure views over an ActivitySummary:
// the connected bar (ActivityBar / the StatusBar slot / the phone pill) feeds them.

import { AlertTriangle, Check, ChevronDown, ChevronUp, Clock, X } from "lucide-react";

import { humanizeJobErrorReason } from "../api/ps5";
import { KindIcon } from "../components/TaskList";
import { ProgressBar } from "../components/ProgressBar";
import { Spinner } from "../components/Spinner";
import { formatBytes } from "../lib/format";
import type { ActivitySummary, FinishedRow, ActivityRow, QueuedRow } from "../state/activitySummary";
import { routeForTask, shortLabel, shortLabelKey } from "../state/activitySummary";
import { useTr } from "../state/lang";
import type { Task, TaskKind } from "../state/tasks";
import { formatDuration } from "../lib/formatDate";

type Tr = ReturnType<typeof useTr>;

const kindName = (tr: Tr, kind: TaskKind) => tr(shortLabelKey(kind), undefined, shortLabel(kind));

/** A time left worth showing: positive, finite and under 30 days (anything longer comes from a
 *  speed too small to mean anything). */
export function shownEta(eta: number | undefined | null): eta is number {
  return eta != null && eta > 0 && Number.isFinite(eta) && eta < 30 * 86_400;
}

/** What a running job says about itself, in order: its step, how far, how fast, how long. A
 *  job that has gone quiet says only that. `long` adds the byte counts (the panel has room). */
export function rowFigures(row: ActivityRow, tr: Tr, long = false): string[] {
  const { task, pct, staleMin } = row;
  if (staleMin != null) return [tr("activity_stale", { min: staleMin }, `No update for ${staleMin} min`)];
  const out: string[] = [];
  if (task.status === "queued") out.push(tr("activity_status_queued", undefined, "Waiting to start"));
  else if (task.status === "paused") out.push(tr("activity_status_paused", undefined, "Paused"));
  else if (task.status === "awaiting")
    out.push(tr("activity_status_awaiting", undefined, "Waiting for the console"));
  if (task.stage) out.push(task.stage);
  if (pct != null) out.push(`${Math.floor(pct)}%`);
  const p = task.progress;
  if (long && p && p.total > 0) {
    if (p.unit === "bytes") {
      const cur = formatBytes(p.current);
      const total = formatBytes(p.total);
      out.push(tr("activity_bytes_of", { cur, total }, `${cur} of ${total}`));
    } else {
      out.push(
        tr("activity_items_of", { cur: p.current, total: p.total }, `${p.current} of ${p.total}`),
      );
    }
  }
  if (task.rate && task.rate.bytesPerSec >= 1) out.push(`${formatBytes(task.rate.bytesPerSec)}/s`);
  if (shownEta(task.eta)) {
    const time = formatDuration(task.eta);
    out.push(tr("activity_left", { time }, `${time} left`));
  }
  return out;
}

/** The strip's short version: how far, how fast, how long; the step only when it says more than
 *  a percentage would (no percentage, or all bytes sent and the console finishing). */
export function lineFigures(row: ActivityRow, tr: Tr): string[] {
  const { task, pct, staleMin } = row;
  if (staleMin != null) return [tr("activity_stale", { min: staleMin }, `No update for ${staleMin} min`)];
  const out: string[] = [];
  if (pct != null) out.push(`${Math.floor(pct)}%`);
  if (task.stage && (pct == null || pct >= 100)) out.push(task.stage);
  if (task.rate && task.rate.bytesPerSec >= 1 && (pct == null || pct < 100))
    out.push(`${formatBytes(task.rate.bytesPerSec)}/s`);
  if (shownEta(task.eta)) {
    const time = formatDuration(task.eta);
    out.push(tr("activity_left", { time }, `${time} left`));
  }
  return out;
}

/** A slim bar: the share done, or a pulse when the job does not say. */
function ProgressLine({ pct, stale, className = "" }: { pct: number | null; stale?: boolean; className?: string }) {
  const tone = stale ? "bg-[var(--color-warn)]" : "bg-[var(--color-accent)]";
  return (
    <span
      aria-hidden
      className={`block overflow-hidden rounded-full bg-[var(--color-surface-3)] ${className}`}
    >
      {pct != null ? (
        <span
          className={`block h-full rounded-full ${tone} transition-[width] duration-500`}
          style={{ width: `${Math.max(2, Math.min(100, pct))}%` }}
        />
      ) : (
        <span className="anim-skeleton block h-full w-full" />
      )}
    </span>
  );
}

/** Whether the line has anything to say beyond "nothing running" (the phone pill hides then). */
export function summaryHasNews(summary: ActivitySummary): boolean {
  return (
    summary.running.length > 0 ||
    summary.queued.length > 0 ||
    summary.flash != null ||
    summary.failedUnseen > 0
  );
}

export function ActivitySummaryLine({
  summary,
  open,
  onToggle,
  variant = "strip",
}: {
  summary: ActivitySummary;
  open: boolean;
  onToggle: () => void;
  /** `strip`: the desktop status bar. `pill`: the phone's floating pill (fills its width). */
  variant?: "strip" | "pill";
}) {
  const tr = useTr();
  const primary = summary.primary;
  let body;
  let spoken: string;
  if (primary) {
    const all = lineFigures(primary, tr);
    // The phone pill has room for how far and how long only.
    const figures = variant === "pill" && all.length > 2 ? [all[0], all[all.length - 1]] : all;
    const name = kindName(tr, primary.task.kind);
    spoken = [name, primary.task.label, ...rowFigures(primary, tr, true)].join(", ");
    body = (
      <>
        {primary.staleMin != null ? (
          <AlertTriangle size={11} className="shrink-0 text-[var(--color-warn)]" aria-hidden />
        ) : (
          <Spinner size={11} tone="accent" className="shrink-0" />
        )}
        <span className="shrink-0 font-medium text-[var(--color-text)]">{name}</span>
        <span className="min-w-[2.5rem] truncate text-[var(--color-text)]" data-testid="activity-primary-label">
          {primary.task.label}
        </span>
        {figures.length > 0 && (
          <span
            className={`shrink-0 tabular-nums ${primary.staleMin != null ? "text-[var(--color-warn)]" : ""}`}
            data-testid="activity-primary-figures"
          >
            {figures.join(" · ")}
          </span>
        )}
        {summary.others > 0 && (
          <span
            className="shrink-0 rounded-full border border-[var(--color-border-strong)] px-1.5 text-[0.625rem] tabular-nums"
            title={tr("activity_others", { count: summary.others }, `${summary.others} more`)}
          >
            {tr("activity_others_short", { count: summary.others }, `+${summary.others}`)}
          </span>
        )}
        <ProgressLine
          pct={primary.pct}
          stale={primary.staleMin != null}
          className={
            variant === "pill"
              ? "absolute inset-x-4 bottom-1 h-[3px]"
              : "absolute inset-x-2 -bottom-0.5 h-[2px]"
          }
        />
      </>
    );
  } else if (summary.queued.length > 0) {
    const count = summary.queued.length;
    spoken = tr("activity_waiting_count", { count }, `${count} waiting`);
    body = (
      <>
        <Clock size={11} className="shrink-0" aria-hidden />
        <span className="truncate">{spoken}</span>
      </>
    );
  } else if (summary.flash) {
    const { label, outcome } = summary.flash;
    spoken =
      outcome === "done"
        ? tr("activity_flash_done", { label }, `${label} finished`)
        : tr("activity_flash_failed", { label }, `${label} failed`);
    body = (
      <>
        {outcome === "done" ? (
          <Check size={11} className="shrink-0 text-[var(--color-good)]" aria-hidden />
        ) : (
          <X size={11} className="shrink-0 text-[var(--color-bad)]" aria-hidden />
        )}
        <span className="truncate">{spoken}</span>
      </>
    );
  } else {
    spoken = tr("activity_nothing_running", undefined, "Nothing running");
    body = <span className="truncate opacity-80">{spoken}</span>;
  }
  const label = `${tr("activity_toggle", undefined, "Show activity")}: ${spoken}`;
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-expanded={open}
      aria-controls={open ? ACTIVITY_PANEL_ID : undefined}
      data-activity-toggle
      aria-label={label}
      title={spoken}
      className={
        variant === "pill"
          ? "relative flex h-10 w-full min-w-0 items-center gap-1.5 rounded-full px-4 text-xs text-[var(--color-muted)]"
          : "relative flex min-w-0 max-w-[34rem] items-center gap-1.5 rounded-full px-2 py-0.5 hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
      }
    >
      {body}
      {summary.failedUnseen > 0 && (
        <span
          className="shrink-0 rounded-full bg-[var(--color-bad)] px-1.5 text-[0.625rem] font-medium text-[var(--color-accent-contrast)]"
          title={tr("activity_failed_badge", { count: summary.failedUnseen }, `${summary.failedUnseen} failed`)}
        >
          {/* The pill is narrow while a job runs: the count alone, the words in its title. */}
          {variant === "pill" && primary
            ? `${summary.failedUnseen} ✕`
            : tr("activity_failed_badge", { count: summary.failedUnseen }, `${summary.failedUnseen} failed`)}
        </span>
      )}
      {open ? (
        <ChevronDown size={11} className="ms-auto shrink-0" aria-hidden />
      ) : (
        <ChevronUp size={11} className="ms-auto shrink-0" aria-hidden />
      )}
    </button>
  );
}

/** The open panel's id: the summary line points `aria-controls` at it. */
export const ACTIVITY_PANEL_ID = "activity-panel";

export interface ActivityPanelProps {
  summary: ActivitySummary;
  now: number;
  /** Open a screen: a job's own, or the Activity screen. */
  onOpen: (route: string) => void;
  onCancel: (task: Task) => void;
  onRetry: (task: Task) => void;
  canCancel: (task: Task) => boolean;
  canRetry: (task: Task) => boolean;
  /** Take queued work out of its queue before it starts. Absent: no Remove buttons. */
  onRemoveQueued?: (row: QueuedRow) => void;
  /** Close the panel (its own close button). */
  onClose?: () => void;
  /** The console a job targets, when there is more than one to tell apart. */
  consoleOf?: (task: Pick<Task, "consoleId" | "consoleKey">) => string | null;
}

function SectionTitle({ children }: { children: React.ReactNode }) {
  return (
    <h3 className="mb-1.5 text-[0.6875rem] font-semibold uppercase tracking-wide text-[var(--color-muted)]">
      {children}
    </h3>
  );
}

export function ActivityPanelView(props: ActivityPanelProps) {
  const tr = useTr();
  const { summary, onOpen, onClose } = props;
  const idle = summary.running.length === 0 && summary.queued.length === 0;
  return (
    <div
      id={ACTIVITY_PANEL_ID}
      role="region"
      aria-label={tr("activity_panel_title", undefined, "Activity")}
      // Focusable by script: opening the panel moves focus here (ActivityBar),
      // so the next Tab lands on its first control.
      tabIndex={-1}
      className="space-y-4 text-xs outline-none"
    >
      <div className="flex items-center gap-2">
        <h2 className="text-sm font-semibold text-[var(--color-text)]">
          {tr("activity_panel_title", undefined, "Activity")}
        </h2>
        {!idle && (
          <span className="text-[var(--color-muted)]">
            {[
              summary.running.length > 0 &&
                tr("activity_running_count", { count: summary.running.length }, `${summary.running.length} running`),
              summary.queued.length > 0 &&
                tr("activity_waiting_count", { count: summary.queued.length }, `${summary.queued.length} waiting`),
            ]
              .filter(Boolean)
              .join(" · ")}
          </span>
        )}
        <button
          type="button"
          onClick={() => onOpen("/tasks")}
          className="ms-auto shrink-0 text-[var(--color-accent)] hover:underline"
        >
          {tr("activity_see_all", undefined, "See all activity")}
        </button>
        {onClose && (
          <button
            type="button"
            onClick={onClose}
            aria-label={tr("activity_close", undefined, "Close")}
            className="shrink-0 rounded-full p-1 hover:bg-[var(--color-surface-3)]"
          >
            <X size={14} aria-hidden />
          </button>
        )}
      </div>

      {idle ? (
        <p className="text-[var(--color-muted)]">
          {tr("activity_nothing_running", undefined, "Nothing running")}
        </p>
      ) : null}

      {summary.running.length > 0 && (
        <section>
          <SectionTitle>{tr("activity_section_running", undefined, "Running")}</SectionTitle>
          <ul className="space-y-3">
            {summary.running.map((row) => (
              <RunningRow key={row.id} row={row} {...props} />
            ))}
          </ul>
        </section>
      )}

      {summary.queued.length > 0 && (
        <section>
          <SectionTitle>{tr("activity_section_waiting", undefined, "Waiting")}</SectionTitle>
          <ul className="space-y-1.5">
            {summary.queued.map((row, i) => (
              <QueuedRowView key={`${row.owner}:${row.id}`} row={row} position={i + 1} {...props} />
            ))}
          </ul>
        </section>
      )}

      {summary.finished.length > 0 && (
        <section>
          <SectionTitle>{tr("activity_just_finished", undefined, "Just finished")}</SectionTitle>
          <ul className="space-y-1.5">
            {summary.finished.map((row) => (
              <FinishedRowView key={row.id} row={row} {...props} />
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}

function RunningRow({ row, onOpen, onCancel, canCancel, consoleOf }: { row: ActivityRow } & ActivityPanelProps) {
  const tr = useTr();
  const { task, pct, staleMin } = row;
  const where = consoleOf?.(task);
  const figures = rowFigures(row, tr, true);
  return (
    <li data-testid="activity-running-row">
      <div className="flex min-w-0 items-center gap-2">
        <button
          type="button"
          onClick={() => onOpen(routeForTask(task))}
          title={tr("activity_open_owner", undefined, "Open its screen")}
          className="flex min-w-0 flex-1 items-center gap-2 text-start hover:text-[var(--color-accent)]"
        >
          <KindIcon kind={task.kind} size={13} />
          <span className="shrink-0 text-[var(--color-muted)]">{kindName(tr, task.kind)}</span>
          <span className="min-w-0 truncate font-medium text-[var(--color-text)]">{task.label}</span>
          {where && <span className="shrink-0 text-[var(--color-muted)]">→ {where}</span>}
        </button>
        {canCancel(task) && (
          <button
            type="button"
            onClick={() => onCancel(task)}
            className="shrink-0 rounded-full border border-[var(--color-border)] px-2 py-0.5 hover:bg-[var(--color-surface-3)]"
          >
            {tr("activity_cancel", undefined, "Cancel")}
          </button>
        )}
      </div>
      {figures.length > 0 && (
        <div
          className={`mt-0.5 break-words ps-[1.3rem] tabular-nums ${staleMin != null ? "text-[var(--color-warn)]" : "text-[var(--color-muted)]"}`}
        >
          {figures.join(" · ")}
        </div>
      )}
      {task.detail && (
        <div className="truncate ps-[1.3rem] text-[var(--color-muted)] opacity-80" title={task.detail}>
          {task.detail}
        </div>
      )}
      <ProgressBar
        className="mt-1.5"
        size="sm"
        value={pct == null ? null : pct / 100}
        tone={staleMin != null ? "warn" : "accent"}
        label={task.label}
      />
    </li>
  );
}

function QueuedRowView({
  row,
  position,
  onOpen,
  onRemoveQueued,
  consoleOf,
}: { row: QueuedRow; position: number } & ActivityPanelProps) {
  const tr = useTr();
  const where = row.consoleId ? consoleOf?.({ consoleId: row.consoleId }) : null;
  return (
    <li className="flex min-w-0 items-center gap-2" data-testid="activity-queued-row">
      <span className="w-4 shrink-0 text-end tabular-nums text-[var(--color-muted)]">{position}</span>
      <button
        type="button"
        onClick={() => onOpen(row.route)}
        title={tr("activity_open_owner", undefined, "Open its screen")}
        className="flex min-w-0 flex-1 items-center gap-2 text-start hover:text-[var(--color-accent)]"
      >
        <KindIcon kind={row.kind} size={13} />
        <span className="shrink-0 text-[var(--color-muted)]">{kindName(tr, row.kind)}</span>
        <span className="min-w-0 truncate text-[var(--color-text)]">{row.label}</span>
        {where && <span className="shrink-0 text-[var(--color-muted)]">→ {where}</span>}
      </button>
      {row.sizeBytes != null && (
        <span className="shrink-0 tabular-nums text-[var(--color-muted)]">{formatBytes(row.sizeBytes)}</span>
      )}
      {onRemoveQueued && (
        <button
          type="button"
          onClick={() => onRemoveQueued(row)}
          className="shrink-0 rounded-full border border-[var(--color-border)] px-2 py-0.5 hover:bg-[var(--color-surface-3)]"
        >
          {tr("activity_remove", undefined, "Remove")}
        </button>
      )}
    </li>
  );
}

function agoText(ms: number, tr: Tr): string {
  const min = Math.floor(ms / 60_000);
  if (min < 1) return tr("activity_ago_now", undefined, "just now");
  if (min < 60) return tr("activity_ago_min", { min }, `${min} min ago`);
  const hours = Math.floor(min / 60);
  return tr("activity_ago_hours", { hours }, `${hours} h ago`);
}

/** A failure in words: an engine token (`ava1_unreachable`) said the way the job screens say
 *  it; any other text as it is. */
export function failureText(message: string): string {
  return (/^[a-z0-9_]+$/.test(message.trim()) && humanizeJobErrorReason(message.trim())) || message;
}

function FinishedRowView({ row, onOpen, onRetry, canRetry, consoleOf }: { row: FinishedRow } & ActivityPanelProps) {
  const tr = useTr();
  const { task, outcome } = row;
  const where = consoleOf?.(task);
  const icon =
    outcome === "done" ? (
      <Check size={12} className="shrink-0 text-[var(--color-good)]" aria-label={tr("activity_outcome_done", undefined, "Done")} />
    ) : outcome === "failed" ? (
      <X size={12} className="shrink-0 text-[var(--color-bad)]" aria-label={tr("activity_outcome_failed", undefined, "Failed")} />
    ) : (
      <AlertTriangle size={12} className="shrink-0 text-[var(--color-warn)]" aria-label={tr("activity_outcome_other", undefined, "Did not finish")} />
    );
  const error = outcome === "failed" && task.lastError?.message ? failureText(task.lastError.message) : null;
  return (
    <li className="min-w-0">
      <div className="flex min-w-0 items-center gap-2">
        {icon}
        <button
          type="button"
          onClick={() => onOpen(routeForTask(task))}
          title={tr("activity_open_owner", undefined, "Open its screen")}
          className="flex min-w-0 flex-1 items-center gap-2 text-start hover:text-[var(--color-accent)]"
        >
          <span className="shrink-0 text-[var(--color-muted)]">{kindName(tr, task.kind)}</span>
          <span className="min-w-0 truncate text-[var(--color-text)]">{task.label}</span>
          {where && <span className="shrink-0 text-[var(--color-muted)]">→ {where}</span>}
        </button>
        <span className="shrink-0 text-[var(--color-muted)]">{agoText(row.agoMs, tr)}</span>
        {canRetry(task) && (
          <button
            type="button"
            onClick={() => onRetry(task)}
            className="shrink-0 rounded-full border border-[var(--color-border)] px-2 py-0.5 hover:bg-[var(--color-surface-3)]"
          >
            {tr("activity_retry", undefined, "Retry")}
          </button>
        )}
      </div>
      {error && (
        <div className="truncate ps-5 text-[var(--color-bad)]" title={error}>
          {error}
        </div>
      )}
    </li>
  );
}
