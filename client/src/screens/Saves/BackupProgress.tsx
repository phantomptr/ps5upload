import { useCallback, useEffect, useRef, useState } from "react";
import { Activity, Play, RotateCcw, Square } from "lucide-react";

import { Button, Modal, ProgressBar, SegmentedControl } from "../../components";
import {
  saveBackupRunsApi,
  type BackupPlace,
  type BackupRunSummary,
  type BackupRunsView,
  type BackupStep,
  type BackupWhich,
} from "../../api/saveBackup";
import { formatBytes } from "../../lib/format";
import { formatDate, formatDuration } from "../../lib/formatDate";
import {
  overall,
  pressOnce,
  retryable,
  rows,
  shouldPoll,
  SpeedMeter,
  stoppedNote,
  type Row,
  type RowFilter,
  type StoppedNote,
} from "../../lib/backupProgress";
import { useTr } from "../../state/lang";
import { useSaveGameName } from "./gameNames";
import { userHex } from "./saveUsers";

type Tr = ReturnType<typeof useTr>;
type Owners = ReadonlyArray<{ id: number; name: string | null }>;

/** How often a run being shown is asked about while it runs. */
const POLL_MS = 2000;

function usePageVisible(): boolean {
  const [visible, setVisible] = useState(
    () =>
      typeof document === "undefined" || document.visibilityState !== "hidden",
  );
  useEffect(() => {
    const on = () => setVisible(document.visibilityState !== "hidden");
    document.addEventListener("visibilitychange", on);
    return () => document.removeEventListener("visibilitychange", on);
  }, []);
  return visible;
}

/** The engine's record of `which`'s runs. Asked once (and again when `nudge` changes); while
 *  `follow` is on and a run runs, every few seconds while the page is shown. `supported` is false
 *  for an engine without runs (before 12.6.20). */
export function useBackupRuns(
  which: BackupWhich,
  follow: boolean,
  nudge?: unknown,
) {
  const [view, setView] = useState<BackupRunsView | null>(null);
  const [supported, setSupported] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const visible = usePageVisible();
  const live = useRef(true);
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);
  const refresh = useCallback(async () => {
    try {
      const v = await saveBackupRunsApi.view(which);
      if (!live.current) return;
      setView(v);
      setSupported(true);
      setError(null);
    } catch (e) {
      if (!live.current) return;
      const msg = e instanceof Error ? e.message : String(e);
      // An older engine has no such route.
      if (/HTTP 404|not found/i.test(msg)) setSupported(false);
      else setError(msg);
    }
  }, [which]);
  useEffect(() => {
    void refresh();
  }, [refresh, nudge]);
  const polling = follow && shouldPoll(view, visible);
  useEffect(() => {
    if (!polling) return;
    const id = window.setInterval(() => void refresh(), POLL_MS);
    return () => window.clearInterval(id);
  }, [polling, refresh]);
  const cancel = useCallback(async () => {
    try {
      await saveBackupRunsApi.cancel(which);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
    await refresh();
  }, [which, refresh]);
  // One press at a time, until the run it started shows (a second press got "a backup is running").
  const pressing = useRef(false);
  const again = useCallback(
    (ids?: string[]) =>
      pressOnce(pressing, async () => {
        try {
          setView(await saveBackupRunsApi.again(which, ids));
          setError(null);
        } catch (e) {
          setError(e instanceof Error ? e.message : String(e));
        }
        // The run starts in the background: ask again shortly to show it running.
        await new Promise((r) => window.setTimeout(r, 700));
        await refresh();
      }),
    [which, refresh],
  );
  return { view, supported, error, refresh, cancel, again };
}

export function placeLabel(p: BackupPlace, tr: Tr): string {
  if (p === "share")
    return tr("backup_place_share", undefined, "Network share");
  if (p === "drive") return tr("backup_place_drive", undefined, "PS5 drive");
  return tr("backup_place_local", undefined, "This computer");
}

export function stepLabel(
  step: BackupStep,
  pct: number | null,
  tr: Tr,
): string {
  switch (step) {
    case "reading":
      return tr("backup_step_reading", undefined, "Reading from the PS5");
    case "copying":
      return pct === null
        ? tr("backup_step_copying_plain", undefined, "Copying")
        : tr("backup_step_copying", { pct }, "Copying {pct}%");
    case "checking":
      return tr("backup_step_checking", undefined, "Checking");
    case "verified":
      return tr("backup_step_verified", undefined, "Verified");
    case "failed":
      return tr("backup_step_failed", undefined, "Failed");
    case "skipped":
      return tr("backup_step_skipped", undefined, "Skipped");
    default:
      return tr("backup_step_waiting", undefined, "Waiting");
  }
}

