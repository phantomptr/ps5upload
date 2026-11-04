import {
  deleteBrowserPkgUpload,
  type BrowserPkgUpload,
  type StageOptions,
} from "../api/pkgUpload";
import { fsUploadForHost, useFsUploadStore } from "../state/fsUpload";
import { useFsUploadQueue } from "../state/fsUploadQueue";
import { useTaskStore } from "../state/tasks";
import { useUploadQueueStore } from "../state/uploadQueue";
import { safeGetItem, safeSetItem } from "./safeStorage";
import { trStatic } from "./trStatic";

/**
 * Files staged from a browser: a copy on the engine's disk that only exists to be sent on.
 *
 * Staging a large file can take hours, so it is a task with a percent and a Cancel. And the copy
 * goes when its work has: the Upload queue and the Files screen hold nothing but the path, and
 * without this a 60 GB package stayed in the container for a week, on top of the next upload's room.
 * The engine still sweeps what a closed browser leaves behind (`cleanup_stale_pkg_uploads`).
 */

/** A staging upload the task's Cancel can reach. */
const running = new Map<string, AbortController>();

export function cancelStaging(id: string): boolean {
  const c = running.get(id);
  c?.abort();
  return !!c;
}

export const isStagingCancelled = (e: unknown): boolean =>
  e instanceof DOMException && e.name === "AbortError";

type Stage = (file: File, opts?: StageOptions) => Promise<BrowserPkgUpload>;

/** Sends one file to the engine as a task; rejects when it fails or is cancelled
 *  (`isStagingCancelled`). `onProgress` is for a picker that also shows it. */
export async function stageWithTask(
  file: File,
  stage: Stage,
  onProgress?: (sent: number, total: number) => void,
  outerSignal?: AbortSignal,
): Promise<BrowserPkgUpload> {
  const tasks = useTaskStore.getState();
  const controller = new AbortController();
  outerSignal?.addEventListener("abort", () => controller.abort());
  const stagingId = `staging-${Date.now()}-${Math.random().toString(36).slice(2)}`;
  const taskId = tasks.registerTask({
    kind: "upload-file",
    origin: "browser.staging",
    label: trStatic(
      "browser_staging_task",
      "Uploading {name} to the server",
    ).replace("{name}", file.name),
    consoleId: "",
    control: { owner: "browser-staging", stagingId },
    status: "running",
    progress: { unit: "bytes", current: 0, total: file.size },
  });
  running.set(stagingId, controller);
  try {
    const staged = await stage(file, {
      signal: controller.signal,
      onProgress: (sent, total) => {
        useTaskStore
          .getState()
          .updateTask(taskId, {
            progress: { unit: "bytes", current: sent, total },
          });
        onProgress?.(sent, total);
      },
    });
    useTaskStore.getState().finishTask(taskId, "done");
    return staged;
  } catch (e) {
    if (isStagingCancelled(e))
      useTaskStore.getState().finishTask(taskId, "cancelled");
    else {
      useTaskStore.getState().finishTask(taskId, "failed", {
        lastError: {
          code: "staging",
          message: e instanceof Error ? e.message : String(e),
          recoverable: false,
        },
      });
    }
    throw e;
  } finally {
    running.delete(stagingId);
  }
}

// ---- deleting the copy when its work is over ----

const KEY = "ps5upload.browser_staged.v1";
/** Longer than the engine's own sweep (a week): by then the entry names nothing. */
const FORGET_AFTER_MS = 8 * 24 * 60 * 60 * 1000;

interface Staged {
  uploadId: string;
  at: number;
  /** Some piece of work has held this path: its leaving means the copy can go. Until then it may
   *  still be in the review, on its way to a queue. */
  held: boolean;
}

function load(): Record<string, Staged> {
  try {
    const v = JSON.parse(safeGetItem(KEY) ?? "{}") as Record<string, Staged>;
    return v && typeof v === "object" ? v : {};
  } catch {
    return {};
  }
}

/** Remembers a staged copy, by the path the rest of the app knows it by. */
export function noteStaged(upload: BrowserPkgUpload, now = Date.now()): void {
  const all = load();
  all[upload.path] = { uploadId: upload.uploadId, at: now, held: false };
  safeSetItem(KEY, JSON.stringify(all));
}

/** The paths work still needs: the queue's items that are not done, the Files screen's waiting and
 *  running picks, and a stopped run that can be resumed. */
export function pathsInUse(): Set<string> {
  const out = new Set<string>();
  for (const it of useUploadQueueStore.getState().items) {
    if (it.status !== "done") out.add(it.sourcePath);
  }
  for (const slot of Object.values(useFsUploadQueue.getState().byHost)) {
    for (const item of slot.items) {
      for (const src of item.request.srcPaths ?? []) out.add(src);
      for (const step of item.request.steps ?? []) {
        if (step.kind === "one") out.add(step.src);
        else for (const f of step.files) out.add(f.src);
      }
    }
  }
  const runs = useFsUploadStore.getState();
  for (const host of Object.keys(runs.byHost)) {
    for (const src of fsUploadForHost(runs, host).stopped?.srcPaths ?? [])
      out.add(src);
  }
  return out;
}

/** One pass: which copies have stopped being needed. Pure, for the test. */
export function planRelease(
  all: Record<string, Staged>,
  inUse: ReadonlySet<string>,
  now: number,
): { keep: Record<string, Staged>; release: string[] } {
  const keep: Record<string, Staged> = {};
  const release: string[] = [];
  for (const [path, s] of Object.entries(all)) {
    if (inUse.has(path)) keep[path] = { ...s, held: true };
    else if (s.held) release.push(s.uploadId);
    else if (now - s.at < FORGET_AFTER_MS) keep[path] = s;
  }
  return { keep, release };
}

function releasePass(): void {
  const all = load();
  if (Object.keys(all).length === 0) return;
  const { keep, release } = planRelease(all, pathsInUse(), Date.now());
  if (JSON.stringify(keep) !== JSON.stringify(all))
    safeSetItem(KEY, JSON.stringify(keep));
  for (const id of release) void deleteBrowserPkgUpload(id).catch(() => {});
}

/** Watches the work and deletes the copies it no longer needs. Returns a stop. */
export function startStagedRelease(): () => void {
  let timer: ReturnType<typeof setTimeout> | null = null;
  // A pick moves between stores as it runs: look once things have settled, not in between.
  const soon = () => {
    if (!timer) {
      timer = setTimeout(() => {
        timer = null;
        releasePass();
      }, 2000);
    }
  };
  const stops = [
    useUploadQueueStore.subscribe((s, prev) => {
      if (s.items !== prev.items) soon();
    }),
    useFsUploadQueue.subscribe(soon),
    useFsUploadStore.subscribe(soon),
  ];
  soon();
  return () => {
    if (timer) clearTimeout(timer);
    for (const stop of stops) stop();
  };
}
