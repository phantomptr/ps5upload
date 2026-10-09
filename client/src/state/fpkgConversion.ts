// Convert to FPKG as one pipeline: an engine build job, then (for Convert & install) the install
// of the package it wrote — streamed from this computer, or uploaded to the PS5 first. The screen renders `pipeline`; every action here checks the
// phase it is allowed from, so a stray click cannot act on the wrong package or reset a run.

import { installErrorLink } from "../lib/installErrorDoc";
import { create } from "zustand";

import { imageUploadItem, type ImageUploadPlan } from "../lib/imageUpload";
import { useUploadQueueStore } from "./uploadQueue";

import { fpkg, type FpkgBuildRequest, type ImageFormat } from "../api/fpkg";
import { jobCancel, jobStatus } from "../api/ps5";
import { useConnectionStore } from "./connection";
import { pushNotification } from "./notifications";
import { pkgLibraryStore, type PkgLibraryStore } from "./pkgLibrary";
import { useTaskStore } from "./tasks";
import { remoteApi } from "../api/remote";
import { fetchRemote } from "../lib/materialize";
import { isRemotePath } from "../lib/remotePath";
import { finishSwap, runSwap, type SwapJournal } from "../lib/dumpSwap";
import { consoleSwapDeps } from "../lib/dumpSwapConsole";

export type PipelineStage =
  | "copy"
  | "extract"
  | "check"
  | "plan"
  | "compress"
  | "write"
  | "verify"
  | "park"
  | "send"
  | "install";

/** What a run does: a package, a package then its install, an install of a kept package, a
 *  compressed .ffpfsc image from an image, or a game image (.exfat) from a folder. */
export type PipelineMode = "convert" | "convert-install" | "install" | "ffpfsc" | "image";

/** A run whose result is a game image to mount, not a package to install. */
export const makesImage = (mode: PipelineMode) => mode === "ffpfsc" || mode === "image";

/** How a built package reaches the console: streamed from this computer over HTTP (nothing
 *  staged), or uploaded to PS5 staging and installed from there (never needs the console to
 *  reach this computer). */
export type InstallMethod = "stream" | "upload";

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
      /** This run's row in the activity bar; null for a re-install, which the install's own
       *  task already shows. */
      taskId: string | null;
      packagePath: string | null;
      /** The package's title id, from the build's content id (what Launch starts). */
      titleId: string | null;
      /** A server source copied to this computer for the build; removed once it succeeds. */
      copiedSource: string | null;
      /** The game unpacked from an archive (inside an engine unpack folder); removed with it
       *  once the run is over. */
      extractedSource: string | null;
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
      titleId: string | null;
      /** A console dump swapped for this package, awaiting Delete the old dump / Keep it
       *  parked; null once that is chosen (or when nothing was swapped). */
      swap?: SwapJournal | null;
      /** An image put in the Upload queue (by the run's own plan, or Upload to PS5). */
      uploadQueued?: boolean;
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
      titleId: string | null;
    };

export interface ConversionState {
  pipeline: Pipeline;
  start: (
    req: FpkgBuildRequest,
    opts: {
      install: boolean;
      host: string | null;
      method?: InstallMethod;
      /** An archive's password (RAR); never stored. */
      password?: string;
    },
  ) => Promise<void>;
  /** Compress an .exfat / .ffpkg image into a .ffpfsc. */
  compress: (source: string, outputDir?: string) => Promise<void>;
  /** Write a game folder as one game image (`format`, .exfat by default); with
   *  `thenCompress`, straight into a .ffpfsc (no uncompressed copy is written). */
  buildImage: (
    source: string,
    outputDir: string | undefined,
    thenCompress: boolean,
    format?: ImageFormat,
    /** Once the image is built and checked, put it in the Upload queue. */
    thenUpload?: ImageUploadPlan,
  ) => Promise<void>;
  /** Put the finished image in the Upload queue. */
  uploadImage: (plan: ImageUploadPlan) => void;
  /** Install the kept package again: after a failed install, or Install again on a result. */
  retryInstall: (host: string, method?: InstallMethod) => Promise<void>;
  cancel: () => Promise<void>;
  /** A new source: clears a finished result; ignored while running. */
  reset: () => void;
  deletePackage: () => Promise<void>;
  /** After a swap: delete the parked dump, or keep it parked outside the scan roots. */
  finishReplace: (choice: "delete" | "keep") => Promise<void>;
}

