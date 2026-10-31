// Sharing a PS5 save with another account (engine save_backup.rs): the save's own files as a zip
// from one console, put into your own save of the same game on another, which the console then
// keeps as yours.

import { getEngineUrl } from "../state/engine";

async function failed(r: Response): Promise<never> {
  const b = (await r.json().catch(() => ({}))) as { error?: string };
  throw new Error(b.error || `HTTP ${r.status}`);
}

/** The save image's files as a zip, with the name to save it under. */
export async function shareSave(addr: string, image: string): Promise<{ blob: Blob; name: string }> {
  const r = await fetch(`${getEngineUrl()}/api/save-backup/share`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ addr, image }),
  });
  if (!r.ok) return failed(r);
  const cd = r.headers.get("content-disposition") ?? "";
  const name = /filename="([^"]+)"/.exec(cd)?.[1] ?? "shared save.zip";
  return { blob: await r.blob(), name };
}

/** Put a shared save's files into your own save `image` (backed up first, put back on failure). */
export async function putSharedSave(addr: string, image: string, userId: number, zip: Blob): Promise<void> {
  const q = new URLSearchParams({ addr, image, user_id: String(userId >>> 0) });
  const r = await fetch(`${getEngineUrl()}/api/save-backup/use-shared?${q}`, {
    method: "POST",
    headers: { "content-type": "application/zip" },
    body: zip,
  });
  if (!r.ok) return failed(r);
}
