import { create } from "zustand";

import { hostOf } from "../lib/addr";

/**
 * A speed test between this computer and a console, kept outside the screen.
 *
 * It sends a file of incompressible data to the PS5 with the ordinary upload route and reads
 * it back with the ordinary download route, so the numbers are what a real copy gets, not a
 * synthetic best case. The file is removed from both machines whichever way the run ends.
 */

/** Where the test file goes on the console (a folder the app owns). */
export const SPEED_TEST_DIR = "/data/ps5upload/tests";
export const SPEED_TEST_REMOTE = `${SPEED_TEST_DIR}/ps5upload-speedtest.bin`;

export type SpeedTestPhase =
  | "preparing"
  | "uploading"
  | "downloading"
  | "cleaning"
  | "done"
  | "failed"
  | "stopped";

export interface SpeedTestState {
  phase: SpeedTestPhase;
  sizeBytes: number;
  /** Progress of the running leg. */
  sent: number;
  total: number;
  /** Bytes per second; null until that leg has been measured. */
  uploadBps: number | null;
  downloadBps: number | null;
  error: string | null;
  failedLeg: "prepare" | "upload" | "download" | null;
  stopRequested: boolean;
  finishedAtMs: number | null;
}

interface Store {
  byHost: Record<string, SpeedTestState>;
}

export const useSpeedTestStore = create<Store>(() => ({ byHost: {} }));

export function speedTestFor(s: Store, host: string): SpeedTestState | null {
  return s.byHost[hostOf(host)] ?? null;
}

const RUNNING: SpeedTestPhase[] = [
  "preparing",
  "uploading",
  "downloading",
  "cleaning",
];
export const speedTestRunning = (s: SpeedTestState | null) =>
  !!s && RUNNING.includes(s.phase);

function put(host: string, patch: Partial<SpeedTestState>) {
  const key = hostOf(host);
  useSpeedTestStore.setState((s) => {
    const cur = s.byHost[key];
    if (!cur) return s;
    return { byHost: { ...s.byHost, [key]: { ...cur, ...patch } } };
  });
}

interface JobSnap {
  status: string;
  bytes_sent?: number;
  total_bytes?: number;
  elapsed_ms?: number;
  error?: string | null;
}

/** What the run needs from the app. Injected so it is testable without a console. */
export interface SpeedTestDeps {
  /** Makes the test file on this computer. */
  prepare: (
    sizeMib: number,
  ) => Promise<{ path: string; download_dir: string; bytes: number }>;
  mkdir: (host: string, path: string) => Promise<unknown>;
  startUpload: (src: string, dest: string, host: string) => Promise<string>;
  startDownload: (
    remote: string,
    localDir: string,
    host: string,
  ) => Promise<string>;
  jobStatus: (jobId: string, host: string) => Promise<JobSnap>;
  jobCancel: (jobId: string) => Promise<unknown>;
  deleteRemote: (host: string, path: string) => Promise<unknown>;
  /** Removes the test files from this computer. */
  cleanup: () => Promise<unknown>;
  sleep: (ms: number) => Promise<void>;
}

type Leg =
  { ok: true; bps: number } | { ok: false; stopped: boolean; error: string };

async function leg(
  host: string,
  jobId: string,
  deps: SpeedTestDeps,
  fallbackBytes: number,
): Promise<Leg> {
  const startedAt = Date.now();
  for (;;) {
    let snap: JobSnap;
    try {
      snap = await deps.jobStatus(jobId, host);
    } catch (e) {
      return {
        ok: false,
        stopped: false,
        error: e instanceof Error ? e.message : String(e),
      };
    }
    if (snap.status === "done") {
      const bytes = snap.total_bytes || snap.bytes_sent || fallbackBytes;
      // The engine's own timing where it gives one: this loop only looks every so often.
      const ms =
        snap.elapsed_ms && snap.elapsed_ms > 0
          ? snap.elapsed_ms
          : Date.now() - startedAt;
      return { ok: true, bps: ms > 0 ? (bytes * 1000) / ms : 0 };
    }
    if (snap.status === "failed") {
      return {
        ok: false,
        stopped: false,
        error: snap.error ?? "The transfer failed.",
      };
    }
    put(host, {
      sent: snap.bytes_sent ?? 0,
      total: snap.total_bytes ?? fallbackBytes,
    });
    await deps.sleep(250);
    if (speedTestFor(useSpeedTestStore.getState(), host)?.stopRequested) {
      await deps.jobCancel(jobId).catch(() => {});
      return { ok: false, stopped: true, error: "" };
    }
  }
}