/** How often a build job is polled (ms). */
export const POLL_MS = 500;
/** Consecutive failed polls before the run is declared lost. */
const MAX_POLL_FAILURES = 5;
const BUILD_STAGES: readonly PipelineStage[] = ["check", "plan", "compress", "write", "verify"];

type Running = Extract<Pipeline, { phase: "running" }>;

/** The stage names Convert's progress card shows. */
const STAGE_LABEL: Record<PipelineStage, string> = {
  copy: "Copy from server",
  extract: "Unpack archive",
  check: "Check source",
  plan: "Plan package",
  compress: "Compress",
  write: "Write package",
  verify: "Verify",
  park: "Set the dump aside",
  send: "Send to PS5",
  install: "Install on PS5",
};

function baseName(path: string): string {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
}

/** A cancelled build ends as an error saying so. */
function isCancelMessage(message: string): boolean {
  return /\bcancel(l)?ed\b/i.test(message);
}

function running(): Running | null {
  const p = useFpkgConversion.getState().pipeline;
  return p.phase === "running" ? p : null;
}

function update(patch: Partial<Running>) {
  const p = running();
  if (p) useFpkgConversion.setState({ pipeline: { ...p, ...patch } });
}

function reportStage(taskId: string | null, stage: PipelineStage, done: number, total: number) {
  if (!taskId) return;
  useTaskStore.getState().updateTask(taskId, {
    stage: STAGE_LABEL[stage],
    progress: total > 0 ? { current: done, total, unit: "bytes" } : undefined,
  });
}

/** Move to `stage`, recording how long the previous one took. */
function enterStage(stage: PipelineStage, done = 0, total = 0) {
  const p = running();
  if (!p) return;
  reportStage(p.taskId, stage, done, total);
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
  dropCopy(p);
  if (p.taskId) {
    if (isCancelMessage(message)) useTaskStore.getState().finishTask(p.taskId, "cancelled");
    else
      useTaskStore.getState().finishTask(p.taskId, "failed", {
        lastError: { code: "FPKG_FAILED", message, recoverable: false },
      });
  }
  const what =
    p.mode === "ffpfsc"
      ? "Compression"
      : p.mode === "image"
        ? "Making the game image"
        : p.mode === "convert"
          ? "FPKG conversion"
          : "Convert & install";
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
      titleId: p.titleId,
    },
  });
  // An install-stage failure links to the install-route matrix entry for its
  // error; a build failure has no such page, so it goes back to Convert.
  pushNotification("error", `${what} failed`, {
    body: message,
    link: stage === "install" ? installErrorLink(message) : "/convert",
  });
}

/** The Upload plan of the image being built ("send this game folder as an image"). */
let pendingImageUpload: ImageUploadPlan | null = null;

/** Puts a finished image in the Upload queue and starts that console's queue. */
function queueImage(imagePath: string, bytes: number, plan: ImageUploadPlan) {
  const q = useUploadQueueStore.getState();
  const id = q.add(imageUploadItem(imagePath, plan, bytes));
  // Run just this image: uploads someone queued for later still wait for Start.
  void q.startHost(plan.host, { onlyIds: [id] });
}

/** A build that ends the run (Convert only, or a .ffpfsc image). */
function finish(packagePath: string, packageBytes: number, convertMs: number) {
  const p = running();
  if (!p) return;
  const plan = makesImage(p.mode) ? pendingImageUpload : null;
  pendingImageUpload = null;
  if (plan) queueImage(packagePath, packageBytes, plan);
  if (p.taskId) useTaskStore.getState().finishTask(p.taskId, "done");
  dropCopy(p);
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
      titleId: p.titleId,
      uploadQueued: !!plan,
    },
  });
}

/** Upload to staging, then install; the staged row's bytes and status drive Send and Install. */
async function uploadThenInstall(store: PkgLibraryStore, packagePath: string, host: string) {
  let dest: string | null = null;
  const unsubscribe = store.subscribe((s) => {
    const row = dest ? s.entries.find((e) => e.path === dest) : undefined;
    if (row?.status === "uploading") enterStage("send", row.bytes ?? 0, row.totalBytes ?? 0);
    else if (row?.status === "installing") enterStage("install");
  });
  try {
    return await store.getState().uploadInstall(packagePath, host, { onDest: (d) => (dest = d) });
  } finally {
    unsubscribe();
  }
}

