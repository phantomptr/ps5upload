import {
  deleteBrowserPkgUpload,
  stageBrowserFile,
  type BrowserPkgUpload,
  type StageOptions,
} from "../api/pkgUpload";
import { noteStaged, runStagingTask } from "./browserStaging";
import { trStatic } from "./trStatic";

/**
 * Files and folders from the device the browser runs on (#730), for the web UI's Files view.
 *
 * A browser cannot hand the engine a path, so each file is uploaded to the engine's staging
 * area first and the engine-side path goes on into the normal copy to the PS5. A folder keeps
 * its shape: every file is sent with its place in the folder (`relpath`) into ONE upload, whose
 * root is then the folder the copy sends. Files go one at a time and are read by the browser
 * as they are sent, so a large folder is never held in memory. A failed or cancelled pick
 * removes every copy it made: half a folder is never sent on.
 */

export interface FolderFile {
  file: File;
  /** The file's path in the folder, its first name being the folder's (`Game/sce_sys/a`). */
  relPath: string;
}

/** One thing picked or dropped on this device. */
export type DevicePick =
  | { kind: "file"; file: File }
  | { kind: "folder"; name: string; files: FolderFile[] };

/** What `<input type="file">` gave: with `webkitdirectory` each file carries its path inside the
 *  picked folder (`webkitRelativePath`); without it, they are loose files. */
export function picksFromInput(files: readonly File[]): DevicePick[] {
  const out: DevicePick[] = [];
  const folders = new Map<string, FolderFile[]>();
  for (const file of files) {
    const rel = (file.webkitRelativePath ?? "").replace(/\\/g, "/").replace(/^\/+/, "");
    const slash = rel.indexOf("/");
    if (slash <= 0) {
      out.push({ kind: "file", file });
      continue;
    }
    const top = rel.slice(0, slash);
    let list = folders.get(top);
    if (!list) {
      list = [];
      folders.set(top, list);
      out.push({ kind: "folder", name: top, files: list });
    }
    list.push({ file, relPath: rel });
  }
  return out;
}

/** The parts of the File and Directory Entries API a drop uses (`webkitGetAsEntry`). */
export interface DropEntry {
  name: string;
  isFile: boolean;
  isDirectory: boolean;
  file?: (ok: (f: File) => void, fail?: (e: unknown) => void) => void;
  createReader?: () => {
    readEntries: (ok: (entries: DropEntry[]) => void, fail?: (e: unknown) => void) => void;
  };
}

/** One dropped item, taken while the drop event lasts (the list is emptied after it). */
export interface DroppedItem {
  entry: DropEntry | null;
  file: File | null;
}

/** Takes the dropped files and folders off a drop's `DataTransfer`, synchronously. */
export function droppedItems(dt: DataTransfer): DroppedItem[] {
  const items = Array.from(dt.items ?? []).filter((i) => i.kind === "file");
  if (items.length === 0) return Array.from(dt.files).map((file) => ({ entry: null, file }));
  return items.map((i) => ({
    entry: (i.webkitGetAsEntry?.() as DropEntry | null | undefined) ?? null,
    file: i.getAsFile(),
  }));
}

function entryFile(e: DropEntry): Promise<File> {
  return new Promise((ok, fail) => e.file?.(ok, fail) ?? fail(new Error("not a file")));
}

/** Every file under a dropped folder, with its path. `readEntries` answers in batches: it is
 *  called until it gives nothing more. */
async function walk(dir: DropEntry, prefix: string, out: FolderFile[]): Promise<void> {
  const reader = dir.createReader?.();
  if (!reader) return;
  for (;;) {
    const batch = await new Promise<DropEntry[]>((ok, fail) => reader.readEntries(ok, fail));
    if (batch.length === 0) return;
    for (const e of batch) {
      const rel = `${prefix}/${e.name}`;
      if (e.isDirectory) await walk(e, rel, out);
      else if (e.isFile) out.push({ file: await entryFile(e), relPath: rel });
    }
  }
}

