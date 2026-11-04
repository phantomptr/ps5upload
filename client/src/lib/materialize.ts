// Some consumers need the bytes on this computer: a payload to send, a save zip to unzip, an
// archive to inspect. For a pick from a saved server, copy it here first (a job with progress,
// shown in the activity bar) and hand back the local path. A local path passes through.

import { jobStatus } from "../api/ps5";
import { remoteApi } from "../api/remote";
import { useConnectionsStore } from "../state/connections";
import { useTaskStore } from "../state/tasks";
import { isRemotePath, parseRemotePath } from "./remotePath";
import { readWithRetries } from "./pollRead";
import { trStatic } from "./trStatic";

/** Local copies this app made of server files, so only those are ever cleaned up. */
const copies = new Set<string>();
/** The local copy of a server file, while it is still ours (a later upload of it reuses it). */
const copyOfRemote = new Map<string, string>();

/** A copy already made of this server file, or null. */
export function existingCopyOf(remotePath: string): string | null {
  const local = copyOfRemote.get(remotePath);
  return local && copies.has(local) ? local : null;
}

/** Remove a copy made by `materializeRemote` once nothing needs it. Anything else is left alone. */
export async function releaseCopy(path: string): Promise<void> {
  if (!copies.delete(path)) return;
  for (const [remote, local] of copyOfRemote) if (local === path) copyOfRemote.delete(remote);
  await remoteApi.cleanupFetched(path).catch(() => {});
}

/** Run `work` on a local copy of `path` (the path itself when local), then remove the copy. */
export async function withLocalCopy<T>(
  path: string,
  work: (local: string) => Promise<T>,
  opts: { destDir?: string; pollMs?: number } = {},
): Promise<T> {
  const local = await materializeRemote(path, opts);
  try {
    return await work(local);
  } finally {
    await releaseCopy(local);
  }
}

/** Run the engine's copy job for `path` and resolve with the local copy. No task of its own:
 *  the caller shows the progress (Convert, as its first stage). */
export async function fetchRemote(
  path: string,
  opts: {
    destDir?: string;
    pollMs?: number;
    onJob?: (jobId: string) => void;
    onProgress?: (done: number, total: number) => void;
  } = {},
): Promise<string> {
  const { job_id } = await remoteApi.fetch(path, opts.destDir);
  opts.onJob?.(job_id);
  for (;;) {
    const s = (await readWithRetries(() => jobStatus(job_id))) as {
      status: string;
      dest?: string;
      error?: string;
      bytes_sent?: number;
      total_bytes?: number;
    };
    if (s.status === "done") return s.dest ?? "";
    if (s.status === "failed") throw new Error(s.error ?? "The copy did not finish.");
    if (s.total_bytes) opts.onProgress?.(s.bytes_sent ?? 0, s.total_bytes);
    await new Promise((r) => setTimeout(r, opts.pollMs ?? 500));
  }
}

export async function materializeRemote(
  path: string,
  opts: { destDir?: string; pollMs?: number } = {},
): Promise<string> {
  if (!isRemotePath(path)) return path;
  const parsed = parseRemotePath(path);
  const name = parsed?.path.split("/").filter(Boolean).pop() ?? "file";
  const server = parsed && useConnectionsStore.getState().nameOf(parsed.connectionId);
  const pollMs = opts.pollMs ?? 500;
  const tasks = useTaskStore.getState();
  const id = tasks.registerTask({
    kind: "download",
    origin: "remote.fetch",
    label: (server
      ? trStatic("remote_copy_task", "Copy {name} from {server}").replace("{server}", () => server)
      : trStatic("remote_copy_task_unknown_server", "Copy {name} from the server")
    ).replace("{name}", () => name),
    consoleId: "",
  });
  let local: string;
  try {
    local = await fetchRemote(path, {
      destDir: opts.destDir,
      pollMs,
      // Known once the engine took the job: from then on Activity can cancel it.
      onJob: (jobId) =>
        useTaskStore.getState().updateTask(id, { engineJobId: jobId, control: { owner: "remote-fetch", jobId } }),
      onProgress: (current, total) =>
        useTaskStore.getState().updateTask(id, { progress: { current, total, unit: "bytes" } }),
    });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    // The engine ends a cancelled fetch with the error "cancelled" (remote/fetch.rs copy_all).
    if (message === "cancelled") useTaskStore.getState().finishTask(id, "cancelled");
    else
      useTaskStore.getState().finishTask(id, "failed", {
        lastError: { code: "OP_FAILED", message, recoverable: false },
      });
    throw error;
  }
  useTaskStore.getState().finishTask(id, "done");
  copies.add(local);
  return local;
}
