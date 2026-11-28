// Convert to FPKG as one pipeline: an engine build job, then (for Convert & install) the install
// of the package it wrote — streamed from this computer, or uploaded to the PS5 first. The screen renders `pipeline`; every action here checks the
// phase it is allowed from, so a stray click cannot act on the wrong package or reset a run.

import { installErrorLink } from "../lib/installErrorDoc";
import { isTarArchiveName, stripTarSuffix } from "../lib/tarNames";
import { create } from "zustand";

import { t as translate } from "../i18n";
import { useLangStore } from "./lang";
import { useConvertPrefs, type AmprEmuBuild } from "./convertPrefs";

import {
  DEFAULT_IMAGE_SUBPATH,
  imageDriveNow,
  imageUploadItem,
  resolveImagePlan,
  type ImageUploadPlan,
} from "../lib/imageUpload";
import { resolveUploadDest } from "../lib/uploadDest";
import type { BuildExtras, ImageDestination } from "../api/fpkg";
import { useUploadQueueStore } from "./uploadQueue";

import {
  fpkg,
  type AmprLz4Options,
  type FfpfscLevel,
  type FpkgBuildRequest,
  type ImageFormat,
  type WorkshopRun,
  type ZstdPacks,
} from "../api/fpkg";
import { jobCancel, jobPause, jobResume, jobStatus, type JobSnapshot } from "../api/ps5";
import { useConnectionStore } from "./connection";
import { parsePS5Firmware } from "../lib/ps5Firmware";
import { useRosterStore } from "./roster";
import { hostOf } from "../lib/addr";
import { pushNotification } from "./notifications";
import { pkgLibraryStore, type PkgLibraryStore } from "./pkgLibrary";
import { interruptedAtLoad, useTaskStore, type Task } from "./tasks";
import { remoteApi } from "../api/remote";
import { fetchRemote } from "../lib/materialize";
import { isRemotePath, parseRemotePath } from "../lib/remotePath";
import { finishSwap, runSwap, type SwapJournal } from "../lib/dumpSwap";
import { consoleSwapDeps } from "../lib/dumpSwapConsole";
import { readWithRetries } from "../lib/pollRead";
import { trStatic } from "../lib/trStatic";

export type PipelineStage =
  | "copy"
  | "extract"
  | "check"
  | "pack"
  | "plan"
  | "compress"
  | "write"
  | "verify"
  | "park"
  | "send"
  | "install";

/** What a run does: a package, a package then its install, an install of a kept package, a
 *  game image (compressed into a .ffpfsc or not) from a folder or another image, or a plain
 *  folder from an image or a debug-key package. */
export type PipelineMode = "convert" | "convert-install" | "install" | "image" | "folder";

/** A run whose result is a game image to mount, not a package to install. */
export const makesImage = (mode: PipelineMode) => mode === "image";

/** A run whose result is a plain game folder on this computer. */
export const makesFolder = (mode: PipelineMode) => mode === "folder";

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
      /** `extractedSource` was unpacked with its archive deleted on the way ("free the
       *  archive's space while it unpacks"): that folder is the only copy of the game until
       *  something is made of it, so a run that fails keeps it. */
      freedArchive?: boolean;
      /** Found running, not started by this window (see `attachRunning`). */
      attached?: boolean;
      /** The engine job is paused (#631): it waits, and so does the queue behind it. */
      paused?: boolean;
      /** When the current pause began; null or absent while not paused. */
      pausedAtMs?: number | null;
      /** Paused time in this run before the current pause, left out of its times left. */
      pausedMs?: number;
      /** The same, for the current stage only (its speed). */
      stagePausedMs?: number;
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
      /** An image written straight to the PS5: `packagePath` is its path there. */
      onConsole?: boolean;
      /** A build found running after a reload, not started by this window: the queue takes its
       *  result rather than building the game again. */
      attached?: boolean;
      /** The PS5 took the install but it could not be confirmed: what to check. The package
       *  was kept. */
      unverifiedNote?: string;
      /** The run was asked to move its source to the Trash: whether it went, and the engine's
       *  line saying so (or why it was kept). */
      sourceNote?: SourceNote;
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
      /** Stopped by the user: not a fault. */
      cancelled?: boolean;
      /** The archive this run unpacked with its space freed is gone, and nothing was made of
       *  its game: where the unpacked files were kept. */
      archiveGone?: ArchiveGone;
    };

