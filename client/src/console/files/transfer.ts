/**
 * Files' writes and transfers that go past the JSON routes: saving a text
 * file (/api/fs/save-text, up to 1 MiB, atomic on the helper, refused when
 * the file changed since it was read), uploading from the device the page is
 * open on (/api/upload/begin + /api/upload/data, one file at a time, with
 * progress and cancel) and downloading to it (/api/download/begin hands out a
 * link the browser saves; a folder comes as a .tar).
 *
 * Upload and download are for a phone or PC on the LAN only: on the console
 * itself there is nothing to pick files from and nowhere to save them.
 */
import { ApiError, call, callRaw, sendWithProgress } from "../api";
import { isPs5Browser } from "../fullscreen";
import { onConsoleBrowser } from "../ui/PhoneHandoff";
import { joinPath } from "./move";

/** The most the editor saves (the helper's cap too). */
export const TEXT_SAVE_MAX = 1024 * 1024;

/** A file as the editor read it: a save is refused when it is not like this any more. */
export interface FileStamp {
  mtime: number;
  size: number;
  /** The mtime's nanoseconds (a helper that sends them): a same-second change is caught too. */
  mtime_ns?: number;
}

/**
 * Is the page open on another device (a phone or PC over the LAN), not the console's own browser?
 * `host` is location.hostname: the address the page was opened BY, which on a phone is the
 * console's own LAN address, so it cannot tell the two apart (it used to be compared with that
 * address, and Upload never showed on a phone). The console's browser is told by its user agent
 * ("PlayStation") or by opening the page at 127.0.0.1 (the home-screen tile).
 */
export function onOtherDevice(
  host: string = typeof location !== "undefined" ? location.hostname : "",
  ua: string = typeof navigator !== "undefined" ? navigator.userAgent : "",
): boolean {
  return host !== "" && !onConsoleBrowser(host) && !isPs5Browser(ua);
}

/** onOtherDevice for this page (a promise, as Files has always read it). */
export function loadOnOtherDevice(): Promise<boolean> {
  return Promise.resolve(onOtherDevice());
}

/** The bytes a save writes: the editor's text (a file that used CRLF keeps CRLF), or why it cannot be saved. */
export function encodeText(text: string, crlf: boolean): Uint8Array | "binary" | "too_large" {
  if (text.includes("\u0000")) return "binary";
  const out = new TextEncoder().encode(crlf ? text.replace(/\r?\n/g, "\r\n") : text);
  return out.length > TEXT_SAVE_MAX ? "too_large" : out;
}

/** Text the editor can open: valid UTF-8 without a NUL byte (a BOM is kept as it is). Else null. */
export function decodeEditable(buf: ArrayBuffer): string | null {
  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(buf);
  } catch {
    return null;
  }
  return text.includes("\u0000") ? null : text;
}

/** The save request: one line of JSON, a newline, then the bytes. */
export function saveBody(head: object, bytes: Uint8Array): Blob {
  return new Blob([`${JSON.stringify(head)}\n`, bytes as BlobPart]);
}

/** Save `bytes` as `path` if it is still as `stamp` says; the new stamp. 409 "changed" when it is not. */
export async function saveText(path: string, bytes: Uint8Array, stamp: FileStamp): Promise<FileStamp> {
  const head: Record<string, unknown> = { path, mtime: stamp.mtime, size: stamp.size };
  if (typeof stamp.mtime_ns === "number") head.mtime_ns = stamp.mtime_ns;
  const r = await callRaw<FileStamp>("/api/fs/save-text", saveBody(head, bytes));
  if (!r || typeof r.mtime !== "number" || typeof r.size !== "number") throw new ApiError(500, "bad_reply");
  return r;
}

/** A new, empty file (409 "exists" when anything has that name). */
export async function createFile(path: string): Promise<void> {
  await callRaw("/api/fs/save-text", saveBody({ path, create: true }, new Uint8Array(0)));
}