/** A drop's items as picks: a folder is read through (recursively), a file is itself. */
export async function picksFromDrop(items: readonly DroppedItem[]): Promise<DevicePick[]> {
  const out: DevicePick[] = [];
  for (const { entry, file } of items) {
    if (entry?.isDirectory) {
      const files: FolderFile[] = [];
      await walk(entry, entry.name, files);
      out.push({ kind: "folder", name: entry.name, files });
    } else if (file) {
      out.push({ kind: "file", file });
    } else if (entry?.isFile) {
      out.push({ kind: "file", file: await entryFile(entry) });
    }
  }
  return out;
}

export const pickBytes = (p: DevicePick): number =>
  p.kind === "file" ? p.file.size : p.files.reduce((n, f) => n + f.file.size, 0);

export interface StagePicksDeps {
  stage: (file: File, opts?: StageOptions) => Promise<BrowserPkgUpload>;
  remove: (uploadId: string) => Promise<void>;
  note: (u: BrowserPkgUpload) => void;
}

const defaultDeps: StagePicksDeps = {
  stage: stageBrowserFile,
  remove: deleteBrowserPkgUpload,
  note: (u) => noteStaged(u),
};

/**
 * Uploads the picks to the engine, one file at a time, and gives back the engine-side paths
 * to copy on: a file's own path, a folder's root. All or nothing: on a failure or a cancel
 * every upload made so far is removed and the error is thrown on (`isStagingCancelled` tells
 * a cancel). An empty folder has nothing to send and is left out.
 */
export async function stagePicks(
  picks: readonly DevicePick[],
  signal: AbortSignal,
  progress: (sent: number, total: number) => void,
  deps: StagePicksDeps = defaultDeps,
): Promise<string[]> {
  const total = picks.reduce((n, p) => n + pickBytes(p), 0);
  let done = 0;
  const made: BrowserPkgUpload[] = [];
  const paths: string[] = [];
  const send = async (file: File, extra: Pick<StageOptions, "uploadId" | "relPath">) => {
    const staged = await deps.stage(file, {
      ...extra,
      signal,
      onProgress: (sent) => progress(done + sent, total),
    });
    done += file.size;
    progress(done, total);
    return staged;
  };
  try {
    for (const p of picks) {
      signal.throwIfAborted?.();
      if (p.kind === "file") {
        const staged = await send(p.file, {});
        made.push(staged);
        paths.push(staged.path);
        continue;
      }
      let folder: BrowserPkgUpload | null = null;
      for (const f of p.files) {
        signal.throwIfAborted?.();
        const staged = await send(f.file, { uploadId: folder?.uploadId, relPath: f.relPath });
        if (!folder) {
          folder = { ...staged, path: staged.root ?? staged.path };
          made.push(folder);
        }
      }
      if (folder) paths.push(folder.path);
    }
  } catch (e) {
    await Promise.allSettled(made.map((u) => deps.remove(u.uploadId)));
    throw e;
  }
  for (const u of made) deps.note(u);
  return paths;
}

/** `stagePicks` as one task with a percent and a Cancel. */
export function stagePicksWithTask(
  picks: readonly DevicePick[],
  deps: StagePicksDeps = defaultDeps,
): Promise<string[]> {
  const one = picks.length === 1 ? picks[0] : null;
  const label = one
    ? trStatic("browser_staging_task", "Uploading {name} to the server").replace(
        "{name}",
        one.kind === "file" ? one.file.name : one.name,
      )
    : trStatic("browser_staging_many", "Uploading {count} items to the server").replace(
        "{count}",
        String(picks.length),
      );
  return runStagingTask(
    label,
    picks.reduce((n, p) => n + pickBytes(p), 0),
    (signal, progress) => stagePicks(picks, signal, progress, deps),
  );
}