/** An archive deleted while it unpacked, in a run that did not finish. */
export interface ArchiveGone {
  /** The folder holding what was unpacked. */
  kept: string;
  /** The unpack itself stopped part way: the archive is cut short, and only some of the
   *  game is in `kept`. Otherwise the whole game is there and the archive is gone. */
  partly: boolean;
}

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
      /** Delete the package once its install is verified (Convert's "delete after"). */
      deleteAfter?: boolean;
      /** This game's own choice (a queued game keeps what it was added with); else the setting. */
      fullVerify?: boolean;
      /** DELETES the archive `source` as it unpacks, to free its space (confirmed on the
       *  screen for this one game; never for a copy fetched from a server). */
      freeArchive?: boolean;
    },
  ) => Promise<void>;
  /** Write a game folder or image as one game image (`format`, .exfat by default); with
   *  `thenCompress`, straight into a .ffpfsc (no uncompressed copy is written). */
  buildImage: (
    source: string,
    outputDir: string | undefined,
    thenCompress: boolean,
    format?: ImageFormat,
    /** Once the image is built and checked, put it in the Upload queue. */
    thenUpload?: ImageUploadPlan,
    /** Pack the game's files into AMPR LZ4 asset packs first (an .exfat image). */
    lz4?: AmprLz4Options,
    /** An archive's password (RAR); never stored. */
    password?: string,
    /** Number the image's name when it is taken in `outputDir`, instead of refusing. */
    keepBoth?: boolean,
    /** This game's own choices (a queued game keeps what it was added with); else the settings. */
    own?: {
      fullVerify?: boolean;
      imageLevel?: FfpfscLevel;
      addAmprEmu?: boolean;
      amprEmuBuild?: AmprEmuBuild;
      /** Save to the PS5: the image goes straight to this console's drive and folder, never
       *  stored on this computer (`outputDir` and `thenUpload` are not used). */
      toConsole?: ImageUploadPlan;
      /** Save to a server: the image goes straight to this folder on a saved server (a
       *  `remote://<connection>/<folder>` path), never stored on this computer. */
      toServer?: string;
      /** A PS4 update merged into the PS4 game `source`. */
      update?: string;
      /** Leave the backport libraries in the game's fakelib out of the image. */
      removeBackport?: boolean;
      /** Move `source` to the Trash once the image is verified and in place. For an archive
       *  only when it is read in place (one `.zip`, `.7z` or `.rar` on this computer). */
      deleteSource?: boolean;
      /** A game in asset packs is written as plain files again. */
      unpackPacks?: boolean;
      /** DELETES the archive `source` as it unpacks, when this run has to unpack it (a game
       *  image inside, an update merged in, a tar): see `start`. */
      freeArchive?: boolean;
    },
  ) => Promise<void>;
  /** Unpack a game image or a debug-key package into a plain folder in `outputDir`; with
   *  `keepBoth`, a name already taken there is numbered instead of refused. */
  buildFolder: (
    source: string,
    outputDir: string | undefined,
    keepBoth?: boolean,
    /** This game's own choice (a queued game keeps what it was added with); else the setting.
     *  `zstd` / `lz4`: the folder's files in zstd or LZ4 packs. */
    own?: {
      fullVerify?: boolean;
      zstd?: ZstdPacks;
      lz4?: AmprLz4Options;
      /** A PS4 update merged into the PS4 game `source`. */
      update?: string;
      /** Save to a server: the folder goes straight to this `remote://` folder. */
      toServer?: string;
      /** Leave the backport libraries in the game's fakelib out of the folder. */
      removeBackport?: boolean;
      /** Move `source` to the Trash once the folder is verified and in place. */
      deleteSource?: boolean;
      /** A game in asset packs is written as plain files again, with this emulator choice
       *  (the settings when absent). */
      unpackPacks?: boolean;
      addAmprEmu?: boolean;
      amprEmuBuild?: AmprEmuBuild;
      /** An archive's password (RAR); never stored. */
      password?: string;
      /** DELETES the archive `source` as it unpacks, when this run has to unpack it. */
      freeArchive?: boolean;
    },
  ) => Promise<void>;
  /** Put the finished image in the Upload queue. */
  uploadImage: (plan: ImageUploadPlan) => void;
  /** Install the kept package again: after a failed install, or Install again on a result. */
  retryInstall: (host: string, method?: InstallMethod) => Promise<void>;
  cancel: () => Promise<void>;
  /** Hold the running engine job where it is (#631); the queue waits behind it. */
  pause: () => Promise<void>;
  /** Let a paused job go on. */
  resume: () => Promise<void>;
  /** A new source: clears a finished result; ignored while running. */
  reset: () => void;
  /** Follow a Workshop job the engine is still running (after a reload, or from another window),
   *  when this window is idle. */
  attachRunning: () => Promise<void>;
  deletePackage: () => Promise<void>;
  /** After a swap: delete the parked dump, or keep it parked outside the scan roots. */
  finishReplace: (choice: "delete" | "keep") => Promise<void>;
}

/** How often a build job is polled (ms). */
export const POLL_MS = 500;
/** Consecutive failed polls before the run is declared lost. */
const MAX_POLL_FAILURES = 5;
const BUILD_STAGES: readonly PipelineStage[] = ["check", "pack", "plan", "compress", "write", "verify"];

type Running = Extract<Pipeline, { phase: "running" }>;

/** The stage names Convert's progress card shows. */
/** The stage as a Tasks line, in the language of the moment. */
function stageLabel(stage: PipelineStage): string {
  const english: Record<PipelineStage, string> = {
    copy: "Copy from server",
    extract: "Unpack archive",
    check: "Check source",
    pack: "Pack LZ4 assets",
    plan: "Plan package",
    compress: "Compress",
    write: "Write package",
    verify: "Verify",
    park: "Set the dump aside",
    send: "Send to PS5",
    install: "Install on PS5",
  };
  return trStatic(`fpkg.stage.${stage}`, english[stage]);
}

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

/** What a run's engine job is: a build (followed by `poll`) or an archive's unpack. */
type JobKind = "build" | "extract";

/** The run follows the engine job `jobId` from now on. Its task keeps the job's id and kind, so
 *  a page closed or reloaded meanwhile finds the job again (`attachRunning`, #954): the engine
 *  runs it by itself, whether a page watches or not. */
function follow(jobId: string, kind: JobKind) {
  update({ jobId });
  const p = running();
  if (p?.taskId)
    useTaskStore
      .getState()
      .updateTask(p.taskId, { engineJobId: jobId, payload: { mode: p.mode, source: p.source, jobKind: kind } });
}

/** Time this run has spent paused up to `now`, the current pause included. */
export function pausedSoFar(p: Running, now: number): number {
  return (p.pausedMs ?? 0) + (p.pausedAtMs != null ? Math.max(0, now - p.pausedAtMs) : 0);
}

/** Time the current stage has spent paused up to `now`. */
export function stagePausedSoFar(p: Running, now: number): number {
  const current = p.pausedAtMs != null ? Math.max(0, now - Math.max(p.pausedAtMs, p.stageStartedMs)) : 0;
  return (p.stagePausedMs ?? 0) + current;
}

/** Follow the engine's word on whether the job is paused, keeping the paused time apart. */
function markPaused(paused: boolean) {
  const p = running();
  if (!p || !!p.paused === paused) return;
  const now = Date.now();
  if (paused) {
    update({ paused: true, pausedAtMs: now });
  } else {
    update({
      paused: false,
      pausedAtMs: null,
      pausedMs: pausedSoFar(p, now),
      stagePausedMs: stagePausedSoFar(p, now),
    });
  }
  if (p.taskId) useTaskStore.getState().updateTask(p.taskId, { status: paused ? "paused" : "running" });
}

function reportStage(taskId: string | null, stage: PipelineStage, done: number, total: number) {
  if (!taskId) return;
  useTaskStore.getState().updateTask(taskId, {
    stage: stageLabel(stage),
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
    stageMs: { ...p.stageMs, [p.stage]: now - p.stageStartedMs - stagePausedSoFar(p, now) },
    // A pause that runs on into the new stage counts from the stage's start (stagePausedSoFar).
    stagePausedMs: 0,
  });
}