function whyLabel(code: string, tr: Tr): string {
  switch (code) {
    case "console_gone":
      return tr(
        "backup_why_console_gone",
        undefined,
        "the PS5 stopped answering",
      );
    case "interrupted":
      return tr(
        "backup_why_interrupted",
        undefined,
        "the app's engine stopped (closed or restarted)",
      );
    case "cancelled":
      return tr("backup_why_cancelled", undefined, "it was cancelled");
    default:
      return tr("backup_why_other", undefined, "it did not finish");
  }
}

function triggerLabel(trigger: string, tr: Tr): string {
  switch (trigger) {
    case "manual":
      return tr("saves_server_trigger_manual", undefined, "Back up now");
    case "closed":
      return tr("saves_server_trigger_closed", undefined, "a game was closed");
    case "interval":
    case "auto":
      return tr("saves_server_trigger_interval", undefined, "on schedule");
    case "continue":
      return tr("backup_trigger_continue", undefined, "Continue");
    case "retry":
      return tr("backup_trigger_retry", undefined, "Retry");
    default:
      return trigger;
  }
}

function stateLabel(s: BackupRunSummary["state"], tr: Tr): string {
  switch (s) {
    case "running":
      return tr("backup_state_running", undefined, "running");
    case "done":
      return tr("backup_state_done", undefined, "finished");
    case "stopped":
      return tr("backup_state_stopped", undefined, "stopped");
    case "cancelled":
      return tr("backup_state_cancelled", undefined, "cancelled");
    default:
      return tr("backup_state_interrupted", undefined, "interrupted");
  }
}

/** "The last backup stopped after 7 of 66 saves: the PS5 stopped answering at 03:12." */
export function stoppedText(note: StoppedNote, tr: Tr): string {
  return tr(
    "backup_stopped_note",
    {
      done: note.done,
      total: note.total,
      why: whyLabel(note.code, tr),
      time: formatDate(note.at, "time-short"),
    },
    "The last backup stopped after {done} of {total} saves: {why} at {time}.",
  );
}

/** On a backup card: what happened to a run that did not go through, with Continue, and the way
 *  to the progress view. Nothing for an engine without runs. */
export function BackupRunBar({
  which,
  runs,
  running,
  onOpen,
  onStarted,
  testId,
}: {
  which: BackupWhich;
  runs: ReturnType<typeof useBackupRuns>;
  /** The card's own "a backup runs" (from its status). */
  running: boolean;
  onOpen: () => void;
  /** Continue was pressed: the card follows the run. */
  onStarted?: () => void;
  testId: string;
}) {
  const tr = useTr();
  if (!runs.supported) return null;
  const note = running ? null : stoppedNote(runs.view);
  const hasRun = !!runs.view?.run;
  return (
    <div
      className="mt-2 flex flex-wrap items-center gap-2 text-xs"
      data-testid={testId}
    >
      {note && (
        <p
          className="min-w-0 flex-1 basis-[14rem] text-[var(--color-warn)]"
          data-testid={`${testId}-stopped`}
        >
          {stoppedText(note, tr)}
        </p>
      )}
      {note?.canContinue && (
        <Button
          variant="secondary"
          size="sm"
          leftIcon={<Play size={12} />}
          onClick={() => void runs.again().then(() => onStarted?.())}
          data-testid={`${testId}-continue`}
        >
          {tr("backup_progress_continue", undefined, "Continue backup")}
        </Button>
      )}
      {(hasRun || running) && (
        <Button
          variant="ghost"
          size="sm"
          leftIcon={<Activity size={12} />}
          onClick={onOpen}
          data-testid={`${testId}-open`}
          aria-label={
            tr("backup_progress_open", undefined, "Backup progress") +
            ` (${which})`
          }
        >
          {tr("backup_progress_open", undefined, "Backup progress")}
        </Button>
      )}
    </div>
  );
}

/** A save's game (named where known) and whose it is. */
function SaveTitle({ row, owners }: { row: Row; owners: Owners }) {
  const tr = useTr();
  const { name } = useSaveGameName(row.title_id);
  const owner = owners.find((o) => o.id >>> 0 === row.user_id >>> 0);
  const who =
    owner?.name ??
    tr("saves_user_other", { id: userHex(row.user_id) }, "Other user ({id})");
  return (
    <>
      {name ? `${name} (${row.title_id})` : row.title_id}
      <span className="text-[var(--color-muted)]"> · {who}</span>
    </>
  );
}

