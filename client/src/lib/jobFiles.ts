import { getEngineUrl } from "../state/engine";
import type { PlannedFile } from "../api/ps5";

/**
 * A running job's planned file list. The job snapshot polled every 500 ms only
 * carries its length (`files_count`): the list itself can be megabytes on a big
 * folder, so the UI fetches it once per job from `GET /api/jobs/{id}/files`.
 *
 * Read over the engine's HTTP API directly (like the job summary), so the desktop
 * app and the self-hosted web UI share it.
 */

const TIMEOUT_MS = 10_000;

/** The list, or null when the engine could not be read (the caller may try again).
 *  A finished or unknown job's list is empty. */
export async function fetchJobFiles(jobId: string): Promise<PlannedFile[] | null> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), TIMEOUT_MS);
  try {
    const res = await fetch(`${getEngineUrl()}/api/jobs/${encodeURIComponent(jobId)}/files`, {
      signal: controller.signal,
    });
    if (res.status === 404) return [];
    if (!res.ok) return null;
    const body = (await res.json()) as { files?: unknown };
    if (!Array.isArray(body?.files)) return null;
    return body.files.filter(
      (f): f is PlannedFile =>
        typeof f === "object" &&
        f !== null &&
        typeof (f as PlannedFile).rel_path === "string" &&
        typeof (f as PlannedFile).size === "number",
    );
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
}