function fail(stage: PipelineStage, message: string, packagePath: string | null, gone?: ArchiveGone) {
  const p = running();
  if (!p) return;
  // A game unpacked from an archive that was deleted on the way, with nothing made of it yet:
  // its folder is all there is of the game, so it stays and the result says where it is.
  const archiveGone =
    gone ??
    (p.freedArchive && p.extractedSource && !packagePath ? { kept: p.extractedSource, partly: false } : undefined);
  dropCopy(p, !archiveGone);
  const cancelled = isCancelMessage(message);
  const said = archiveGone ? `${message} ${archiveGoneText(archiveGone)}` : message;
  if (p.taskId) {
    if (cancelled) useTaskStore.getState().finishTask(p.taskId, "cancelled");
    else
      useTaskStore.getState().finishTask(p.taskId, "failed", {
        lastError: { code: "FPKG_FAILED", message: said, recoverable: false },
      });
  }
  const failedTitle =
    p.mode === "image"
        ? trStatic("fpkg_failed_image", "Making the game image failed")
        : p.mode === "folder"
          ? trStatic("fpkg_failed_folder", "Making the game folder failed")
          : p.mode === "convert"
            ? trStatic("fpkg_failed_convert", "FPKG conversion failed")
            : trStatic("fpkg_failed_install", "Convert & install failed");
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
      cancelled,
      ...(archiveGone ? { archiveGone } : {}),
    },
  });
  // The user asked for this: the card says so, and it is not an error to report.
  if (cancelled) return;
  // An install-stage failure links to the install-route matrix entry for its
  // error; a build failure has no such page, so it goes back to Convert.
  pushNotification("error", failedTitle, {
    // The install stage's failure is an engine job's ending, which the host announces.
    jobOutcome: stage === "install",
    body: said,
    link: stage === "install" ? installErrorLink(message) : "/workshop",
  });
}

/** The Upload plan of the image being built ("send this game folder as an image"). */
let pendingImageUpload: ImageUploadPlan | null = null;
/** The image being built goes straight to the PS5: its result is a path there. */
let pendingToConsole = false;

/** Where an image goes on the PS5 (Save to: PS5): the drive decided now, as for an upload. */
async function consoleDestination(plan: ImageUploadPlan): Promise<ImageDestination> {
  const { volume } = await imageDriveNow(plan.host, plan.volume);
  const { destRoot } = resolveUploadDest(volume, plan.subpath || DEFAULT_IMAGE_SUBPATH, "x");
  return { kind: "console", addr: hostOf(plan.host), dir: destRoot };
}
/** Where a build goes on a saved server: `remote://<id>/<folder>` as the engine takes it. */
export function serverDestination(folder: string): ImageDestination | undefined {
  const parsed = parseRemotePath(folder);
  return parsed ? { kind: "server", source_id: parsed.connectionId, dir: parsed.path } : undefined;
}
/** The running Convert & install asked for its package to go once the install verified. */
let deleteAfterInstall = false;

/** Puts a finished image in the Upload queue and starts that console's queue. */
function queueImage(imagePath: string, bytes: number, plan: ImageUploadPlan) {
  // Its drive is decided now, with that console's drives as they are (#673).
  void resolveImagePlan(plan).then((resolved) => {
    const q = useUploadQueueStore.getState();
    const id = q.add(imageUploadItem(imagePath, resolved, bytes));
    // Like every add (#536): the console's queue runs in order, or waits for Start.
    q.startAdded(plan.host, [id]);
  });
}

/** What became of a source the run was asked to move to the Trash. */
export interface SourceNote {
  removed: boolean;
  note: string;
}

/** A finished job's word on its source, when it was asked to remove it. */
export function sourceNoteOf(ack: Record<string, unknown> | null | undefined): SourceNote | undefined {
  if (typeof ack?.source_removed !== "boolean") return undefined;
  return { removed: ack.source_removed, note: typeof ack.source_note === "string" ? ack.source_note : "" };
}

