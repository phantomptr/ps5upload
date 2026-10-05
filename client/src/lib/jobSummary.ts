import { getEngineUrl } from "../state/engine";

/**
 * Per-job telemetry (review 009 #4): the engine writes one `job_summary` for every finished,
 * failed or cancelled transfer, on this machine only. The record names the console by a hash
 * and holds no address or local path, so it is safe to show and to put in a bug bundle.
 *
 * Read over the engine's HTTP API directly (the same way the bug report reads the engine log),
 * so the desktop app and the self-hosted web UI share it; there is no Tauri command to forget.
 */

export type DominantShare =
  | "receiver_bound"
  | "source_starved"
  | "credit_starved"
  | "network"
  | "unmeasured";

export interface JobSummaryShares {
  ticks?: number;
  credit_starved_pct?: number;
  source_starved_pct?: number;
  receiver_bound_pct?: number;
  receiver_bottleneck?: string;
}

export interface JobSummary {
  schema: number;
  job_id: string;
  kind: string;
  /** A hash of the console's key, never its address. */
  console: string | null;
  started_at_ms: number;
  ended_at_ms: number;
  elapsed_ms: number;
  result: "done" | "failed" | "cancelled";
  code: string | null;
  message: string | null;
  files: number;
  bytes: number;
  skipped_files: number;
  skipped_bytes: number;
  resumed: boolean;
  attempts: number;
  drive: string;
  engine_version: string;
  shares: JobSummaryShares | null;
  lanes_avg?: number;
  lanes_max?: number;
  chunk_avg_kib?: number;
  history?: [number, number, number][];
  slow_drive_switch?: boolean;
  settle_ms?: number;
  unswept_peak?: number;
  resent_bytes?: number;
  console_line?: string;
  why: { dominant: DominantShare; pct: number; text: string };
}

const TIMEOUT_MS = 3000;

async function getJson<T>(path: string): Promise<T | null> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), TIMEOUT_MS);
  try {
    const res = await fetch(`${getEngineUrl()}${path}`, { signal: controller.signal });
    if (!res.ok) return null;
    return (await res.json()) as T;
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
}

/** The finished job's summary, or null when there is none (still running, recorded off, or
 *  the engine is unreachable). */
export async function fetchJobSummary(jobId: string): Promise<JobSummary | null> {
  return getJson<JobSummary>(`/api/jobs/${encodeURIComponent(jobId)}/summary`);
}

/** The newest `limit` summaries, newest first. */
export async function fetchJobSummaries(limit = 20): Promise<JobSummary[]> {
  const r = await getJson<{ summaries?: JobSummary[] }>(`/api/jobs/summaries?limit=${limit}`);
  return Array.isArray(r?.summaries) ? r.summaries : [];
}

/** Catalog key and English text of the one-line interpretation for each dominant share. The
 *  keys are written out so the i18n scripts see them. */
export const WHY_TEXT: Record<DominantShare, { key: string; text: string }> = {
  receiver_bound: {
    key: "job_why_receiver_bound",
    text: "Console-bound {pct} %: the console could not take data faster than its drive or workers allowed.",
  },
  source_starved: {
    key: "job_why_source_starved",
    text: "Source-bound {pct} %: reading the source was the limit (a slow disk, a network share, or archive decoding on this computer).",
  },
  credit_starved: {
    key: "job_why_credit_starved",
    text: "Console memory {pct} %: the console's receive window was full, so it was the limit.",
  },
  network: {
    key: "job_why_network",
    text: "No single limit dominated: neither the console nor the source held the job back, so the network link was the limit.",
  },
  unmeasured: {
    key: "job_why_unmeasured",
    text: "This job ended before it could be measured.",
  },
};

export function whyEntry(summary: JobSummary): { key: string; text: string; pct: number } {
  const dominant = summary.why?.dominant;
  const e = dominant && dominant in WHY_TEXT ? WHY_TEXT[dominant] : WHY_TEXT.unmeasured;
  return { ...e, pct: Math.round(summary.why?.pct ?? 0) };
}