/** `ps5://10.0.0.2/data/homebrew/G.exfat` → `/data/homebrew/G.exfat`. */
export function consoleDumpPath(source: string): string | null {
  const m = /^ps5:\/\/[^/]+(\/.*)$/.exec(source);
  return m ? m[1] : null;
}

/** A dump on the console is swapped for its package, never installed beside it: both share a
 *  title id, and ShadowMountPlus would register the dump again over the install. */
async function runReplace(packagePath: string, host: string, dump: string) {
  const p = running();
  if (!p) return;
  if (!p.titleId) {
    fail("park", "The package has no title id to match the dump with.", packagePath);
    return;
  }
  enterStage("park");
  const startedMs = Date.now();
  const r = await runSwap(
    { titleId: p.titleId, dump, packagePath },
    consoleSwapDeps(host),
    (step) => enterStage(step === "install" ? "install" : "park"),
  );
  if (!running()) return;
  if (!r.ok) {
    fail(running()!.stage, r.message ?? "The swap did not finish.", packagePath);
    return;
  }
  const cur = running()!;
  installDone(packagePath, startedMs - cur.startedMs, Date.now() - startedMs, r.journal ?? null);
  pushNotification("success", "Installed on the PS5", {
    body: "The old dump is set aside; delete it or keep it on the Convert screen.",
    link: "/convert",
  });
}

async function runInstall(packagePath: string, host: string | null, method: InstallMethod) {
  const p = running();
  if (!p) return;
  if (!host) {
    fail("send", "Connect to a PS5 to install.", packagePath);
    return;
  }
  const dump = consoleDumpPath(p.source);
  if (dump) {
    await runReplace(packagePath, host, dump);
    return;
  }
  enterStage("send");
  const startedMs = Date.now();
  const store = pkgLibraryStore(host);
  const r =
    method === "upload"
      ? await uploadThenInstall(store, packagePath, host)
      : await store
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

function installDone(
  packagePath: string,
  convertMs: number,
  installMs: number,
  swap: SwapJournal | null = null,
) {
  const p = running();
  if (!p) return;
  if (p.taskId) useTaskStore.getState().finishTask(p.taskId, "done");
  dropCopy(p);
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
      titleId: p.titleId,
      swap,
    },
  });
}

/** Bytes of the package being installed, remembered from the build (or the earlier result). */
const packageSizes = new Map<string, number>();
function packageBytesOf(p: Running): number {
  return (p.packagePath && packageSizes.get(p.packagePath)) || 0;
}

/** `install`: how to install the package once it is built, or null to stop at the package. */
function poll(jobId: string, install: InstallMethod | null, failures = 0) {
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
      } else if (!s && p.mode === "ffpfsc") {
        // A compression job reports no stages, only its overall bytes.
        enterStage("compress", snapshot.bytes_sent ?? 0, snapshot.total_bytes ?? 0);
      }
      poll(jobId, install);
      return;
    }
    if (snapshot.status === "done") {
      const path = snapshot.dest ?? "";
      const bytes = snapshot.bytes_sent ?? 0;
      packageSizes.set(path, bytes);
      const cur = running()!;
      update({ packagePath: path, jobId: null, titleId: titleIdOf(snapshot.tx_id_hex) });
      if (install) {
        // The console of the moment the install starts: the user may have switched during an
        // hour-long build.
        const host = currentHost();
        update({ host });
        await runInstall(path, host, install);
      } else {
        finish(path, bytes, Date.now() - cur.startedMs);
        pushNotification(
          "success",
          cur.mode === "ffpfsc"
            ? "Compression complete"
            : cur.mode === "image"
              ? "Game image ready"
              : "FPKG conversion complete",
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
  const taskId =
    mode === "install"
      ? null
      : useTaskStore.getState().registerTask({
          kind: mode === "ffpfsc" ? "ffpfsc-compress" : "fpkg-convert",
          origin: "convert",
          label: `${mode === "ffpfsc" ? "Compress" : mode === "image" ? "Make image of" : "Convert"} ${baseName(source)}`,
          detail: source,
          consoleId: host ?? "",
          control: { owner: "fpkg-convert" },
        });
  reportStage(taskId, stage, 0, 0);
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
      taskId,
      packagePath: null,
      titleId: null,
      copiedSource: null,
      extractedSource: null,
    },
  });
}

