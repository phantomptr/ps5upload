// Some consumers need the bytes on this computer: a payload to send, a save zip to unzip, an
// archive to inspect. For a pick from a saved server, copy it here first (a job with progress,
// shown in the activity bar) and hand back the local path. A local path passes through.

import { jobStatus } from "../api/ps5";
import { remoteApi } from "../api/remote";
import { useConnectionsStore } from "../state/connections";
import { trackTask } from "../state/trackTask";
import { isRemotePath, parseRemotePath } from "./remotePath";

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
    const s = (await jobStatus(job_id)) as {
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
  const server =
    (parsed && useConnectionsStore.getState().nameOf(parsed.connectionId)) ?? "the server";
  const pollMs = opts.pollMs ?? 500;
  return trackTask(
    { kind: "download", origin: "remote.fetch", label: `Copy ${name} from ${server}` },
    (report) =>
      fetchRemote(path, {
        destDir: opts.destDir,
        pollMs,
        onProgress: (current, total) =>
          report({ progress: { current, total, unit: "bytes" } }),
      }),
  );
}