/** Runs the test to its end. Resolves when it ends, whichever way; the outcome is in the
 *  store. A test already running for this console is left alone. */
export async function runSpeedTest(
  host: string,
  sizeMib: number,
  deps: SpeedTestDeps,
): Promise<void> {
  const key = hostOf(host);
  if (speedTestRunning(speedTestFor(useSpeedTestStore.getState(), host)))
    return;
  useSpeedTestStore.setState((s) => ({
    byHost: {
      ...s.byHost,
      [key]: {
        phase: "preparing",
        sizeBytes: sizeMib * 1024 * 1024,
        sent: 0,
        total: 0,
        uploadBps: null,
        downloadBps: null,
        error: null,
        failedLeg: null,
        stopRequested: false,
        finishedAtMs: null,
      },
    },
  }));

  let uploaded = false;
  const end = async (patch: Partial<SpeedTestState>) => {
    put(host, { phase: "cleaning", sent: 0, total: 0 });
    // Best effort on both sides: a leftover is also swept by Health's clean-up.
    if (uploaded)
      await deps.deleteRemote(host, SPEED_TEST_REMOTE).catch(() => {});
    await deps.cleanup().catch(() => {});
    put(host, { ...patch, finishedAtMs: Date.now() });
  };

  let file: { path: string; download_dir: string; bytes: number };
  try {
    file = await deps.prepare(sizeMib);
  } catch (e) {
    put(host, {
      phase: "failed",
      failedLeg: "prepare",
      error: e instanceof Error ? e.message : String(e),
      finishedAtMs: Date.now(),
    });
    return;
  }
  put(host, { sizeBytes: file.bytes });
  await deps.mkdir(host, SPEED_TEST_DIR).catch(() => {});

  put(host, { phase: "uploading", sent: 0, total: file.bytes });
  let up: Leg;
  try {
    // From here the file may be on the console, in whole or in part.
    uploaded = true;
    up = await leg(
      host,
      await deps.startUpload(file.path, SPEED_TEST_REMOTE, host),
      deps,
      file.bytes,
    );
  } catch (e) {
    up = {
      ok: false,
      stopped: false,
      error: e instanceof Error ? e.message : String(e),
    };
  }
  if (!up.ok) {
    return end(
      up.stopped
        ? { phase: "stopped" }
        : { phase: "failed", failedLeg: "upload", error: up.error },
    );
  }
  put(host, { uploadBps: up.bps });

  put(host, { phase: "downloading", sent: 0, total: file.bytes });
  let down: Leg;
  try {
    down = await leg(
      host,
      await deps.startDownload(SPEED_TEST_REMOTE, file.download_dir, host),
      deps,
      file.bytes,
    );
  } catch (e) {
    down = {
      ok: false,
      stopped: false,
      error: e instanceof Error ? e.message : String(e),
    };
  }
  if (!down.ok) {
    return end(
      down.stopped
        ? { phase: "stopped" }
        : { phase: "failed", failedLeg: "download", error: down.error },
    );
  }
  return end({ phase: "done", downloadBps: down.bps });
}

/** Asks a running test to stop. It cancels its transfer and cleans up after itself. */
export function stopSpeedTest(host: string): void {
  if (speedTestRunning(speedTestFor(useSpeedTestStore.getState(), host))) {
    put(host, { stopRequested: true });
  }
}