/** `.zip`, `.7z` or `.rar` (a multi-part set starts at its first `.partN.rar`). */
export function isArchiveSource(path: string): boolean {
  return /\.(zip|7z|rar)$/i.test(path.trim());
}

/** The engine's archive errors, said for a person. */
export function archiveErrorText(message: string): string {
  if (/rar_password_required/.test(message))
    return "This archive is password-protected. Enter its password and start again.";
  if (/rar_password_wrong/.test(message))
    return "The archive's password is wrong. Check it and start again.";
  const missing = /rar_missing_volume:\s*(.+)/.exec(message);
  if (missing)
    return `A part of this archive is missing: ${missing[1].trim()}. Keep every part in the same folder.`;
  return message;
}

/** Unpack an archive with the engine, following its job; the game path found inside. */
async function extractArchive(archive: string, outputDir: string | undefined, password?: string) {
  const { job_id } = await fpkg.extract(archive, outputDir, password);
  update({ jobId: job_id });
  for (;;) {
    const s = await jobStatus(job_id);
    if (s.status === "done") return s.dest ?? "";
    if (s.status === "failed") throw new Error(s.error ?? "The archive did not unpack.");
    if (running()?.jobId === job_id) enterStage("extract", s.bytes_sent ?? 0, s.total_bytes ?? 0);
    await new Promise((r) => setTimeout(r, POLL_MS));
  }
}

/** A source that needs this run's own stages before the build: a server archive is copied
 *  here (unpacking needs a local file), and any archive is unpacked. A server folder or image
 *  needs neither: the engine reads it in place. */
function needsPrep(source: string): boolean {
  return isArchiveSource(source);
}

/** Get a source ready for the build, as stages of this run (each with progress and Cancel):
 *  a server archive is copied into the output folder, an archive is unpacked there. Then start
 *  the job `startJob` makes on what that left. */
async function prepareThenStart(
  source: string,
  outputDir: string | undefined,
  startJob: (local: string) => Promise<{ job_id: string }>,
  install: InstallMethod | null,
  password?: string,
) {
  let local = source;
  if (isRemotePath(source)) {
    if (!(await copyFromServer(source, outputDir))) return;
    local = running()?.copiedSource ?? "";
  }
  if (isArchiveSource(local)) {
    enterStage("extract");
    try {
      local = await extractArchive(local, outputDir, password);
    } catch (error) {
      fail("extract", archiveErrorText(error instanceof Error ? error.message : String(error)), null);
      return;
    }
    if (!running()) {
      void fpkg.cleanupExtract(local).catch(() => {});
      return;
    }
    update({ extractedSource: local, jobId: null });
  }
  enterStage("check");
  try {
    const { job_id } = await startJob(local);
    update({ jobId: job_id });
    poll(job_id, install);
  } catch (error) {
    fail("check", error instanceof Error ? error.message : String(error), null);
  }
}

/** Copy a server source into the output folder; false (the run already failed) if it didn't. */
async function copyFromServer(source: string, outputDir: string | undefined): Promise<boolean> {
  let local: string;
  try {
    local = await fetchRemote(source, {
      // A folder per run, so a copy left by one run never blocks the next.
      destDir: outputDir
        ? `${outputDir.replace(/[\\/]+$/, "")}/.ps5upload-source/${runFolder()}`
        : undefined,
      pollMs: POLL_MS,
      onJob: (id) => update({ jobId: id }),
      onProgress: (done, total) => enterStage("copy", done, total),
    });
  } catch (error) {
    fail("copy", error instanceof Error ? error.message : String(error), null);
    return false;
  }
  if (!running()) return false;
  update({ copiedSource: local, jobId: null });
  return true;
}

