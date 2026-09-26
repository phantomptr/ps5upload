// Convert to FPKG as one pipeline: an engine build job, then (for Convert & install) the stream
// install of the package it wrote. The screen renders `pipeline`; every action here checks the
// phase it is allowed from, so a stray click cannot act on the wrong package or reset a run.

import { create } from "zustand";

import { fpkg, type FpkgBuildRequest } from "../api/fpkg";
import { jobCancel, jobStatus } from "../api/ps5";
import { pushNotification } from "./notifications";
import { pkgLibraryStore } from "./pkgLibrary";

export type PipelineStage =
  | "check"
  | "plan"
  | "compress"
  | "write"
  | "verify"
  | "send"
  | "install";

/** What a run does: a package, a package then its install, an install of a kept package, or
 *  a compressed .ffpfsc image. */
export type PipelineMode = "convert" | "convert-install" | "install" | "ffpfsc";

type StageMs = Partial<Record<PipelineStage, number>>;

export type Pipeline =
  | { phase: "idle" }
  | {
      phase: "running";
      mode: PipelineMode;
      source: string;
      host: string | null;
      stage: PipelineStage;
      stageDone: number;
      stageTotal: number;
      startedMs: number;
      stageStartedMs: number;
      stageMs: StageMs;
      jobId: string | null;
      installTaskId: string | null;
      packagePath: string | null;
    }
  | {
      phase: "done";
      mode: PipelineMode;
      source: string;
      host: string | null;
      packagePath: string;
      packageBytes: number;
      convertMs: number;
      installMs: number;
      stageMs: StageMs;
      deleted: boolean;
    }
  | {
      phase: "failed";
      mode: PipelineMode;
      source: string;
      host: string | null;
      stage: PipelineStage;
      message: string;
      packagePath: string | null;
      stageMs: StageMs;
    };

export interface ConversionState {
  pipeline: Pipeline;
  start: (req: FpkgBuildRequest, opts: { install: boolean; host: string | null }) => Promise<void>;
  /** Compress an .exfat / .ffpkg image into a .ffpfsc. */
  compress: (source: string, outputDir?: string) => Promise<void>;
  /** Install the kept package again: after a failed install, or Install again on a result. */
  retryInstall: (host: string) => Promise<void>;
  cancel: () => Promise<void>;
  /** A new source: clears a finished result; ignored while running. */
  reset: () => void;
  deletePackage: () => Promise<void>;
}

/** How often a build job is polled (ms). */
export const POLL_MS = 500;
/** Consecutive failed polls before the run is declared lost. */
const MAX_POLL_FAILURES = 5;
const BUILD_STAGES: readonly PipelineStage[] = ["check", "plan", "compress", "write", "verify"];

type Running = Extract<Pipeline, { phase: "running" }>;

function running(): Running | null {
  const p = useFpkgConversion.getState().pipeline;
  return p.phase === "running" ? p : null;
}

function update(patch: Partial<Running>) {
  const p = running();
  if (p) useFpkgConversion.setState({ pipeline: { ...p, ...patch } });
}

/** Move to `stage`, recording how long the previous one took. */
function enterStage(stage: PipelineStage, done = 0, total = 0) {
  const p = running();
  if (!p) return;
  if (p.stage === stage) {
    update({ stageDone: done, stageTotal: total });
    return;
  }
  const now = Date.now();
  update({
    stage,
    stageDone: done,
    stageTotal: total,
    stageStartedMs: now,
    stageMs: { ...p.stageMs, [p.stage]: now - p.stageStartedMs },
  });
}

function fail(stage: PipelineStage, message: string, packagePath: string | null) {
  const p = running();
  if (!p) return;
  const what = p.mode === "ffpfsc" ? "Compression" : p.mode === "convert" ? "FPKG conversion" : "Convert & install";
  useFpkgConversion.setState({
    pipeline: {
      phase: "failed",
      mode: p.mode,
      source: p.source,
      host: p.host,
      stage,
      message,
      packagePath,
      stageMs: { ...p.stageMs, [p.stage]: Date.now() - p.stageStartedMs },
    },
  });
  pushNotification("error", `${what} failed`, { body: message, link: "/convert" });
}

/** A build that ends the run (Convert only, or a .ffpfsc image). */
function finish(packagePath: string, packageBytes: number, convertMs: number) {
  const p = running();
  if (!p) return;
  const now = Date.now();
  useFpkgConversion.setState({
    pipeline: {
      phase: "done",
      mode: p.mode,
      source: p.source,
      host: p.host,
      packagePath,
      packageBytes,
      convertMs,
      installMs: 0,
      stageMs: { ...p.stageMs, [p.stage]: now - p.stageStartedMs },
      deleted: false,
    },
  });
}

