import type { Task, TaskKind, TaskStatus } from "../state/tasks";

/** Work that must not be cut short by the computer or the PS5 going to sleep: anything that
 *  moves data or installs. Quick lookups (artwork, a launch, a bug report) do not count. */
const AWAKE_KINDS: ReadonlySet<TaskKind> = new Set<TaskKind>([
  "upload-file",
  "upload-dir",
  "upload-archive",
  "download",
  "fs-copy",
  "fs-move",
  "pkg-install",
  "pkg-dpi-install",
  "install-batch",
  "backup-snapshot",
  "backup-restore",
  "save-backup",
  "save-restore",
  "fpkg-convert",
  "ffpfsc-compress",
  "backport-patch",
]);

/** A task that is doing that work right now (or is about to, or waits on the console). */
const LIVE: ReadonlySet<TaskStatus> = new Set<TaskStatus>([
  "queued",
  "running",
  "awaiting",
]);

function keepsAwake(t: Task): boolean {
  return AWAKE_KINDS.has(t.kind) && LIVE.has(t.status);
}

/** Whether this computer should stay awake: any upload, install, copy or build is live. */
export function anyAwakeWork(tasks: readonly Task[]): boolean {
  return tasks.some(keepsAwake);
}

/** The consoles that should stay awake: those a live upload, install or copy runs against.
 *  A build on this computer alone (no console) keeps only the computer awake. */
export function awakeConsoles(tasks: readonly Task[]): Set<string> {
  const hosts = new Set<string>();
  for (const t of tasks) {
    if (keepsAwake(t) && t.consoleId && t.consoleId !== "_")
      hosts.add(t.consoleId);
  }
  return hosts;
}