/** A build that ends the run (Convert only, or a .ffpfsc image). */
function finish(packagePath: string, packageBytes: number, convertMs: number, sourceNote?: SourceNote) {
  const p = running();
  if (!p) return;
  const plan = makesImage(p.mode) ? pendingImageUpload : null;
  const onConsole = makesImage(p.mode) && pendingToConsole;
  pendingImageUpload = null;
  pendingToConsole = false;
  if (plan) queueImage(packagePath, packageBytes, plan);
  if (p.taskId) useTaskStore.getState().finishTask(p.taskId, "done");
  dropCopy(p, true);
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
      onConsole,
      attached: p.attached,
      ...(sourceNote ? { sourceNote } : {}),
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

/** The swaps whose Delete / Keep is being carried out right now (see `finishReplace`). */
const finishingSwaps = new WeakSet<object>();

/** A dump on the console is swapped for its package, never installed beside it: both share a
 *  title id, and ShadowMountPlus would register the dump again over the install. */
async function runReplace(packagePath: string, host: string, dump: string) {
  const p = running();
  if (!p) return;
  if (!p.titleId) {
    fail(
      "park",
      trStatic("fpkg_swap_no_title_id", "The package has no title id to match the dump with."),
      packagePath,
    );
    return;
  }
  enterStage("park");
  const startedMs = Date.now();
  let r: Awaited<ReturnType<typeof runSwap>>;
  try {
    r = await runSwap(
      { titleId: p.titleId, dump, packagePath },
      consoleSwapDeps(host),
      (step) => enterStage(step === "install" ? "install" : "park"),
    );
  } catch (e) {
    // The swap's first questions (is the game running, is the dump there) reject when the
    // console does not answer, before anything is moved. Nothing caught that: the run stayed
    // "running" for good, with no Cancel and the queue waiting behind it.
    if (!running()) return;
    fail(
      "park",
      trStatic("fpkg_swap_no_answer", "The PS5 did not answer, so nothing was moved: {error}", {
        error: e instanceof Error ? e.message : String(e),
      }),
      packagePath,
    );
    return;
  }
  if (!running()) return;
  if (!r.ok) {
    fail(running()!.stage, r.message ?? trStatic("fpkg_swap_unfinished", "The swap did not finish."), packagePath);
    return;
  }
  const cur = running()!;
  if (r.unconfirmed) {
    // The result card does not offer to delete the dump: the swap stays in Convert's list of
    // unfinished swaps, with all three choices.
    installDone(
      packagePath,
      startedMs - cur.startedMs,
      Date.now() - startedMs,
      null,
      r.message ?? trStatic("queue_install_unverified", "install accepted; verify on PS5 (package kept)"),
    );
    return;
  }
  installDone(packagePath, startedMs - cur.startedMs, Date.now() - startedMs, r.journal ?? null);
  pushNotification("success", trStatic("fpkg_installed_on_ps5", "Installed on the PS5"), {
    body: trStatic("fpkg_dump_set_aside", "The old dump is set aside. Delete it or keep it on the Convert screen."),
    link: "/workshop",
  });
}

async function runInstall(packagePath: string, host: string | null, method: InstallMethod) {
  const p = running();
  if (!p) return;
  if (!host) {
    fail("send", trStatic("fpkg_connect_to_install", "Connect to a PS5 to install."), packagePath);
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
    if (r.unverified) {
      // The PS5 took it but nobody could confirm it: the package stays, and the card and the
      // bell say what to check instead of "Installed".
      const note =
        r.message ?? trStatic("queue_install_unverified", "install accepted; verify on PS5 (package kept)");
      installDone(packagePath, convertMs, Date.now() - startedMs, null, note);
      pushNotification(
        "warning",
        trStatic("pkg.check_the_ps5", "{label}: check the PS5").replace(
          "{label}",
          packagePath.slice(Math.max(packagePath.lastIndexOf("/"), packagePath.lastIndexOf("\\")) + 1),
        ),
        { body: note, link: "/workshop", jobOutcome: true },
      );
      return;
    }
    installDone(packagePath, convertMs, Date.now() - startedMs);
    pushNotification("success", trStatic("fpkg_installed_on_ps5", "Installed on the PS5"), {
      body: packagePath,
      link: "/workshop",
      jobOutcome: true,
    });
  } else {
    fail("install", r.message ?? "The install did not finish.", packagePath);
  }
}

function installDone(
  packagePath: string,
  convertMs: number,
  installMs: number,
  swap: SwapJournal | null = null,
  /** Set when the install could not be confirmed: the package is kept whatever was asked. */
  unverifiedNote: string | null = null,
) {
  const p = running();
  if (!p) return;
  if (p.taskId) useTaskStore.getState().finishTask(p.taskId, "done");
  dropCopy(p, true);
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
      ...(unverifiedNote ? { unverifiedNote } : {}),
    },
  });
  if (unverifiedNote) {
    deleteAfterInstall = false;
    return;
  }
  if (deleteAfterInstall) {
    deleteAfterInstall = false;
    void fpkg
      .deletePackage(packagePath)
      .then(() => {
        const cur = useFpkgConversion.getState().pipeline;
        if (cur.phase === "done" && cur.packagePath === packagePath)
          useFpkgConversion.setState({ pipeline: { ...cur, deleted: true } });
      })
      .catch(() => {});
  }
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
        // Giving up on watching must not leave the build running unseen: stop it, and say
        // so honestly when even the stop could not reach the engine.
        let stopped = true;
        try {
          await jobCancel(jobId);
        } catch {
          stopped = false;
        }
        if (running()?.jobId !== jobId) return;
        fail(
          p.stage,
          stopped
            ? trStatic("fpkg_engine_stopped", "The engine stopped responding, so the conversion was stopped.")
            : trStatic("fpkg_engine_lost", "The engine stopped responding. The conversion may still be running there. Check Tasks before starting it again."),
          null,
        );
      } else {
        poll(jobId, install, failures + 1);
      }
      return;
    }
    if (running()?.jobId !== jobId) return;
    if (snapshot.status === "running") {
      markPaused(snapshot.paused === true);
      const s = snapshot.stage;
      if (s && BUILD_STAGES.includes(s.id as PipelineStage)) {
        enterStage(s.id as PipelineStage, s.done, s.total);
      }
      poll(jobId, install);
      return;
    }
    markPaused(false);
    if (snapshot.status === "done") {
      const path = snapshot.dest ?? "";
      const bytes = snapshot.bytes_sent ?? 0;
      packageSizes.set(path, bytes);
      const cur = running()!;
      update({ packagePath: path, jobId: null, titleId: titleIdOf(snapshot.tx_id_hex) });
      if (install) {
        // The console chosen when Convert & install was pressed, even if the user switched
        // to another one during an hour-long build. When it has left the roster since, stop
        // with the package kept so the user picks where it goes.
        const target = installTarget(cur.host);
        if (target.gone) {
          fail(
            "send",
            trStatic("fpkg_console_gone", "{name} is no longer in your consoles. The package is kept. Choose a console and install it from here.", { name: target.gone }),
            path,
          );
          return;
        }
        update({ host: target.host });
        await runInstall(path, target.host, install);
      } else {
        finish(
          path,
          bytes,
          Date.now() - cur.startedMs - pausedSoFar(cur, Date.now()),
          sourceNoteOf(snapshot.commit_ack),
        );
        pushNotification(
          "success",
          cur.mode === "image"
            ? trStatic("fpkg_done_image", "Game image ready")
            : cur.mode === "folder"
              ? trStatic("fpkg_done_folder", "Game folder ready")
              : trStatic("fpkg_done_fpkg", "FPKG conversion complete"),
          { body: path, link: "/workshop" },
        );
      }
      return;
    }
    fail(p.stage, snapshot.error ?? "The build failed.", null);
  }, POLL_MS);
}


/** `taskId`: the task of a run this page started before it was closed or reloaded (#954), which
 *  goes on in place of a new one. */
