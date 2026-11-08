import { getEngineUrl } from "../state/engine";

export interface BrowserPkgUpload {
  uploadId: string;
  path: string;
  filename: string;
  size: number;
  /** With `relPath`: the staged folder's own path on the engine (its first name). */
  root?: string;
}

async function responseError(response: Response): Promise<string> {
  const body = (await response.json().catch(() => null)) as {
    error?: unknown;
  } | null;
  return body?.error ? String(body.error) : `engine HTTP ${response.status}`;
}

/** What a caller can do with a staging upload that may take hours. */
export interface StageOptions {
  onProgress?: (sent: number, total: number) => void;
  signal?: AbortSignal;
  /** Files only: add the file to this earlier upload instead of starting a new one. */
  uploadId?: string;
  /** Files only: the file's place in a folder being sent (`Game/sce_sys/param.json`). */
  relPath?: string;
}

/**
 * Put a browser-selected package on the engine host. Browsers cannot hand the
 * engine a local filesystem path, while the desktop shell can. The returned
 * path is therefore temporary and must be paired with `deleteBrowserPkgUpload`
 * after both the direct Stream attempt and any staged fallback have finished.
 */
export async function stageBrowserPkg(file: File, opts?: StageOptions): Promise<BrowserPkgUpload> {
  return stageBrowserFileAt(file, "/api/pkg/upload", opts);
}

/** Stage any browser-selected file for the normal Upload review. */
export async function stageBrowserFile(file: File, opts?: StageOptions): Promise<BrowserPkgUpload> {
  return stageBrowserFileAt(file, "/api/files/upload", opts);
}

/** The POST, through `XMLHttpRequest` where there is one: `fetch` has no upload progress, and a
 *  50 GB file looked as if nothing was happening. */
function send(url: string, form: FormData, opts?: StageOptions): Promise<Response> {
  if (typeof XMLHttpRequest === "undefined") {
    return fetch(url, { method: "POST", body: form, signal: opts?.signal });
  }
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    const onAbort = () => xhr.abort();
    const done = () => opts?.signal?.removeEventListener("abort", onAbort);
    xhr.open("POST", url);
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable) opts?.onProgress?.(e.loaded, e.total);
    };
    xhr.onload = () => {
      done();
      resolve(new Response(xhr.responseText, { status: xhr.status }));
    };
    xhr.onerror = () => {
      done();
      reject(new TypeError("the upload to the server failed"));
    };
    xhr.onabort = () => {
      done();
      reject(new DOMException("the upload was cancelled", "AbortError"));
    };
    if (opts?.signal?.aborted) {
      xhr.onabort(new ProgressEvent("abort"));
      return;
    }
    opts?.signal?.addEventListener("abort", onAbort);
    xhr.send(form);
  });
}

async function stageBrowserFileAt(file: File, endpoint: string, opts?: StageOptions): Promise<BrowserPkgUpload> {
  const form = new FormData();
  form.append(endpoint === "/api/pkg/upload" ? "pkg" : "file", file, file.name);
  const query = new URLSearchParams();
  if (opts?.uploadId) query.set("upload_id", opts.uploadId);
  if (opts?.relPath) query.set("relpath", opts.relPath);
  const qs = query.toString();
  const response = await send(`${getEngineUrl()}${endpoint}${qs ? `?${qs}` : ""}`, form, opts);
  if (!response.ok) throw new Error(await responseError(response));
  const body = (await response.json()) as {
    upload_id?: unknown;
    path?: unknown;
    filename?: unknown;
    size?: unknown;
    root?: unknown;
  };
  if (typeof body.upload_id !== "string" || typeof body.path !== "string") {
    throw new Error("engine returned an invalid package-upload response");
  }
  return {
    uploadId: body.upload_id,
    path: body.path,
    filename:
      typeof body.filename === "string" ? body.filename : file.name,
    size: typeof body.size === "number" ? body.size : file.size,
    ...(typeof body.root === "string" ? { root: body.root } : {}),
  };
}

/** Idempotent cleanup for temporary browser staging. */
export async function deleteBrowserPkgUpload(uploadId: string): Promise<void> {
  const response = await fetch(
    `${getEngineUrl()}/api/pkg/upload/${encodeURIComponent(uploadId)}`,
    { method: "DELETE" },
  );
  if (!response.ok) throw new Error(await responseError(response));
}
