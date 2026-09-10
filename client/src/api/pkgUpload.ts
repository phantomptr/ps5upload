import { getEngineUrl } from "../state/engine";

export interface BrowserPkgUpload {
  uploadId: string;
  path: string;
  filename: string;
  size: number;
}

async function responseError(response: Response): Promise<string> {
  const body = (await response.json().catch(() => null)) as {
    error?: unknown;
  } | null;
  return body?.error ? String(body.error) : `engine HTTP ${response.status}`;
}

/**
 * Put a browser-selected package on the engine host. Browsers cannot hand the
 * engine a local filesystem path, while the desktop shell can. The returned
 * path is therefore temporary and must be paired with `deleteBrowserPkgUpload`
 * after both the direct Stream attempt and any staged fallback have finished.
 */
export async function stageBrowserPkg(file: File): Promise<BrowserPkgUpload> {
  const form = new FormData();
  form.append("pkg", file, file.name);
  const response = await fetch(`${getEngineUrl()}/api/pkg/upload`, {
    method: "POST",
    body: form,
  });
  if (!response.ok) throw new Error(await responseError(response));
  const body = (await response.json()) as {
    upload_id?: unknown;
    path?: unknown;
    filename?: unknown;
    size?: unknown;
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