function RowView({
  row,
  owners,
  canRetry,
  onRetry,
}: {
  row: Row;
  owners: Owners;
  canRetry: boolean;
  onRetry: () => void;
}) {
  const tr = useTr();
  const tone =
    row.step === "failed"
      ? "text-[var(--color-bad)]"
      : row.step === "verified"
        ? "text-[var(--color-good)]"
        : "";
  return (
    <li
      className="border-b border-[var(--color-border)] py-2 last:border-b-0"
      data-testid="backup-progress-row"
    >
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <span
          className="min-w-0 flex-1 basis-[12rem] truncate text-sm"
          title={row.path}
        >
          {row.kind === "folder" ? (
            row.path
          ) : (
            <SaveTitle row={row} owners={owners} />
          )}
        </span>
        {row.size > 0 && (
          <span className="text-xs text-[var(--color-muted)]">
            {formatBytes(row.size)}
          </span>
        )}
        <span className={`text-xs ${tone}`}>
          {stepLabel(row.step, row.pct, tr)}
        </span>
        {canRetry && row.outcome === "failed" && (
          <Button
            variant="ghost"
            size="sm"
            leftIcon={<RotateCcw size={12} />}
            onClick={onRetry}
          >
            {tr("backup_progress_retry", undefined, "Retry")}
          </Button>
        )}
      </div>
      {row.places.length > 0 && (
        <div className="mt-1 flex flex-wrap gap-x-3 text-xs text-[var(--color-muted)]">
          {row.places.map((p) => (
            <span key={p.place} title={p.error ?? undefined}>
              {placeLabel(p.place, tr)}: {stepLabel(p.step, p.pct, tr)}
            </span>
          ))}
        </div>
      )}
      {row.error && row.outcome !== "verified" && (
        <p className="mt-1 break-words text-xs text-[var(--color-bad)]">
          {row.error}
        </p>
      )}
    </li>
  );
}

/** The Backup progress view (#895): how the run goes overall and for each save in each place,
 *  with Retry, Cancel, Continue and the last runs. */
