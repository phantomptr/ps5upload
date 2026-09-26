// Some consumers need the bytes on this computer: a payload to send, a save zip to unzip, an
// archive to inspect. For a pick from a saved server, copy it here first (a job with progress,
// shown in the activity bar) and hand back the local path. A local path passes through.

import { jobStatus } from "../api/ps5";
import { remoteApi } from "../api/remote";
import { useConnectionsStore } from "../state/connections";
import { trackTask } from "../state/trackTask";
import { isRemotePath, parseRemotePath } from "./remotePath";

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
    async (report) => {
      const { job_id } = await remoteApi.fetch(path, opts.destDir);
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
        if (s.total_bytes) {
          report({ progress: { current: s.bytes_sent ?? 0, total: s.total_bytes, unit: "bytes" } });
        }
        await new Promise((r) => setTimeout(r, pollMs));
      }
    },
  );
}
