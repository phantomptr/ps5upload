// Payload sources the user adds (a GitHub, Gitea or Forgejo project that publishes payload files in
// its releases), through the engine's /api/payloads/repos routes (engine payloads_api.rs, core
// payload_library/user_sources.rs). Plain fetches against the engine origin, so the desktop app,
// the phones and the browser build take the same path, and the file lands in the engine's
// payload folder, which the send reads.

import { getEngineUrl } from "../state/engine";

/** Why a source call failed: `code` is stable (the app words it), `message` is the engine's
 *  English. `retry_at`: when the host said to ask again (seconds since the Unix epoch). */
export interface SourceProblem {
  code: string;
  message: string;
  retry_at?: number;
}

export interface SourceHaveCopy {
  /** The release it came from; empty for a copy an older version saved. */
  tag: string;
  file_name: string;
  size: number;
  /** SHA-256 of the file as it is on disk: what a send gives the console. */
  sha256: string;
  published_at: string;
  downloaded_at: number;
  /** "matched", "none", or "" for a copy from an older version. */
  checksum: string;
  /** Where it is, on the engine's computer: what Send uses. */
  path: string;
}

export interface SourceLatestFile {
  tag: string;
  release_name: string;
  file_name: string;
  size: number;
  published_at: string;
  prerelease: boolean;
  html_url: string;
}

export interface PayloadSource {
  id: string;
  name: string;
  kind: "github" | "gitea";
  project_url: string;
  owner: string;
  repo: string;
  /** The picked file; "" for a source added before 12.6.0, which asks for it. */
  file_name: string;
  /** Which names newer releases may use: the picked name with `*` for each version part. */
  file_pattern: string;
  have?: SourceHaveCopy;
  latest?: SourceLatestFile;
  update_available: boolean;
  checked_age_secs?: number;
  error?: SourceProblem;
}

export interface SourcePreviewFile {
  name: string;
  size: number;
  pattern: string;
}

export interface SourcePreviewRelease {
  tag: string;
  name: string;
  published_at: string;
  prerelease: boolean;
  html_url: string;
  files: SourcePreviewFile[];
}

export interface SourcePreview {
  id: string;
  name: string;
  kind: "github" | "gitea";
  project_url: string;
  already_added: boolean;
  current_file: string;
  /** Newest first; only releases with a .elf or .bin file. */
  releases: SourcePreviewRelease[];
}

/** A refused source call, with the engine's code and, for a rate limit, when to try again. */
export class PayloadSourceError extends Error {
  readonly code: string;
  readonly retryAt: number | null;
  constructor(problem: SourceProblem) {
    super(problem.message);
    this.name = "PayloadSourceError";
    this.code = problem.code;
    this.retryAt = typeof problem.retry_at === "number" ? problem.retry_at : null;
  }

  get problem(): SourceProblem {
    return { code: this.code, message: this.message, retry_at: this.retryAt ?? undefined };
  }
}

/** A download can take a while on a slow line; the engine ends a stalled one itself. */
const DOWNLOAD_TIMEOUT_MS = 10 * 60_000;

async function call<T>(method: string, path: string, body?: unknown, timeoutMs = 45_000): Promise<T> {
  const res = await fetch(`${getEngineUrl()}${path}`, {
    method,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(timeoutMs),
  });
  const v = (await res.json().catch(() => ({}))) as T & { error?: string; code?: string; retry_at?: number };
  if (!res.ok) {
    throw new PayloadSourceError({
      code: v.code ?? `http_${res.status}`,
      message: v.error || `request failed (${res.status})`,
      retry_at: v.retry_at,
    });
  }
  return v;
}

/** The sources. `check`: read each one's newest releases (kept ten minutes unless `force`);
 *  without it nothing asks the network. */
export async function listPayloadSources(check = false, force = false): Promise<PayloadSource[]> {
  const q = check ? `?check=1${force ? "&force=1" : ""}` : "";
  const v = await call<{ sources?: PayloadSource[] }>("GET", `/api/payloads/repos${q}`, undefined, 120_000);
  return Array.isArray(v.sources) ? v.sources : [];
}

/** A project's newest releases with payload files, for picking the file. Saves nothing. */
export function previewPayloadSource(url: string): Promise<SourcePreview> {
  return call("POST", "/api/payloads/repos/preview", { url });
}

/** Add a project with the picked file (adding one again changes its file). Asks no network. */
export function addPayloadSource(url: string, fileName: string): Promise<PayloadSource> {
  return call("POST", "/api/payloads/repos", { url, file_name: fileName });
}

/** Change a source's file, or pick it for one added before 12.6.0. */
export function setPayloadSourceFile(id: string, fileName: string): Promise<PayloadSource> {
  return call("PUT", `/api/payloads/repos/${encodeURIComponent(id)}/file`, { file_name: fileName });
}

/** Download the newest release's file into the payload folder (checked before it replaces the
 *  copy there). */
export function downloadPayloadSource(id: string): Promise<PayloadSource> {
  return call("POST", `/api/payloads/repos/${encodeURIComponent(id)}/download`, {}, DOWNLOAD_TIMEOUT_MS);
}

/** Remove a source and its downloaded file. */
export async function removePayloadSource(id: string): Promise<boolean> {
  const v = await call<{ removed?: boolean }>("DELETE", `/api/payloads/repos/${encodeURIComponent(id)}`);
  return v.removed === true;
}
