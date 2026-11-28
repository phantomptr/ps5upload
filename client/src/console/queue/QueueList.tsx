/**
 * The console's queue on Home (#982): what runs now (with its speed, elapsed time and time left),
 * then what waits, in order, then what ended and how. Cancel or Remove per item, Move to top for
 * what waits, Clear finished, Pause / Resume. Hidden while the queue is empty.
 */
import { useState } from "react";
import { ArrowUpToLine, CircleX, Copy, PackagePlus, Pause, Play, Trash2 } from "lucide-react";
import { formatBytes } from "../../lib/format";
import { ApiError } from "../api";
import { filesErrorText } from "../files/Files";
import { t } from "../strings";
import { RateLine } from "../ui/rate";
import { cancelQueued, clearFinished, linePosition, moveToTop, setPaused, unfinished, useQueue, type Queue, type QueueItem } from "./queue";

/** Why it waits, in words. */
export function waitText(wait: string): string {
  return t(wait === "convert_busy" ? "cw_queue_wait_convert" : "cw_queue_wait_install");
}

/** How a failed item failed, in words. */
export function failText(error: string): string {
  if (error === "interrupted") return t("cw_queue_err_interrupted");
  return filesErrorText(new ApiError(500, error)) ?? error;
}

/** The item's state line: "Copying · 45% · 1.2 GiB of 2.6 GiB", "#2 in line", "Failed: …". */
export function itemLine(q: Queue, i: QueueItem): string {
  if (i.state === "running") {
    if (i.cancel) return t("cw_files_batch_cancelling");
    const doing = t(i.kind === "copy" ? "cw_queue_copying" : i.step === 0 && i.src.startsWith("smb://") ? "cw_queue_staging" : "cw_queue_installing");
    if (!(i.bytes_total > 0)) return doing;
    const pct = Math.min(100, Math.floor((i.bytes_done / i.bytes_total) * 100));
    return `${doing} · ${t("cw_home_percent", { n: pct })} · ${t("cw_home_progress_bytes", { done: formatBytes(i.bytes_done), total: formatBytes(i.bytes_total) })}`;
  }
  if (i.state === "pending") return i.wait ? waitText(i.wait) : t("cw_queue_position", { n: linePosition(q, i.id) });
  if (i.state === "done") return t("cw_home_state_done");
  if (i.state === "cancelled") return t("cw_queue_cancelled");
  return t("cw_queue_failed", { why: failText(i.error) });
}

function QueueRow({ q, i, onError }: { q: Queue; i: QueueItem; onError: (e: unknown) => void }) {
  const Icon = i.kind === "copy" ? Copy : PackagePlus;
  const running = i.state === "running";
  const pos = i.state === "pending" ? linePosition(q, i.id) : 0;
  const pct = running && i.bytes_total > 0 ? Math.min(100, Math.floor((i.bytes_done / i.bytes_total) * 100)) : null;
  const act = (fn: () => Promise<void>) => () => void fn().catch(onError);
  return (
    <li className={`cw-task is-${running ? "running" : i.state === "pending" ? "running" : i.state === "done" ? "done" : "failed"}`} data-queue-item={i.id}>
      <div className="cw-focusable cw-task-main" tabIndex={0}>
        <Icon className="cw-task-icon" aria-hidden />
        <div className="cw-task-text">
          <div className="cw-task-title">
            <span>
              {t(i.kind === "copy" ? "cw_home_kind_copy" : "cw_lib_install")}
              {i.name ? <span className="cw-dim"> · <span dir="auto">{i.name}</span></span> : null}
            </span>
          </div>
          {running ? (
            <div className="cw-bar" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct ?? undefined}>
              <div className={`cw-bar-fill${pct == null ? " is-unknown" : ""}`} style={{ width: `${pct ?? 100}%` }} />
            </div>
          ) : null}
          <div className="cw-task-sub">{itemLine(q, i)}</div>
          {running && !i.cancel ? (
            <div className="cw-task-sub">
              <RateLine id={i.job ? `q:${i.job}` : `q:${i.id}`} done={i.bytes_done} total={i.bytes_total} elapsedS={i.elapsed_s} />
            </div>
          ) : null}
        </div>
      </div>
      {pos > 1 ? (
        <button type="button" className="cw-focusable cw-button" data-action="queue-top" onClick={act(() => moveToTop(i.id))}>
          <ArrowUpToLine aria-hidden />
          <span>{t("cw_queue_top")}</span>
        </button>
      ) : null}
      {unfinished(i) && !i.cancel ? (
        <button type="button" className="cw-focusable cw-button" data-action="queue-cancel" onClick={act(() => cancelQueued(i.id))}>
          {running ? <CircleX aria-hidden /> : <Trash2 aria-hidden />}
          <span>{t(running ? "cw_home_cancel_task" : "cw_queue_remove")}</span>
        </button>
      ) : null}
    </li>
  );
}

/** The queue's section (pure: the state comes in, for tests). */
export function QueueView({ q, onError }: { q: Queue; onError: (e: unknown) => void }) {
  if (!q.items.length && !q.paused) return null;
  const ended = q.items.some((i) => !unfinished(i));
  const waiting = q.items.some(unfinished);
  const act = (fn: () => Promise<void>) => () => void fn().catch(onError);
  return (
    <section className="cw-home-section" data-queue="">
      <h2 className="cw-h2">{t("cw_queue_title")}</h2>
      {q.paused ? (
        <p className="cw-msg" role="status">
          {t(q.reason === "interrupted_install" ? "cw_queue_paused_interrupted" : "cw_queue_paused")}
        </p>
      ) : null}
      <ul className="cw-tasks">
        {q.items.map((i) => (
          <QueueRow key={i.id} q={q} i={i} onError={onError} />
        ))}
      </ul>
      <div className="cw-dialog-buttons">
        {q.paused ? (
          <button type="button" className="cw-focusable cw-button" data-action="queue-resume" onClick={act(() => setPaused(false))}>
            <Play aria-hidden />
            <span>{t("cw_queue_resume")}</span>
          </button>
        ) : waiting ? (
          <button type="button" className="cw-focusable cw-button" data-action="queue-pause" onClick={act(() => setPaused(true))}>
            <Pause aria-hidden />
            <span>{t("cw_queue_pause")}</span>
          </button>
        ) : null}
        {ended ? (
          <button type="button" className="cw-focusable cw-button" data-action="queue-clear" onClick={act(clearFinished)}>
            <Trash2 aria-hidden />
            <span>{t("cw_queue_clear")}</span>
          </button>
        ) : null}
      </div>
    </section>
  );
}

/** Home's queue section, live. */
export function QueueSection() {
  const { queue } = useQueue();
  const [error, setError] = useState<string | null>(null);
  return (
    <>
      <QueueView q={queue} onError={(e) => setError(filesErrorText(e))} />
      {error ? (
        <p className="cw-field-error" role="alert">
          {error}
        </p>
      ) : null}
    </>
  );
}
