import { memo, useEffect, useMemo, useState } from "react";
import { Loader2 } from "lucide-react";

import { jobCancel } from "../api/ps5";
import { fetchRunningJobs, unclaimedJobs, type RunningEngineJob } from "../lib/engineJobs";
import { formatBytes } from "../lib/format";
import { isTauriEnv } from "../lib/tauriEnv";
import { formatEtaSeconds } from "../lib/uploadEta";
import { useRateEta } from "../lib/useRateEta";
import { useDocumentVisible } from "../lib/visibility";
import { useTr } from "../state/lang";
import { useTransferStore } from "../state/transfer";
import { useUploadQueueStore } from "../state/uploadQueue";
import { humanizePs5Error } from "../lib/humanizeError";
import { Button } from "./Button";
import { ProgressBar } from "./ProgressBar";

const POLL_MS = 2000;

/** Ask the engine to stop an orphaned job. Resolves to null once it is stopped (or already
 *  gone), or to the error when the request failed — the banner stays up then, because the
 *  transfer is still running. */
export async function cancelOrphanJob(
  jobId: string,
  cancel: (id: string) => Promise<unknown> = jobCancel,
): Promise<string | null> {
  try {
    await cancel(jobId);
    return null;
  } catch (e) {
    return e instanceof Error ? e.message : String(e);
  }
}

/** The job ids this tab is already watching, as one string so the selector's result only
 *  changes when the set does (not on every progress tick of the store it reads). */
const SEP = "\n";

/** Transfers the engine is still running that nothing in this tab is watching: what a reopened
 *  browser tab finds, because the self-hosted engine keeps a job going after the tab closes
 *  (R15, #372). Shows each one's progress and a Cancel. Renders nothing in the desktop app (its
 *  engine ends with the app, so there is never an orphan) or when there is none. */
export const RunningEngineJobs = memo(function RunningEngineJobs() {
  const web = !isTauriEnv();
  const visible = useDocumentVisible();
  const [jobs, setJobs] = useState<RunningEngineJob[]>([]);
  const queueClaimed = useUploadQueueStore((s) => {
    const ids: string[] = [];
    for (const it of s.items) {
      if (it.jobId && (it.status === "running" || it.attachJobId)) ids.push(it.jobId);
      if (it.attachJobId) ids.push(it.attachJobId);
    }
    return ids.join(SEP);
  });
  const oneShotClaimed = useTransferStore((s) =>
    Object.values(s.phasesByHost)
      .flatMap((p) => (p.kind === "running" && p.jobId ? [p.jobId] : []))
      .join(SEP),
  );

  useEffect(() => {
    if (!web || !visible) return;
    let live = true;
    const tick = async () => {
      const next = await fetchRunningJobs();
      if (live) setJobs(next);
    };
    void tick();
    const id = window.setInterval(() => void tick(), POLL_MS);
    return () => {
      live = false;
      window.clearInterval(id);
    };
  }, [web, visible]);

  const orphans = useMemo(() => {
    const claimed = new Set(
      [queueClaimed, oneShotClaimed].flatMap((ids) => (ids ? ids.split(SEP) : [])),
    );
    return unclaimedJobs(jobs, claimed);
  }, [jobs, queueClaimed, oneShotClaimed]);

  if (!web || orphans.length === 0) return null;
  return (
    <div className="mb-3 flex flex-col gap-2" data-testid="running-engine-jobs">
      {orphans.map((j) => (
        <RunningJobRow
          key={j.jobId}
          job={j}
          onGone={() => setJobs((cur) => cur.filter((x) => x.jobId !== j.jobId))}
        />
      ))}
    </div>
  );
});

function RunningJobRow({ job, onGone }: { job: RunningEngineJob; onGone: () => void }) {
  const tr = useTr();
  const { bytesSent, totalBytes, filesFinalized, filesFinalizingTotal } = job;
  const { rate, etaSeconds } = useRateEta(job.jobId, bytesSent, totalBytes);
  const pct = totalBytes > 0 ? Math.min(100, (bytesSent / totalBytes) * 100) : null;
  const finishing = totalBytes > 0 && bytesSent >= totalBytes;
  const [cancelling, setCancelling] = useState(false);
  const [cancelError, setCancelError] = useState<string | null>(null);
  const onCancel = async () => {
    setCancelling(true);
    setCancelError(null);
    const err = await cancelOrphanJob(job.jobId);
    setCancelling(false);
    if (err) setCancelError(err);
    else onGone();
  };
  return (
    <div className="rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-5 py-4 text-xs shadow-[var(--edge-highlight),var(--shadow-1)]">
      <div className="mb-2 flex items-center gap-2">
        <Loader2 size={14} className="animate-spin text-[var(--color-accent-bright)]" aria-hidden />
        <span className="min-w-0 flex-1 text-sm font-semibold">
          {tr(
            "engine_job_running_title",
            undefined,
            "A transfer is still running on the engine",
          )}
        </span>
        <Button variant="secondary" size="sm" loading={cancelling} onClick={() => void onCancel()}>
          {tr("cancel", undefined, "Cancel")}
        </Button>
      </div>
      <div className="mb-1 flex flex-wrap gap-x-3 gap-y-0.5 font-mono text-[var(--color-muted)]">
        <span>
          {pct !== null
            ? `${formatBytes(bytesSent)} / ${formatBytes(totalBytes)} (${pct.toFixed(0)}%)`
            : formatBytes(bytesSent)}
        </span>
        {!finishing && rate > 0 && <span>{formatBytes(rate)}/s</span>}
        {!finishing && etaSeconds !== null && (
          <span>
            {tr(
              "fs_progress_eta",
              { time: formatEtaSeconds(etaSeconds) },
              `about ${formatEtaSeconds(etaSeconds)} left`,
            )}
          </span>
        )}
        {finishing && (
          <span className="text-[var(--color-warn)]">
            {tr("upload_phase_settling", undefined, "Finishing on the console…")}
            {filesFinalizingTotal > 0 &&
              ` ${tr(
                "engine_job_files_done",
                {
                  done: filesFinalized.toLocaleString(),
                  total: filesFinalizingTotal.toLocaleString(),
                },
                `${filesFinalized.toLocaleString()} of ${filesFinalizingTotal.toLocaleString()} files`,
              )}`}
          </span>
        )}
      </div>
      <ProgressBar
        size="sm"
        value={pct === null || finishing ? null : pct / 100}
        label={tr("engine_job_running_title", undefined, "A transfer is still running on the engine")}
      />
      {cancelError && (
        <div role="alert" className="mt-2 text-[var(--color-bad)]">
          {tr(
            "engine_job_cancel_failed",
            { msg: humanizePs5Error(cancelError) },
            `Couldn't stop it: ${humanizePs5Error(cancelError)}. It is still running; try again.`,
          )}
        </div>
      )}
    </div>
  );
}
