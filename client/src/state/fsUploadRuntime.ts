import {
  fsDelete,
  fsListDir,
  generateTxIdHex,
  jobCancel,
  jobStatus,
  pathKind,
  startTransferDir,
  startTransferFile,
  startTransferFileList,
} from "../api/ps5";
import { fetchJobFiles } from "../lib/jobFiles";
import { isAutoRecoverable } from "../lib/uploadRecovery";
import {
  fsUploadForHost,
  resumeFsUpload,
  useFsUploadStore,
  type FsUploadDeps,
} from "./fsUpload";
import { useUploadSettingsStore } from "./uploadSettings";
import { useConnectionStore } from "./connection";
import { hostOf } from "../lib/addr";
import { isRestModeReason } from "../lib/consolePower";

/** Whether the poller last saw this console in rest mode. */
const consoleAsleep = (host: string) =>
  useConnectionStore.getState().runtimeByHost[hostOf(host) || "_"]?.power === "rest";

/** Whether a failure is one the app may try again unasked: the user's auto-resume setting,
 *  and the same policy the upload queue uses (a dropped connection yes, a full drive no). */
const mayRetry = (reason: string | null | undefined, error: string) =>
  useUploadSettingsStore.getState().autoResume &&
  isAutoRecoverable(reason, error);

/** The Files screen's upload runner, wired to the real engine (see state/fsUpload). */
export const fsUploadDeps: FsUploadDeps = {
  pathKind,
  startFile: (src, dest, addr, cap, txId) =>
    startTransferFile(src, dest, addr, txId ?? null, cap),
  startDir: (src, dest, addr, cap, txId) =>
    startTransferDir(src, dest, addr, txId ?? null, undefined, cap),
  newTxId: generateTxIdHex,
  // Two or more files of a pick: one job for all of them (see state/fsUploadPlan).
  startList: (destRoot, files, addr, txId, cap) =>
    startTransferFileList(destRoot, files, addr, txId, cap),
  listDir: (addr, dir) => fsListDir(addr, dir),
  deleteRemote: (addr, path) => fsDelete(addr, path),
  jobFiles: (id) => fetchJobFiles(id),
  // The same limit the upload queue passes (set in Settings or on the Upload screen).
  bandwidthCapMbps: () => useUploadSettingsStore.getState().bandwidthCapMbps,
  jobStatus: (id) => jobStatus(id),
  jobCancel,
  sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
  // In rest mode a retry cannot land: stop now, and the wake resumes it
  // (resumeStoppedFsUploadOnWake) without the retries spent on a sleeping console.
  shouldRetry: (reason, error, host) =>
    !isRestModeReason(reason) && !consoleAsleep(host) && mayRetry(reason, error),
};

/** The console answers again after an outage: carry on with a Files upload that the outage
 *  stopped. Returns whether one was started. Does nothing for a stop a retry cannot fix (a
 *  full drive), or when auto-resume is off: those wait for the Resume button. */
export function resumeStoppedFsUploadOnWake(host: string): boolean {
  const cur = fsUploadForHost(useFsUploadStore.getState(), host);
  if (!cur.stopped || cur.active) return false;
  // Only a failure: the user's own Stop, and a run the app closed on, wait for the button.
  if (cur.stopped.why !== "failed") return false;
  if (!mayRetry(cur.stopped.reason, cur.stopped.error)) return false;
  void resumeFsUpload(host, fsUploadDeps);
  return true;
}

/** The consoles that have a run the app closed or crashed in the middle of. */
export function interruptedFsUploadHosts(): string[] {
  return Object.entries(useFsUploadStore.getState().byHost)
    .filter(([, st]) => st.stopped?.why === "interrupted")
    .map(([host]) => host);
}
