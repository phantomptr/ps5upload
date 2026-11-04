import { CircleX, Copy, Download, FolderCog, ListTodo, Upload, type LucideIcon } from "lucide-react";
import { formatBytes } from "../../lib/format";
import { t } from "../strings";
import type { Job } from "./useJobs";

const KIND_ICON: Record<Job["kind"], LucideIcon> = {
  upload: Upload,
  download: Download,
  copy: Copy,
  op: FolderCog,
  other: ListTodo,
};

export function kindLabel(kind: Job["kind"]): string {
  return t(`cw_home_kind_${KIND_ICON[kind] ? kind : "other"}`);
}

/** 0–100 from bytes, else from files; null when the job reports neither. */
export function jobPercent(j: Job): number | null {
  const [done, total] = j.bytes_total > 0 ? [j.bytes_done, j.bytes_total] : [j.files_done, j.files_total];
  if (!(total > 0)) return null;
  return Math.max(0, Math.min(100, Math.floor((done / total) * 100)));
}

function progressText(j: Job): string {
  const parts: string[] = [];
  if (j.bytes_total > 0)
    parts.push(
      t("cw_home_progress_bytes", {
        done: formatBytes(j.bytes_done),
        total: formatBytes(j.bytes_total),
      }),
    );
  if (j.files_total > 0) parts.push(t("cw_home_progress_files", { done: j.files_done, total: j.files_total }));
  return parts.join(" · ");
}

/**
 * One task on Home: what it is, who started it, how far it is. The row itself
 * is focusable so the D-pad reaches (and scrolls to) every task; a running one
 * has a Cancel button, which only asks (Home shows the confirm).
 */
export function TaskRow({ job, onCancel }: { job: Job; onCancel: (job: Job) => void }) {
  const Icon = KIND_ICON[job.kind] ?? ListTodo;
  const pct = jobPercent(job);
  const running = job.state === "running";
  const progress = progressText(job);
  return (
    <li className={`cw-task is-${job.state}`}>
      <div className="cw-focusable cw-task-main" tabIndex={0}>
        <Icon className="cw-task-icon" aria-hidden />
        <div className="cw-task-text">
          <div className="cw-task-title">
            <span>
              {kindLabel(job.kind)}
              {job.name ? <span className="cw-dim">{` · ${job.name}`}</span> : null}
            </span>
            <span className={`cw-tag${job.mine ? "" : " is-pc"}`}>
              {t(job.mine ? "cw_task_from_console" : "cw_task_from_pc")}
            </span>
          </div>
          {running ? (
            <div
              className="cw-bar"
              role="progressbar"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={pct ?? undefined}
            >
              <div className={`cw-bar-fill${pct == null ? " is-unknown" : ""}`} style={{ width: `${pct ?? 100}%` }} />
            </div>
          ) : null}
          <div className="cw-task-sub">
            {running
              ? pct != null
                ? t("cw_home_percent", { n: pct })
                : t("cw_home_state_running")
              : t(job.state === "done" ? "cw_home_state_done" : "cw_home_state_failed")}
            {progress ? <span className="cw-dim">{` · ${progress}`}</span> : null}
          </div>
        </div>
      </div>
      {running ? (
        <button type="button" className="cw-focusable cw-button" data-action="cancel" onClick={() => onCancel(job)}>
          <CircleX aria-hidden />
          <span>{t("cw_home_cancel_task")}</span>
        </button>
      ) : null}
    </li>
  );
}
