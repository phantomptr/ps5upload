import { getEngineUrl } from "../state/engine";

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

export interface EngineDiagnostics {
  /** Transfer jobs the engine still holds, newest state included. */
  jobs: unknown[] | null;
  /** Live pkg-install sessions. */
  install_sessions: unknown[] | null;
  /** Per-probe failures, so "not collected" is never read as "nothing there". */
  errors: Record<string, string>;
}

export async function collectEngineDiagnostics(): Promise<EngineDiagnostics> {
  const errors: Record<string, string> = {};
  async function probe<T>(name: string, path: string): Promise<T | null> {
    try {
      return await getJsonBounded<T>(path);
    } catch (e) {
      errors[name] = e instanceof Error ? e.message : String(e);
      return null;
    }
  }
  const [jobs, sessions] = await Promise.all([
    probe<unknown[]>("jobs", "/api/jobs"),
    probe<unknown[]>("install_sessions", "/api/pkg/install/sessions"),
  ]);
  return { jobs, install_sessions: sessions, errors };
}