export function BackupProgressModal({
  which,
  open,
  onClose,
  owners,
}: {
  which: BackupWhich;
  open: boolean;
  onClose: () => void;
  owners: Owners;
}) {
  const tr = useTr();
  const runs = useBackupRuns(which, open);
  const [filter, setFilter] = useState<RowFilter>("all");
  const meter = useRef(new SpeedMeter());
  const [speed, setSpeed] = useState<number | null>(null);
  const view = runs.view;
  const run = view?.run ?? null;
  useEffect(() => {
    if (view?.run && view.summary)
      setSpeed(
        meter.current.sample(view.run.id, view.summary.bytes_done, view.now),
      );
  }, [view]);
  const all = overall(
    view ?? { run: null, summary: null, history: [], now: 0 },
    speed,
  );
  const shown = rows(run, which, filter);
  const failed = retryable(run);
  const note = stoppedNote(view);
  const running = run?.state === "running";
  const title =
    which === "share"
      ? tr(
          "backup_progress_title_share",
          undefined,
          "Backup progress: network share",
        )
      : tr(
          "backup_progress_title_local",
          undefined,
          "Backup progress: automatic backups",
        );
  const footer = (
    <>
      {running && (
        <Button
          variant="secondary"
          leftIcon={<Square size={12} />}
          onClick={() => void runs.cancel()}
        >
          {tr("backup_progress_cancel", undefined, "Cancel backup")}
        </Button>
      )}
      {!running && failed.length > 0 && (
        <Button
          variant="secondary"
          leftIcon={<RotateCcw size={12} />}
          onClick={() => void runs.again(failed)}
        >
          {tr(
            "backup_progress_retry_failed",
            { n: failed.length },
            "Retry failed ({n})",
          )}
        </Button>
      )}
      {/* Saves left (a run that stopped, or ones a Retry did not take up): Continue does them. */}
      {!running && (view?.summary?.left ?? 0) > 0 && (
        <Button
          variant="primary"
          leftIcon={<Play size={12} />}
          onClick={() => void runs.again()}
        >
          {tr("backup_progress_continue", undefined, "Continue backup")}
        </Button>
      )}
    </>
  );
  return (
    <Modal
      open={open}
      onClose={onClose}
      title={title}
      titleIcon={<Activity size={16} />}
      size="xl"
      footer={footer}
    >
      <div data-testid={`backup-progress-${which}`}>
        {runs.error && (
          <p className="mb-2 text-xs text-[var(--color-bad)]">{runs.error}</p>
        )}
        {!run || !all ? (
          <p className="text-sm text-[var(--color-muted)]">
            {tr("backup_progress_none", undefined, "No backup has run yet.")}
          </p>
        ) : (
          <>
            <div className="mb-1 flex flex-wrap items-baseline gap-x-3 gap-y-1 text-sm">
              <span className="font-semibold">
                {tr(
                  "backup_progress_saves",
                  { done: all.done, total: all.total },
                  "{done} of {total} saves",
                )}
              </span>
              <span className="text-[var(--color-muted)]">
                {tr(
                  "backup_progress_bytes",
                  {
                    done: formatBytes(all.bytesDone),
                    total: formatBytes(all.bytesTotal),
                  },
                  "{done} of {total}",
                )}
              </span>
              {all.speed !== null && (
                <span className="text-[var(--color-muted)]">
                  {tr(
                    "backup_progress_speed",
                    { speed: formatBytes(all.speed) },
                    "{speed}/s",
                  )}
                </span>
              )}
              <span className="text-[var(--color-muted)]">
                {tr(
                  "backup_progress_elapsed",
                  { time: formatDuration(all.elapsedMs / 1000) },
                  "Elapsed {time}",
                )}
              </span>
              {all.leftMs !== null && (
                <span className="text-[var(--color-muted)]">
                  {tr(
                    "backup_progress_left",
                    { time: formatDuration(all.leftMs / 1000) },
                    "About {time} left",
                  )}
                </span>
              )}
            </div>
            <ProgressBar
              value={
                all.total > 0
                  ? (all.done + all.failed + all.skipped) / all.total
                  : running
                    ? null
                    : 1
              }
              tone={all.failed > 0 ? "warn" : running ? "accent" : "good"}
              label={title}
            />
            <p className="mt-1 text-xs text-[var(--color-muted)]">
              {[
                `${triggerLabel(run.trigger, tr)} · ${formatDate(run.started_at)} · ${stateLabel(run.state, tr)}`,
                all.failed > 0
                  ? tr(
                      "backup_progress_failed_n",
                      { n: all.failed },
                      "{n} failed",
                    )
                  : null,
                all.skipped > 0
                  ? tr(
                      "backup_progress_skipped_n",
                      { n: all.skipped },
                      "{n} skipped",
                    )
                  : null,
                all.left > 0
                  ? tr(
                      "backup_progress_waiting_n",
                      { n: all.left },
                      "{n} still to do",
                    )
                  : null,
              ]
                .filter(Boolean)
                .join(" · ")}
            </p>
            {note && (
              <p className="mt-1 text-xs text-[var(--color-warn)]">
                {stoppedText(note, tr)}
              </p>
            )}
            {Object.entries(run.unreached).map(([host, why]) => (
              <p
                key={host}
                className="mt-1 break-words text-xs text-[var(--color-bad)]"
              >
                {tr(
                  "backup_progress_unreached",
                  { console: host, why },
                  "{console} could not be reached: {why}",
                )}
              </p>
            ))}
            <div className="mt-3">
              <SegmentedControl
                value={filter}
                onChange={(v) => setFilter(v as RowFilter)}
                ariaLabel={tr("backup_progress_filter", undefined, "Show")}
                segments={[
                  {
                    value: "all",
                    label: tr("backup_progress_filter_all", undefined, "All"),
                  },
                  {
                    value: "failed",
                    label: tr(
                      "backup_progress_filter_failed",
                      undefined,
                      "Failed",
                    ),
                  },
                  {
                    value: "left",
                    label: tr(
                      "backup_progress_filter_left",
                      undefined,
                      "To do",
                    ),
                  },
                  {
                    value: "verified",
                    label: tr(
                      "backup_progress_filter_verified",
                      undefined,
                      "Verified",
                    ),
                  },
                ]}
              />
            </div>
            <ul className="mt-2 max-h-[50vh] overflow-y-auto pe-1">
              {shown.map((r) => (
                <RowView
                  key={r.id}
                  row={r}
                  owners={owners}
                  canRetry={!running}
                  onRetry={() => void runs.again([r.id])}
                />
              ))}
            </ul>
          </>
        )}
        {(view?.history.length ?? 0) > 1 && (
          <details className="mt-3 text-xs">
            <summary className="cursor-pointer text-[var(--color-muted)]">
              {tr("backup_progress_history", undefined, "Earlier runs")}
            </summary>
            <ul className="mt-1 space-y-0.5">
              {view!.history.map((h) => (
                <li key={h.id} className="text-[var(--color-muted)]">
                  {tr(
                    "backup_history_row",
                    {
                      when: formatDate(h.started_at),
                      why: `${triggerLabel(h.trigger, tr)}, ${stateLabel(h.state, tr)}`,
                      ok: h.verified,
                      failed: h.failed,
                      left: h.left,
                    },
                    "{when} ({why}): {ok} verified, {failed} failed, {left} left",
                  )}
                  {h.reason_code && h.state !== "done"
                    ? ` · ${whyLabel(h.reason_code, tr)}`
                    : ""}
                  {h.first_error ? ` · ${h.first_error}` : ""}
                </li>
              ))}
            </ul>
          </details>
        )}
      </div>
    </Modal>
  );
}