function runFolder(): string {
  return `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
}

/** The copy of a server source and an archive's unpack are only scaffolding: drop them once
 *  the build is over, whether it finished or not (the next run makes its own). */
function dropCopy(p: Running) {
  if (p.copiedSource) void remoteApi.cleanupFetched(p.copiedSource).catch(() => {});
  if (p.extractedSource) void fpkg.cleanupExtract(p.extractedSource).catch(() => {});
}

/** "UP4433-PPSA17221_00-MINECRAFTPS50000" → "PPSA17221". */
function titleIdOf(contentId: string | undefined | null): string | null {
  return contentId && contentId.length >= 16 ? contentId.slice(7, 16) : null;
}

/** The console the connection bar has now, when a payload answers there. */
function currentHost(): string | null {
  const c = useConnectionStore.getState();
  return c.payloadStatus === "up" && c.host?.trim() ? c.host : null;
}

export const useFpkgConversion = create<ConversionState>((set, get) => ({
  pipeline: { phase: "idle" },

  start: async (req, { install, host, method = "stream", password }) => {
    const then = install ? method : null;
    if (get().pipeline.phase === "running") return;
    if (needsPrep(req.source)) {
      beginRun(
        install ? "convert-install" : "convert",
        req.source,
        host,
        isRemotePath(req.source) ? "copy" : "extract",
      );
      void prepareThenStart(
        req.source,
        req.outputDir,
        (local) => fpkg.build({ ...req, source: local }),
        then,
        password,
      );
      return;
    }
    beginRun(install ? "convert-install" : "convert", req.source, host, "check");
    try {
      const { job_id } = await fpkg.build(req);
      update({ jobId: job_id });
      poll(job_id, then);
    } catch (error) {
      fail("check", error instanceof Error ? error.message : String(error), null);
    }
  },

  compress: async (source, outputDir) => {
    if (get().pipeline.phase === "running") return;
    // Compressing reads the image from this machine's disk, so a server image is copied
    // first (unlike a build, which reads it in place).
    if (isRemotePath(source) || isArchiveSource(source)) {
      beginRun("ffpfsc", source, null, isRemotePath(source) ? "copy" : "extract");
      void prepareThenStart(source, outputDir, (local) => fpkg.compress(local, outputDir), null);
      return;
    }
    beginRun("ffpfsc", source, null, "compress");
    try {
      const { job_id } = await fpkg.compress(source, outputDir);
      update({ jobId: job_id });
      poll(job_id, null);
    } catch (error) {
      fail("compress", error instanceof Error ? error.message : String(error), null);
    }
  },

  buildImage: async (source, outputDir, thenCompress, format = "exfat", thenUpload) => {
    if (get().pipeline.phase === "running") return;
    pendingImageUpload = thenUpload ?? null;
    // One engine job: compressing happens as the image is written, so no uncompressed copy
    // is ever on disk.
    // The console it goes to, when it is sent once built: another console's view of this
    // game is not busy with it.
    beginRun("image", source, thenUpload?.host ?? null, "plan");
    try {
      const { job_id } = await fpkg.buildImage(source, outputDir, format, thenCompress);
      update({ jobId: job_id });
      poll(job_id, null);
    } catch (error) {
      fail("write", error instanceof Error ? error.message : String(error), null);
    }
  },

  retryInstall: async (host, method = "stream") => {
    const p = get().pipeline;
    const path =
      p.phase === "failed" ? p.packagePath : p.phase === "done" && !p.deleted ? p.packagePath : null;
    if (!path || (p.phase !== "failed" && p.phase !== "done")) return;
    if (p.phase === "done" && makesImage(p.mode)) return;
    beginRun("install", p.source, host, "send");
    update({ packagePath: path, titleId: p.titleId });
    await runInstall(path, host, method);
  },

  cancel: async () => {
    const p = running();
    if (p?.jobId) await jobCancel(p.jobId);
  },

  reset: () => {
    const phase = get().pipeline.phase;
    if (phase === "done" || phase === "failed") set({ pipeline: { phase: "idle" } });
  },

  finishReplace: async (choice) => {
    const p = get().pipeline;
    if (p.phase !== "done" || !p.swap || !p.host) return;
    await finishSwap(p.swap, choice, consoleSwapDeps(p.host));
    const now = get().pipeline;
    if (now.phase === "done") set({ pipeline: { ...now, swap: null } });
  },

  uploadImage: (plan) => {
    const p = get().pipeline;
    if (p.phase !== "done" || p.deleted || !makesImage(p.mode) || p.uploadQueued) return;
    queueImage(p.packagePath, p.packageBytes, plan);
    set({ pipeline: { ...p, uploadQueued: true } });
  },

  deletePackage: async () => {
    const p = get().pipeline;
    if (p.phase !== "done" || p.deleted) return;
    await fpkg.deletePackage(p.packagePath);
    set({ pipeline: { ...p, deleted: true } });
  },
}));
