import { getEngineUrl } from "../state/engine";
import { useLogsStore } from "../state/logs";
import { engineLogsTail } from "../api/ps5";

/**
 * Build and download a bug-report zip from a browser.
 *
 * The desktop app assembles its bundle in the Tauri shell, which can read the
 * local log files and open a save dialog. A browser can do neither, so the
 * self-hosted (Docker) web UI previously showed the whole Bug Report form and
 * then a line of grey text saying it required the desktop app — a user who had
 * just hit a bug filled in the description, ticked the options, and only then
 * discovered they could not file it. One reporter said exactly that: "Could not
 * capture a bug report on the webui."
 *
 * Everything the bundle needs is already reachable from the browser: the app's
 * own log ring lives in memory here, the engine's log tail is an HTTP endpoint,
 * and the PS5 snapshot and payload logs are fetched through the engine. The
 * only two things a browser cannot do — build a zip and hand it back as a file
 * — are done by the engine's /api/bug-report/bundle route.
 */

export interface BundleEntry {
  path: string;
  text?: string;
  base64?: string;
}

/** Serialize the in-memory app log as JSONL, matching the desktop's
 *  `logs/app.jsonl` shape (one object per line) so a maintainer reads the same
 *  file either way. */
export function appLogJsonl(
  entries: { timestamp: number; level: string; source: string; message: string; detail?: string }[],
  windowMinutes: number,
): string {
  const cutoff = Date.now() - windowMinutes * 60_000;
  return entries
    .filter((e) => e.timestamp >= cutoff)
    .map((e) =>
      JSON.stringify({
        ts: e.timestamp,
        level: e.level,
        source: e.source,
        message: e.message,
        ...(e.detail ? { detail: e.detail } : {}),
      }),
    )
    .join("\n");
}

/** Fetch the engine's log ring and render it like the desktop's engine.log. */
export async function engineLogText(): Promise<string> {
  const res = await engineLogsTail(0);
  return (res.entries ?? [])
    .map((e) => `[engine:${e.level}] ts=${e.ts_ms} ${e.msg}`)
    .join("\n");
}

/** Read a browser File as base64 (no data: prefix). */
export function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onerror = () => reject(r.error ?? new Error("read failed"));
    r.onload = () => {
      const s = String(r.result ?? "");
      const comma = s.indexOf(",");
      resolve(comma >= 0 ? s.slice(comma + 1) : s);
    };
    r.readAsDataURL(file);
  });
}

/**
 * POST the entries to the engine and trigger a download of the returned zip.
 *
 * Uses a blob URL + synthetic click: the engine sets Content-Disposition, but
 * going through fetch (rather than navigating) keeps the request on the same
 * code path as every other engine call, so an error surfaces as an error
 * instead of replacing the page with JSON.
 */
export async function downloadBugBundle(
  entries: BundleEntry[],
  filename: string,
): Promise<void> {
  const res = await fetch(`${getEngineUrl()}/api/bug-report/bundle`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ filename, entries }),
  });
  if (!res.ok) {
    let msg = `HTTP ${res.status}`;
    try {
      const j = (await res.json()) as { error?: string };
      if (j?.error) msg = j.error;
    } catch {
      /* non-JSON error body — keep the status */
    }
    throw new Error(msg);
  }
  const blob = await res.blob();
  const url = URL.createObjectURL(blob);
  try {
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    a.remove();
  } finally {
    // Revoke on the next tick: revoking synchronously can cancel the download
    // in some browsers before it has read the blob.
    setTimeout(() => URL.revokeObjectURL(url), 10_000);
  }
}

/** Current app log entries, for callers that do not want the store directly. */
export function currentAppLog() {
  return useLogsStore.getState().entries;
}