function beginRun(mode: PipelineMode, source: string, host: string | null, stage: PipelineStage, taskId0?: string) {
  const now = Date.now();
  const taskId =
    taskId0 ??
    (mode === "install"
      ? null
      : useTaskStore.getState().registerTask({
          kind: "fpkg-convert",
          origin: "convert",
          label: `${mode === "image" ? "Make image of" : mode === "folder" ? "Make folder of" : "Convert"} ${baseName(source)}`,
          detail: source,
          consoleId: host ?? "",
          payload: { mode, source },
          control: { owner: "fpkg-convert" },
        }));
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

/** `.zip`, `.7z` or `.rar` (a multi-part set starts at its first `.partN.rar`), or a tar /
 *  `.zst` (lib/tarNames). */
export function isArchiveSource(path: string): boolean {
  return /\.(zip|7z|rar)$/i.test(path.trim()) || isTarArchiveName(path.trim());
}

/** A `.zip`, `.7z` or `.rar` (a multi-part set by its first part): an image or folder build
 *  reads its game straight out of it, nothing unpacked. A tar or `.zst` is unpacked first. */
export function isInPlaceArchive(path: string): boolean {
  return /\.(zip|7z|rar)$/i.test(path.trim());
}

/** The engine's refusal of an archive it can't read in place (a game image inside, an update
 *  to merge): this run unpacks it first instead. */
const ARCHIVE_NEEDS_UNPACK = "archive_needs_unpack";

/** What an image made from an archive is called: the archive's name without its extension
 *  or `.partN` (`Game.part1.rar` → `Game`, `Game.tar.zst` → `Game`). Its unpack folder's name
 *  means nothing. */
export function archiveStem(path: string): string {
  const name = path.trim().split(/[\\/]/).pop() ?? "";
  // Trailing dots and spaces are not allowed at the end of a Windows file name, and the engine
  // would only refuse it after a long unpack.
  return stripTarSuffix(name)
    .replace(/(\.part\d+)?\.(zip|7z|rar)$/i, "")
    .replace(/[. ]+$/, "");
}

/** A game image (`.exfat`, `.ffpkg`/`.ufs2`, `.ffpfs`, `.ffpfsc`) or a package (`.pkg`): what
 *  can be unpacked into a plain folder. A package has to be made with the debug key; the
 *  engine says so when it is not. */
export function isFolderableSource(path: string): boolean {
  return /\.(exfat|ffpkg|ufs2|ffpfs|ffpfsc|pkg)$/i.test(path.trim());
}

const ARCHIVE_PASSWORD_NEEDED = "This archive is password-protected. Enter its password and start again.";
const ARCHIVE_PASSWORD_WRONG = "The archive's password is wrong. Check it and start again.";

/** The engine's archive errors, said for a person. */
export function archiveErrorText(message: string): string {
  if (/rar_password_required/.test(message))
    return ARCHIVE_PASSWORD_NEEDED;
  if (/rar_password_wrong/.test(message))
    return ARCHIVE_PASSWORD_WRONG;
  const missing = /rar_missing_volume:\s*(.+)/.exec(message);
  if (missing)
    return `A part of this archive is missing: ${missing[1].trim()}. Keep every part in the same folder.`;
  return message;
}

/** Whether a run of `source` unpacks it to disk first, so that freeing the archive on the way
 *  gives room back: a package always does, and a tar always is; an image or a folder is made
 *  straight from a `.zip`, `.7z` or `.rar` with nothing unpacked, unless the game inside is a
 *  game image or an update is merged in. Never an archive on a server (what is unpacked is a
 *  copy of it). */
export function archiveIsUnpacked(
  source: string,
  kind: "pkg" | "image" | "folder",
  o: { merge?: boolean; game?: "folder" | "image" } = {},
): boolean {
  const path = source.trim();
  if (!isArchiveSource(path) || isRemotePath(path) || path.startsWith("ps5://")) return false;
  if (kind === "pkg" || !isInPlaceArchive(path)) return true;
  return !!o.merge || o.game === "image";
}

/** The engine's `error_reason` for an unpack that stopped with part of its archive deleted;
 *  `error_detail` is then the folder holding what was unpacked. */
const ARCHIVE_PARTLY_DELETED = "archive_partly_deleted";

/** Such a failure's cause alone: the engine's message goes on to say what the result card
 *  says in the user's language. */
function causeOf(error: string | undefined): string {
  const text = error ?? "The archive did not unpack.";
  const at = text.indexOf(". The archive is partly deleted");
  return at > 0 ? text.slice(0, at) : text;
}

/** An unpack that stopped with part of its archive deleted: `kept` holds what was unpacked. */
class ArchiveGoneError extends Error {
  constructor(
    message: string,
    readonly kept: string,
  ) {
    super(message);
  }
}

/** A scratch unpack folder (`.ps5upload-extract-*`), which is removed after use; the folder of
 *  an unpack that deleted its archive is not one, and stays. */
function isScratchUnpack(path: string): boolean {
  return /(^|[\\/])\.ps5upload-extract-[^\\/]+/.test(path);
}

/** What became of an archive deleted on the way and where its game is, in the user's language. */
export function archiveGoneText(gone: ArchiveGone): string {
  return translate(
    useLangStore.getState().lang,
    gone.partly ? "fpkg.free.gone_partly" : "fpkg.free.gone_whole",
    { path: gone.kept },
  );
}

/** A failed run's message as a list row says it: with where an archive's game was kept. */
export function failureText(p: Extract<Pipeline, { phase: "failed" }>): string {
  return p.archiveGone ? `${p.message} ${archiveGoneText(p.archiveGone)}` : p.message;
}

/** Unpack an archive with the engine, following its job; the game path found inside. With
 *  `free`, the archive is deleted as it unpacks. */
async function extractArchive(archive: string, outputDir: string | undefined, password?: string, free?: boolean) {
  const { job_id } = await fpkg.extract(archive, outputDir, password, ...(free ? [true as const] : []));
  follow(job_id, "extract");
  for (;;) {
    const s = await readWithRetries(() => jobStatus(job_id));
    if (running()?.jobId === job_id) markPaused(s.status === "running" && s.paused === true);
    if (s.status === "done") return s.dest ?? "";
    if (s.status === "failed") {
      if (s.error_reason === ARCHIVE_PARTLY_DELETED && s.error_detail)
        throw new ArchiveGoneError(causeOf(s.error), s.error_detail);
      throw new Error(s.error ?? "The archive did not unpack.");
    }
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
 *  the job `startJob` makes on what that left. With `inPlace`, a `.zip`, `.7z` or `.rar` is
 *  not unpacked: that job reads it as it is, unless the engine says it has to be unpacked. */
async function prepareThenStart(
  source: string,
  outputDir: string | undefined,
  startJob: (local: string) => Promise<{ job_id: string }>,
  install: InstallMethod | null,
  password?: string,
  inPlace?: (local: string) => Promise<{ job_id: string }>,
  /** Delete the archive as it unpacks (the user's own archive only, and only when unpacked). */
  free?: boolean,
) {
  let local = source;
  if (isRemotePath(source)) {
    if (!(await copyFromServer(source, outputDir))) return;
    local = running()?.copiedSource ?? "";
  }
  if (inPlace && isInPlaceArchive(local)) {
    enterStage("check");
    try {
      const { job_id } = await inPlace(local);
      follow(job_id, "build");
      poll(job_id, install);
      return;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (!message.includes(ARCHIVE_NEEDS_UNPACK)) {
        fail("check", archiveErrorText(message), null);
        return;
      }
      // A game image inside, or an update to merge: unpacked first, as before.
    }
    if (!running()) return;
  }
  if (isArchiveSource(local)) {
    enterStage("extract");
    // Only ever the archive that was chosen, never a copy fetched from a server.
    const freeing = !!free && local === source;
    try {
      local = await extractArchive(local, outputDir, password, freeing);
    } catch (error) {
      const gone = error instanceof ArchiveGoneError ? { kept: error.kept, partly: true } : undefined;
      fail("extract", archiveErrorText(error instanceof Error ? error.message : String(error)), null, gone);
      return;
    }
    if (!running()) {
      // Unpacked with its archive deleted, the folder is all there is of the game: it stays.
      if (!freeing) void fpkg.cleanupExtract(local).catch(() => {});
      return;
    }
    update({ extractedSource: local, jobId: null, ...(freeing ? { freedArchive: true } : {}) });
  }
  enterStage("check");
  try {
    const { job_id } = await startJob(local);
    follow(job_id, "build");
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
 *  the build is over, whether it finished or not (the next run makes its own). The unpack of
 *  an archive that was deleted on the way is the game's only copy until something is `made`
 *  of it: it goes then, and stays otherwise. */
function dropCopy(p: Running, made: boolean) {
  if (p.copiedSource) void remoteApi.cleanupFetched(p.copiedSource).catch(() => {});
  if (!p.extractedSource) return;
  if (!p.freedArchive) void fpkg.cleanupExtract(p.extractedSource).catch(() => {});
  else if (made) void fpkg.cleanupExtract(p.extractedSource, true).catch(() => {});
}

/** "UP4433-PPSA17221_00-MINECRAFTPS50000" → "PPSA17221". */
function titleIdOf(contentId: string | undefined | null): string | null {
  return contentId && contentId.length >= 16 ? contentId.slice(7, 16) : null;
}

/** Where a finished build installs: the console recorded when the run started, or (when none
 *  was) the one connected now. `gone` names a recorded console that left the roster. */
export function installTarget(recorded: string | null): { host: string | null; gone?: string } {
  if (!recorded) return { host: currentHost() };
  const profiles = useRosterStore.getState().profiles ?? [];
  if (profiles.length > 0 && !profiles.some((p) => hostOf(p.host) === hostOf(recorded)))
    return { host: null, gone: recorded };
  return { host: recorded };
}

/** The console the connection bar has now, when a payload answers there. */
function currentHost(): string | null {
  const c = useConnectionStore.getState();
  return c.payloadStatus === "up" && c.host?.trim() ? c.host : null;
}

/** An image's format kept when it is compressed into a `.ffpfsc`. */
export function imageFormatOf(path: string): ImageFormat {
  return /\.ffpkg$/i.test(path.trim()) ? "ffpkg" : "exfat";
}

/** Whether an archive's error is about its password (the queue row asks for one). */
export function isArchivePasswordError(message: string | undefined): boolean {
  return message === ARCHIVE_PASSWORD_NEEDED || message === ARCHIVE_PASSWORD_WRONG;
}

/** Run mode for a job kind the engine reports. */
function modeOfKind(kind: WorkshopRun["kind"]): PipelineMode {
  return kind === "image" ? "image" : kind === "folder" ? "folder" : "convert";
}

/** A run this page left going when it was closed or reloaded, as the engine answers for its job. */
interface LeftRun {
  task: Task;
  jobId: string;
  kind: JobKind;
  snap: JobSnapshot;
}

/** The Convert tasks this start-up found running and marked interrupted, with their engine job's
 *  state now; those the engine does not know (it restarted) or did not answer for are left out
 *  and stay interrupted. */
async function leftRunning(): Promise<LeftRun[]> {
  const tasks = useTaskStore
    .getState()
    .tasks.filter((t) => t.kind === "fpkg-convert" && t.status === "interrupted" && interruptedAtLoad.has(t.id));
  const found = await Promise.all(
    tasks.map(async (task): Promise<LeftRun | null> => {
      const jobId = task.engineJobId;
      if (!jobId) return null;
      try {
        const snap = await readWithRetries(() => jobStatus(jobId));
        return { task, jobId, kind: task.payload.jobKind === "extract" ? "extract" : "build", snap };
      } catch {
        return null;
      }
    }),
  );
  return found.filter((l): l is LeftRun => l !== null);
}

/** A left run this screen does not follow (it follows one run at a time): a build that ended has
 *  its task say how; one still running, or an unpack whose build was lost, stays interrupted. */
function settleLeft({ task, kind, snap }: LeftRun) {
  if (kind !== "build" || (snap.status !== "done" && snap.status !== "failed")) return;
  const tasks = useTaskStore.getState();
  if (!tasks.reopenTask(task.id)) return;
  if (snap.status === "done") tasks.finishTask(task.id, "done", snap.dest ? { detail: snap.dest } : undefined);
  else if (isCancelMessage(snap.error ?? "")) tasks.finishTask(task.id, "cancelled");
  else
    tasks.finishTask(task.id, "failed", {
      lastError: { code: "FPKG_FAILED", message: snap.error ?? "The build failed.", recoverable: false },
    });
}

/** The run mode a left task is followed in: what it makes. An install that was to follow a build
 *  is not started from here (another window may be doing it): the result offers Install. */
function modeOfTask(task: Task): PipelineMode {
  const mode = task.payload.mode;
  return mode === "image" || mode === "folder" ? mode : "convert";
}

function sourceOfTask(task: Task): string {
  return typeof task.payload.source === "string" ? task.payload.source : (task.detail ?? "");
}

/** Follow an unpack whose page was closed: the build that was to follow it died with the page, so
 *  its folder is dropped and the user is told to start again. */
async function followLostUnpack(jobId: string) {
  try {
    for (;;) {
      if (running()?.jobId !== jobId) return;
      const s = await jobStatus(jobId);
      if (running()?.jobId !== jobId) return;
      if (s.status === "done") {
        // An unpack that deleted its archive: its folder is the game now, and stays.
        const gone = s.dest && !isScratchUnpack(s.dest) ? { kept: s.dest, partly: false } : undefined;
        if (s.dest && !gone) void fpkg.cleanupExtract(s.dest).catch(() => {});
        fail("extract", translate(useLangStore.getState().lang, "fpkg.unpack_lost", undefined), null, gone);
        return;
      }
      if (s.status === "failed") {
        const gone =
          s.error_reason === ARCHIVE_PARTLY_DELETED && s.error_detail
            ? { kept: s.error_detail, partly: true }
            : undefined;
        fail("extract", archiveErrorText(gone ? causeOf(s.error) : (s.error ?? "The archive did not unpack.")), null, gone);
        return;
      }
      markPaused(s.paused === true);
      enterStage("extract", s.bytes_sent ?? 0, s.total_bytes ?? 0);
      await new Promise((r) => setTimeout(r, POLL_MS));
    }
  } catch (error) {
    fail("extract", error instanceof Error ? error.message : String(error), null);
  }
}

/** An image build's trailing `destination`, `update` and extras arguments, without the empty
 *  ones at the end, so a plain local build's call stays as it was. */
function buildTail(
  destination: ImageDestination | undefined,
  update: string | undefined,
  extra?: BuildExtras,
): [destination?: ImageDestination, update?: string, extra?: BuildExtras] {
  if (extra) return [destination, update, extra];
  if (update) return [destination, update];
  return destination ? [destination] : [];
}

/** The choices of a build that are off unless asked for, or nothing when none is on. An
 *  archive's build from a copy (`scratch`: unpacked, or copied from a server) never removes its
 *  source: what it reads is the copy. `amprEmu` (a folder build's emulator choice) goes along
 *  only when the packs are unpacked; `archivePassword` with an archive read in place. */
function extrasOf(
  own: { removeBackport?: boolean; deleteSource?: boolean; unpackPacks?: boolean } | undefined,
  scratch = false,
  amprEmu?: { add: boolean; build: AmprEmuBuild },
  archivePassword?: string,
): BuildExtras | undefined {
  const removeBackport = !!own?.removeBackport;
  const deleteSource = !!own?.deleteSource && !scratch;
  const unpackPacks = !!own?.unpackPacks;
  if (!removeBackport && !deleteSource && !unpackPacks && !archivePassword) return undefined;
  return {
    ...(removeBackport ? { removeBackport } : {}),
    ...(deleteSource ? { deleteSource } : {}),
    ...(unpackPacks ? { unpackPacks, ...(amprEmu ? { amprEmu } : {}) } : {}),
    ...(archivePassword ? { archivePassword } : {}),
  };
}

/** A folder build's trailing `update`, `destination` and extras arguments, without the empty
 *  ones at the end, so a plain local build's call stays as it was. */
function folderTail(
  update: string | undefined,
  destination: ImageDestination | undefined,
  extra?: BuildExtras,
): [update?: string, destination?: ImageDestination, extra?: BuildExtras] {
  if (extra) return [update, destination, extra];
  if (destination) return [update, destination];
  return update ? [update] : [];
}

export const useFpkgConversion = create<ConversionState>((set, get) => ({
  pipeline: { phase: "idle" },

  start: async (startReq, { install, host, method = "stream", password, deleteAfter, fullVerify: ownVerify, freeArchive }) => {
    const then = install ? method : null;
    if (get().pipeline.phase === "running") return;
    // The engine checks the package's size against what this console's firmware opens.
    const consoleFirmware = parsePS5Firmware(useConnectionStore.getState().ps5Kernel) ?? undefined;
    const req = consoleFirmware && !startReq.consoleFirmware ? { ...startReq, consoleFirmware } : startReq;
    const fullVerify = ownVerify ?? useConvertPrefs.getState().fullVerify;
    deleteAfterInstall = install && !!deleteAfter;
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
        (local) => fpkg.build({ ...req, source: local, fullVerify }),
        then,
        password,
        undefined,
        freeArchive,
      );
      return;
    }
    beginRun(install ? "convert-install" : "convert", req.source, host, "check");
    try {
      const { job_id } = await fpkg.build({ ...req, fullVerify });
      follow(job_id, "build");
      poll(job_id, then);
    } catch (error) {
      fail("check", error instanceof Error ? error.message : String(error), null);
    }
  },

  buildImage: async (source, outputDir, thenCompress, format = "exfat", thenUpload, lz4, password, keepBoth, own) => {
    if (get().pipeline.phase === "running") return;
    const toConsole = own?.toConsole;
    const toServer = own?.toServer ? serverDestination(own.toServer) : undefined;
    // Straight to the PS5 or a server, nothing is left here to upload afterwards.
    pendingImageUpload = toConsole || toServer ? null : (thenUpload ?? null);
    pendingToConsole = !!toConsole;
    thenUpload = toConsole ?? thenUpload;
    const prefs = useConvertPrefs.getState();
    const verifyChunkKib = prefs.verifyChunkKib;
    const fullVerify = own?.fullVerify ?? prefs.fullVerify;
    const imageLevel = own?.imageLevel ?? prefs.imageLevel;
    const amprEmu = {
      add: own?.addAmprEmu ?? prefs.addAmprEmu,
      build: own?.amprEmuBuild ?? prefs.amprEmuBuild,
    };
    // One engine job: compressing happens as the image is written, so no uncompressed copy
    // is ever on disk.
    // The console it goes to, when it is sent once built: another console's view of this
    // game is not busy with it.
    if (needsPrep(source)) {
      // A .zip, .7z or .rar is read in place (copied here first when it is on a server); a tar,
      // or an archive holding a game image, is unpacked first, as a package build does.
      const inPlace = isInPlaceArchive(source);
      beginRun(
        "image",
        source,
        thenUpload?.host ?? null,
        isRemotePath(source) ? "copy" : inPlace ? "check" : "extract",
      );
      const name = archiveStem(source) || undefined;
      const job = async (local: string, extra: BuildExtras | undefined) =>
        fpkg.buildImage(
          local,
          outputDir,
          format,
          thenCompress,
          lz4,
          verifyChunkKib,
          fullVerify,
          name,
          imageLevel,
          keepBoth,
          amprEmu,
          ...buildTail(toConsole ? await consoleDestination(toConsole) : toServer, own?.update, extra),
        );
      void prepareThenStart(
        source,
        outputDir,
        (local) => job(local, extrasOf(own, true)),
        null,
        password,
        inPlace
          ? (local) => job(local, extrasOf(own, isRemotePath(source), undefined, password || undefined))
          : undefined,
        own?.freeArchive,
      );
      return;
    }
    beginRun("image", source, thenUpload?.host ?? null, lz4 ? "pack" : "plan");
    try {
      const { job_id } = await fpkg.buildImage(
        source,
        outputDir,
        format,
        thenCompress,
        lz4,
        verifyChunkKib,
        fullVerify,
        undefined,
        imageLevel,
        keepBoth,
        amprEmu,
        // Only when it goes to the PS5: a local build's call stays as it was.
        ...buildTail(toConsole ? await consoleDestination(toConsole) : toServer, own?.update, extrasOf(own)),
      );
      follow(job_id, "build");
      poll(job_id, null);
    } catch (error) {
      fail(lz4 ? "pack" : "write", error instanceof Error ? error.message : String(error), null);
    }
  },

  buildFolder: async (source, outputDir, keepBoth, own) => {
    if (get().pipeline.phase === "running") return;
    pendingImageUpload = null;
    // Packs are made before the folder is written.
    const first = own?.zstd || own?.lz4 ? "pack" : "write";
    const amprEmu = own?.unpackPacks
      ? {
          add: own.addAmprEmu ?? useConvertPrefs.getState().addAmprEmu,
          build: own.amprEmuBuild ?? useConvertPrefs.getState().amprEmuBuild,
        }
      : undefined;
    // A folder's LZ4 packs go with the build's other choices.
    const withLz4 = (extra: BuildExtras | undefined): BuildExtras | undefined =>
      own?.lz4 ? { ...extra, lz4: own.lz4 } : extra;
    const extras = extrasOf(own, false, amprEmu);
    if (isInPlaceArchive(source)) {
      // A .zip, .7z or .rar: its game folder read straight out of it (copied here first when it
      // is on a server); one holding a game image is unpacked first and the image made a folder.
      beginRun("folder", source, null, isRemotePath(source) ? "copy" : "check");
      const name = archiveStem(source) || undefined;
      const fullVerify = own?.fullVerify ?? useConvertPrefs.getState().fullVerify;
      const dest = own?.toServer ? serverDestination(own.toServer) : undefined;
      const job = (local: string, extra: BuildExtras | undefined) =>
        fpkg.buildFolder(local, outputDir, name, fullVerify, undefined, keepBoth, own?.zstd, ...folderTail(own?.update, dest, withLz4(extra)));
      void prepareThenStart(
        source,
        outputDir,
        (local) => job(local, extrasOf(own, true, amprEmu)),
        null,
        own?.password,
        (local) => job(local, extrasOf(own, isRemotePath(source), amprEmu, own?.password || undefined)),
        own?.freeArchive,
      );
      return;
    }
    beginRun("folder", source, null, first);
    try {
      const { job_id } = await fpkg.buildFolder(
        source,
        outputDir,
        undefined,
        own?.fullVerify ?? useConvertPrefs.getState().fullVerify,
        undefined,
        keepBoth,
        own?.zstd,
        ...folderTail(
          own?.update,
          own?.toServer ? serverDestination(own.toServer) : undefined,
          withLz4(extras),
        ),
      );
      follow(job_id, "build");
      poll(job_id, null);
    } catch (error) {
      fail(first, error instanceof Error ? error.message : String(error), null);
    }
  },

  retryInstall: async (host, method = "stream") => {
    const p = get().pipeline;
    const path =
      p.phase === "failed" ? p.packagePath : p.phase === "done" && !p.deleted ? p.packagePath : null;
    if (!path || (p.phase !== "failed" && p.phase !== "done")) return;
    if (makesFolder(p.mode) || (p.phase === "done" && makesImage(p.mode))) return;
    deleteAfterInstall = false;
    beginRun("install", p.source, host, "send");
    update({ packagePath: path, titleId: p.titleId });
    await runInstall(path, host, method);
  },

  cancel: async () => {
    const p = running();
    if (p?.jobId) await jobCancel(p.jobId);
  },

  pause: async () => {
    const p = running();
    if (!p?.jobId || p.paused) return;
    const jobId = p.jobId;
    await jobPause(jobId);
    // Shown at once; the next poll keeps it in line with the engine.
    if (running()?.jobId === jobId) markPaused(true);
  },

  resume: async () => {
    const p = running();
    if (!p?.jobId || !p.paused) return;
    const jobId = p.jobId;
    await jobResume(jobId);
    if (running()?.jobId === jobId) markPaused(false);
  },

  reset: () => {
    const phase = get().pipeline.phase;
    if (phase === "done" || phase === "failed") set({ pipeline: { phase: "idle" } });
  },

  attachRunning: async () => {
    if (get().pipeline.phase !== "idle") return;
    // This page's own runs that were going when it was closed or reloaded (#954). The engine
    // runs a job by itself, so each is asked about: still running, or ended while the page was
    // away, its task goes on instead of staying "interrupted". A failed read is asked again
    // (readWithRetries); only an engine that no longer knows the job (it restarted), or no
    // answer at all, leaves the task interrupted.
    const left = await leftRunning();
    const runs: WorkshopRun[] | null = await fpkg.runningJobs().catch(() => null);
    // An engine that answers with no list has nothing else to follow.
    const others = (Array.isArray(runs) ? runs : []).filter((r) => !left.some((l) => l.jobId === r.job_id));
    // Followed on this screen: a run of ours still going, else one from another window, else
    // one of ours that ended meanwhile (its result and Install, Delete... as if never away).
    const mine = left.find((l) => l.snap.status === "running") ?? (others.length ? undefined : left[0]);
    const followed = get().pipeline.phase === "idle" ? mine ?? others[0] : undefined;
    for (const l of left) if (l !== followed) settleLeft(l);
    if (!followed) return;
    if ("task" in followed) {
      const { task, jobId, kind, snap } = followed;
      useTaskStore.getState().reopenTask(task.id);
      beginRun(modeOfTask(task), sourceOfTask(task), task.consoleId || null, kind === "extract" ? "extract" : "check", task.id);
      update({ jobId, attached: true, startedMs: snap.started_at_ms || Date.now() });
      if (kind === "extract") void followLostUnpack(jobId);
      else poll(jobId, null);
      return;
    }
    const run = followed;
    const stage: PipelineStage = run.kind === "extract" ? "extract" : "check";
    beginRun(modeOfKind(run.kind), run.source, null, stage);
    update({ jobId: run.job_id, attached: true, startedMs: run.started_at_ms || Date.now() });
    if (run.kind === "extract") void followLostUnpack(run.job_id);
    else poll(run.job_id, null);
  },

  finishReplace: async (choice) => {
    const p = get().pipeline;
    if (p.phase !== "done" || !p.swap || !p.host) return;
    // One answer per swap: a second click (or Delete, then Keep) while the first is still at
    // work ran a second finish on the same dump, and its failure read as a conversion error.
    if (finishingSwaps.has(p.swap)) return;
    finishingSwaps.add(p.swap);
    try {
      await finishSwap(p.swap, choice, consoleSwapDeps(p.host));
    } finally {
      finishingSwaps.delete(p.swap);
    }
    const now = get().pipeline;
    if (now.phase === "done") set({ pipeline: { ...now, swap: null } });
  },

  uploadImage: (plan) => {
    const p = get().pipeline;
    if (p.phase !== "done" || p.deleted || !makesImage(p.mode) || p.uploadQueued) return;
    // On a server: nothing on this computer to upload.
    if (isRemotePath(p.packagePath)) return;
    queueImage(p.packagePath, p.packageBytes, plan);
    set({ pipeline: { ...p, uploadQueued: true } });
  },

  deletePackage: async () => {
    const p = get().pipeline;
    if (p.phase !== "done" || p.deleted || isRemotePath(p.packagePath)) return;
    await fpkg.deletePackage(p.packagePath);
    set({ pipeline: { ...p, deleted: true } });
  },
}));
