import {
  fsDelete,
  fsMkdir,
  jobCancel,
  jobStatus,
  startTransferDownload,
  startTransferFile,
} from "../api/ps5";
import { consoleAddr, transferAddr } from "../lib/addr";
import { invoke } from "../lib/invokeLogged";
import { runSpeedTest, type SpeedTestDeps } from "./speedTest";

/** The speed test's real dependencies: the engine's file maker and the ordinary transfer
 *  routes (so the figures are a real copy's). */
export const speedTestDeps: SpeedTestDeps = {
  prepare: (sizeMib) =>
    invoke<{ path: string; download_dir: string; bytes: number }>(
      "speed_test_prepare",
      {
        sizeMib,
      },
    ),
  mkdir: (host, path) => fsMkdir(transferAddr(host), path),
  startUpload: (src, dest, host) =>
    startTransferFile(src, dest, consoleAddr(host)),
  startDownload: (remote, dir, host) =>
    startTransferDownload(remote, dir, consoleAddr(host), "file"),
  jobStatus: (id, host) => jobStatus(id, host),
  jobCancel,
  deleteRemote: (host, path) => fsDelete(transferAddr(host), path),
  cleanup: () => invoke("speed_test_cleanup", {}),
  sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
};

/** Starts a speed test against `host` (does nothing if one is running). */
export function startSpeedTest(host: string, sizeMib: number): Promise<void> {
  return runSpeedTest(host, sizeMib, speedTestDeps);
}