async function runInstall(packagePath: string, host: string | null) {
  const p = running();
  if (!p) return;
  if (!host) {
    fail("send", "Connect to a PS5 to install.", packagePath);
    return;
  }
  enterStage("send");
  const startedMs = Date.now();
  const r = await pkgLibraryStore(host)
    .getState()
    .installStream(packagePath, host, { onTask: (id) => update({ installTaskId: id }) });
  if (!running()) return;
  if (r.ok) {
    const cur = running()!;
    const convertMs = cur.mode === "install" ? 0 : startedMs - cur.startedMs;
    installDone(packagePath, convertMs, Date.now() - startedMs);
    pushNotification("success", "Installed on the PS5", { body: packagePath, link: "/convert" });
  } else {
    fail("install", r.message ?? "The install did not finish.", packagePath);
  }
}

function installDone(packagePath: string, convertMs: number, installMs: number) {
  const p = running();
  if (!p) return;
  const now = Date.now();
  useFpkgConversion.setState({
    pipeline: {
      phase: "done",
      mode: p.mode,
      source: p.source,
      host: p.host,
      packagePath,
      packageBytes: packageBytesOf(p),
      convertMs,
      installMs,
      stageMs: { ...p.stageMs, [p.stage]: now - p.stageStartedMs },
      deleted: false,
    },
  });
}

/** Bytes of the package being installed, remembered from the build (or the earlier result). */
const packageSizes = new Map<string, number>();
function packageBytesOf(p: Running): number {
  return (p.packagePath && packageSizes.get(p.packagePath)) || 0;
}

function poll(jobId: string, install: boolean, failures = 0) {
  setTimeout(async () => {
    const p = running();
    if (!p || p.jobId !== jobId) return;
    let snapshot;
    try {
      snapshot = await jobStatus(jobId);
    } catch {
      if (failures + 1 >= MAX_POLL_FAILURES) {
        fail(p.stage, "The engine stopped responding; the conversion did not finish.", null);
      } else {
        poll(jobId, install, failures + 1);
      }
      return;
    }
    if (running()?.jobId !== jobId) return;
    if (snapshot.status === "running") {
      const s = snapshot.stage;
      if (s && BUILD_STAGES.includes(s.id as PipelineStage)) {
        enterStage(s.id as PipelineStage, s.done, s.total);
      }
      poll(jobId, install);
      return;
    }
    if (snapshot.status === "done") {
      const path = snapshot.dest ?? "";
      const bytes = snapshot.bytes_sent ?? 0;
      packageSizes.set(path, bytes);
      const cur = running()!;
      update({ packagePath: path, jobId: null });
      if (install) {
        await runInstall(path, cur.host);
      } else {
        finish(path, bytes, Date.now() - cur.startedMs);
        pushNotification(
          "success",
          cur.mode === "ffpfsc" ? "Compression complete" : "FPKG conversion complete",
          { body: path, link: "/convert" },
        );
      }
      return;
    }
    fail(p.stage, snapshot.error ?? "The build failed.", null);
  }, POLL_MS);
}

function beginRun(mode: PipelineMode, source: string, host: string | null, stage: PipelineStage) {
  const now = Date.now();
  useFpkgConversion.setState({
    pipeline: {
      phase: "running",
      mode,
      source,
      host,
      stage,
      stageDone: 0,
      stageTotal: 0,
      startedMs: now,
      stageStartedMs: now,
      stageMs: {},
      jobId: null,
      installTaskId: null,
      packagePath: null,
    },
  });
}

export const useFpkgConversion = create<ConversionState>((set, get) => ({
  pipeline: { phase: "idle" },

  start: async (req, { install, host }) => {
    if (get().pipeline.phase === "running") return;
    beginRun(install ? "convert-install" : "convert", req.source, host, "check");
    try {
      const { job_id } = await fpkg.build(req);
      update({ jobId: job_id });
      poll(job_id, install);
    } catch (error) {
      fail("check", error instanceof Error ? error.message : String(error), null);
    }
  },

  compress: async (source, outputDir) => {
    if (get().pipeline.phase === "running") return;
    beginRun("ffpfsc", source, null, "compress");
    try {
      const { job_id } = await fpkg.compress(source, outputDir);
      update({ jobId: job_id });
      poll(job_id, false);
    } catch (error) {
      fail("compress", error instanceof Error ? error.message : String(error), null);
    }
  },

  retryInstall: async (host) => {
    const p = get().pipeline;
    const path =
      p.phase === "failed" ? p.packagePath : p.phase === "done" && !p.deleted ? p.packagePath : null;
    if (!path || (p.phase !== "failed" && p.phase !== "done")) return;
    if (p.phase === "done" && p.mode === "ffpfsc") return;
    beginRun("install", p.source, host, "send");
    update({ packagePath: path });
    await runInstall(path, host);
  },

  cancel: async () => {
    const p = running();
    if (p?.jobId) await jobCancel(p.jobId);
  },

  reset: () => {
    const phase = get().pipeline.phase;
    if (phase === "done" || phase === "failed") set({ pipeline: { phase: "idle" } });
  },

  deletePackage: async () => {
    const p = get().pipeline;
    if (p.phase !== "done" || p.deleted) return;
    await fpkg.deletePackage(p.packagePath);
    set({ pipeline: { ...p, deleted: true } });
  },
}));
