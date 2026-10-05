import { useEffect, useMemo, useState } from "react";
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

const POLL_MS = 2000;

/** Transfers the engine is still running that nothing in this tab is watching: what a reopened
 *  browser tab finds, because the self-hosted engine keeps a job going after the tab closes
 *  (R15, #372). Shows each one's progress and a Cancel. Renders nothing in the desktop app (its
 *  engine ends with the app, so there is never an orphan) or when there is none. */
export function RunningEngineJobs() {
  const web = !isTauriEnv();
  const visible = useDocumentVisible();
  const [jobs, setJobs] = useState<RunningEngineJob[]>([]);
  const queueItems = useUploadQueueStore((s) => s.items);
  const phases = useTransferStore((s) => s.phasesByHost);

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
    const claimed = new Set<string>();
    for (const it of queueItems) {
      if (it.jobId && (it.status === "running" || it.attachJobId)) claimed.add(it.jobId);
      if (it.attachJobId) claimed.add(it.attachJobId);
    }
    for (const p of Object.values(phases)) {
      if (p.kind === "running" && p.jobId) claimed.add(p.jobId);
    }
    return unclaimedJobs(jobs, claimed);
  }, [jobs, queueItems, phases]);

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
}

function RunningJobRow({ job, onGone }: { job: RunningEngineJob; onGone: () => void }) {
  const tr = useTr();
  const { bytesSent, totalBytes, filesFinalized, filesFinalizingTotal } = job;
  const { rate, etaSeconds } = useRateEta(job.jobId, bytesSent, totalBytes);
  const pct = totalBytes > 0 ? Math.min(100, (bytesSent / totalBytes) * 100) : null;
  const finishing = totalBytes > 0 && bytesSent >= totalBytes;
  return (
    <div className="rounded-md border border-[var(--color-accent)] bg-[var(--color-surface-2)] p-3 text-xs">
      <div className="mb-1 flex items-center gap-2">
        <Loader2 size={14} className="animate-spin text-[var(--color-accent)]" aria-hidden />
        <span className="font-semibold">
          {tr(
            "engine_job_running_title",
            undefined,
            "A transfer is still running on the engine",
          )}
        </span>
        <button
          type="button"
          className="ml-auto rounded-md border border-[var(--color-border)] px-2 py-0.5 hover:bg-[var(--color-surface-3)]"
          onClick={() => {
            void jobCancel(job.jobId).catch(() => {});
            onGone();
          }}
        >
          {tr("cancel", undefined, "Cancel")}
        </button>
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
      <div className="h-1.5 w-full overflow-hidden rounded-full bg-[var(--color-surface-3)]">
        <div
          className={`h-full bg-[var(--color-accent)] transition-[width] duration-300 ${
            pct === null || finishing ? "animate-pulse" : ""
          }`}
          style={{ width: `${Math.max(pct ?? 0, 4)}%` }}
        />
      </div>
    </div>
  );
}
