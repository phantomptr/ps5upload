import { getEngineUrl } from "../state/engine";
import { hostOf } from "./addr";
import { redactHost } from "./diagnosticBundle";

/**
 * Host-side (engine) state for the bug-report bundle.
 *
 * These are NOT PS5 probes — they read the engine's own in-memory state over
 * loopback and answer immediately, so they are safe to collect even when the
 * console is off. They cover the two questions a transfer/install report
 * always raises and that the bundle previously could not answer:
 *
 *  - `jobs`     — why a transfer ended, with the structured `error_reason` /
 *                 `error_detail` the UI shows. The bundle used to carry only
 *                 the renderer's `recent_activity` labels ("Installing X",
 *                 outcome "failed"), which name the failure but never explain
 *                 it.
 *  - `sessions` — what the console said about an install. `install/status`
 *                 requires a session id, so once the user navigated away the
 *                 err_code and phase were unrecoverable.
 *  - `job_summaries` — the last 20 per-job telemetry records (where each transfer's
 *                 time went, how it ended, the console's own end-of-job line). The
 *                 engine writes them locally; they hold no address or path.
 *  - `install_history` — the unified install endpoint's persisted per-console
 *                 history (verdict, Sony code, route, metrics). Engine-side
 *                 disk, so a failure from before this session still rides
 *                 along. An array, not a host-keyed map: redaction turns every
 *                 IPv4 into the same `<IPv4>`, which would collapse consoles.
 */

/** Per-request budget. The engine is loopback and answers from memory; a
 *  wedged accept loop must not hang bundle generation, which is the one
 *  action a user takes precisely because things are already broken. */
const TIMEOUT_MS = 2000;

async function getJsonBounded<T>(path: string): Promise<T> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), TIMEOUT_MS);
  try {
    const res = await fetch(`${getEngineUrl()}${path}`, {
      signal: controller.signal,
    });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    return (await res.json()) as T;
  } finally {
    clearTimeout(timer);
  }
}

/** How many of the newest job summaries ride along in a bug report (review 009 #4). */
export const BUNDLE_JOB_SUMMARIES = 20;

export interface EngineDiagnostics {
  /** Transfer jobs the engine still holds, newest state included. */
  jobs: unknown[] | null;
  /** Live pkg-install sessions. */
  install_sessions: unknown[] | null;
  /** Recent unified installs per known console, newest first. `entries` is
   *  null when that console's history could not be read. */
  install_history: { console: string; entries: unknown[] | null }[];
  /** The newest per-job telemetry records, newest first: where each job's time went and how it
   *  ended. Local to the engine, holding no address or path (a hash names the console). */
  job_summaries: unknown[] | null;
  /** Per-probe failures, so "not collected" is never read as "nothing there". */
  errors: Record<string, string>;
}

export async function collectEngineDiagnostics(
  opts: { consoles?: string[]; redact?: boolean } = {},
): Promise<EngineDiagnostics> {
  const errors: Record<string, string> = {};
  async function probe<T>(name: string, path: string): Promise<T | null> {
    try {
      return await getJsonBounded<T>(path);
    } catch (e) {
      errors[name] = e instanceof Error ? e.message : String(e);
      return null;
    }
  }
  const [jobs, sessions, summaries] = await Promise.all([
    probe<unknown[]>("jobs", "/api/jobs"),
    probe<unknown[]>("install_sessions", "/api/pkg/install/sessions"),
    probe<{ summaries?: unknown[] }>(
      "job_summaries",
      `/api/jobs/summaries?limit=${BUNDLE_JOB_SUMMARIES}`,
    ),
  ]);
  // One probe per distinct console (host:port and bare-host forms are the
  // same console to the engine's history store).
  const hosts = [
    ...new Set((opts.consoles ?? []).map((c) => hostOf(c).trim()).filter(Boolean)),
  ];
  const install_history = await Promise.all(
    hosts.map(async (host) => {
      const label = redactHost(host, opts.redact ?? true);
      const entries = await probe<unknown[]>(
        `install_history:${label}`,
        `/api/pkg/install/history?ps5_addr=${encodeURIComponent(host)}`,
      );
      return { console: label, entries };
    }),
  );
  return {
    jobs,
    install_sessions: sessions,
    install_history,
    job_summaries: Array.isArray(summaries?.summaries) ? summaries.summaries : null,
    errors,
  };
}