/** The file as it is now. */
export async function stampOf(path: string): Promise<FileStamp> {
  const s = await call<{ mtime?: number; size?: number; mtime_ns?: number }>("/api/fs/stat", { path });
  const out: FileStamp = { mtime: Number(s?.mtime ?? 0), size: Number(s?.size ?? 0) };
  if (typeof s?.mtime_ns === "number") out.mtime_ns = s.mtime_ns;
  return out;
}

/* ---- upload ---- */

/** One file to send: its path under the folder it goes into ("a.txt", or "Album/1.jpg" from a folder pick). */
export interface UploadItem {
  rel: string;
  file: File;
}

/** Picked files as upload items: a folder pick's relative paths (webkitRelativePath), else plain names. Bad parts dropped. */
export function uploadItems(files: ArrayLike<File>): UploadItem[] {
  const out: UploadItem[] = [];
  for (let i = 0; i < files.length; i++) {
    const f = files[i];
    const raw = (f as File & { webkitRelativePath?: string }).webkitRelativePath || f.name;
    const parts = raw.split("/").filter((p) => p && p !== "." && p !== "..");
    if (parts.length) out.push({ rel: parts.join("/"), file: f });
  }
  return out;
}

/** The folders a set of items needs, parents first ("Album", "Album/2024"). */
export function uploadFolders(items: UploadItem[]): string[] {
  const set = new Set<string>();
  for (const it of items) {
    const parts = it.rel.split("/");
    for (let i = 1; i < parts.length; i++) set.add(parts.slice(0, i).join("/"));
  }
  return Array.from(set).sort((a, b) => a.split("/").length - b.split("/").length || a.localeCompare(b));
}

/**
 * Send one file as `dest` (its full path on the console). `overwrite` replaces
 * a file there. The helper checks the folder, the name and the free space
 * before any byte moves (begin), then writes to a temporary file and renames
 * it at the end: a cancel or a dropped connection leaves nothing under `dest`.
 */
export async function uploadFile(
  dest: string,
  file: Blob,
  overwrite: boolean,
  onProgress?: (sent: number, total: number) => void,
  signal?: AbortSignal,
): Promise<void> {
  const begun = await call<{ id?: string }>("/api/upload/begin", { path: dest, size: file.size, overwrite });
  const id = begun?.id;
  if (typeof id !== "string") throw new ApiError(500, "bad_reply");
  try {
    await sendWithProgress(`/api/upload/data?id=${encodeURIComponent(id)}`, file, onProgress, signal);
  } catch (e) {
    // Aborted before the helper saw the body: forget the reservation (it would expire anyway).
    await call("/api/upload/cancel", { id }).catch(() => undefined);
    throw e;
  }
}

/** Make the folders an upload into `dir` needs (mkdir makes parents; one that is there is fine). */
export async function makeFolders(dir: string, folders: string[]): Promise<void> {
  for (const f of folders) {
    try {
      await call("/api/fs/mkdir", { path: joinPath(dir, f) });
    } catch (e) {
      if (!(e instanceof ApiError && e.status === 409)) throw e;
    }
  }
}

/* ---- download ---- */

/** A link the browser downloads `path` from (a folder as .tar), and the name it saves. */
export async function downloadLink(path: string): Promise<{ url: string; name: string }> {
  const r = await call<{ url?: string; name?: string }>("/api/download/begin", { path });
  if (!r || typeof r.url !== "string" || !r.url.startsWith("/dl/")) throw new ApiError(500, "bad_reply");
  return { url: r.url, name: typeof r.name === "string" ? r.name : "" };
}

/** Download `path` to this device: the browser's own download (progress, pause and resume there). */
export async function downloadItem(path: string, doc: Document = document): Promise<string> {
  const { url, name } = await downloadLink(path);
  const a = doc.createElement("a");
  a.href = url;
  a.download = name;
  a.rel = "noopener";
  a.style.display = "none";
  doc.body.appendChild(a);
  a.click();
  a.remove();
  return name;
}
