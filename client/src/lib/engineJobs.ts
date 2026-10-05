import { getEngineUrl } from "../state/engine";

/**
 * The engine's own list of running transfer jobs (`GET /api/jobs`).
 *
 * In the self-hosted web UI the engine outlives the browser tab: a transfer keeps running
 * after the tab closes (R15, #372). A reopened tab has no memory of it, so this list is how
 * the UI finds what is still going and shows its progress. Read over the engine's HTTP API
 * directly, so it needs no Tauri command.
 */

export interface RunningEngineJob {
  jobId: string;
  bytesSent: number;
  totalBytes: number;
  /** Files the console has made permanent so far, and the total (0 when not reported). */
  filesFinalized: number;
  filesFinalizingTotal: number;
  startedAtMs: number;
}

interface RawJob {
  job_id?: unknown;
  status?: unknown;
  job?: Record<string, unknown> | null;
}

const num = (v: unknown): number =>
  typeof v === "number" && Number.isFinite(v) && v > 0 ? v : 0;

/** The running jobs in a `/api/jobs` reply; anything else (finished, malformed) is dropped. */
export function parseRunningJobs(raw: unknown): RunningEngineJob[] {
  if (!Array.isArray(raw)) return [];
  const out: RunningEngineJob[] = [];
  for (const r of raw as RawJob[]) {
    if (!r || r.status !== "running" || typeof r.job_id !== "string") continue;
    const j = r.job ?? {};
    out.push({
      jobId: r.job_id,
      bytesSent: num(j.bytes_sent),
      totalBytes: num(j.total_bytes),
      filesFinalized: num(j.files_finalized),
      filesFinalizingTotal: num(j.files_finalizing_total),
      startedAtMs: num(j.started_at_ms),
    });
  }
  return out;
}

/** The jobs nothing in this tab is watching: running on the engine, but not in `claimed`. */
export function unclaimedJobs(
  jobs: RunningEngineJob[],
  claimed: ReadonlySet<string>,
): RunningEngineJob[] {
  return jobs.filter((j) => !claimed.has(j.jobId));
}

/** The engine's running jobs, or [] when it cannot be reached. */
export async function fetchRunningJobs(): Promise<RunningEngineJob[]> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 3000);
  try {
    const res = await fetch(`${getEngineUrl()}/api/jobs`, { signal: controller.signal });
    if (!res.ok) return [];
    return parseRunningJobs(await res.json());
  } catch {
    return [];
  } finally {
    clearTimeout(timer);
  }
}
