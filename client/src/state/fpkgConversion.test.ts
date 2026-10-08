import { beforeEach, describe, expect, it, vi } from "vitest";

// The run reports into the real task store, which persists through `window.localStorage`;
// vitest's node env has no window.
vi.hoisted(() => {
  const mem = new Map<string, string>();
  (globalThis as { window?: unknown }).window = {
    localStorage: {
      getItem: (k: string) => (mem.has(k) ? (mem.get(k) as string) : null),
      setItem: (k: string, v: string) => void mem.set(k, String(v)),
      removeItem: (k: string) => void mem.delete(k),
      clear: () => mem.clear(),
    },
    location: { origin: "http://127.0.0.1:19113" },
    addEventListener: () => {},
    removeEventListener: () => {},
  };
});

const build = vi.fn();
const extract = vi.fn();
const cleanupExtract = vi.fn(async () => ({ ok: true }));
const deletePackage = vi.fn(async () => ({ ok: true }));
const jobStatus = vi.fn();
const installStream = vi.fn();
const uploadInstall = vi.fn();
// The staged row an upload install reports through; the test drives it.
type Listener = (s: { entries: { path: string; status: string; bytes?: number; totalBytes?: number }[] }) => void;
const listeners = new Set<Listener>();
const emitRow = (row: { path: string; status: string; bytes?: number; totalBytes?: number }) =>
  listeners.forEach((l) => l({ entries: [row] }));

vi.mock("../api/fpkg", () => ({
  fpkg: {
    build: (...a: unknown[]) => build(...a),
    extract: (...a: unknown[]) => extract(...a),
    cleanupExtract: (...a: unknown[]) => cleanupExtract(...(a as [])),
    compress: vi.fn(),
    deletePackage: (...a: unknown[]) => deletePackage(...(a as [])),
  },
}));
vi.mock("../api/ps5", () => ({
  jobStatus: (...a: unknown[]) => jobStatus(...a),
  jobCancel: vi.fn(),
}));
vi.mock("./pkgLibrary", () => ({
  pkgLibraryStore: () => ({
    getState: () => ({
      installStream: (...a: unknown[]) => installStream(...a),
      uploadInstall: (...a: unknown[]) => uploadInstall(...a),
    }),
    subscribe: (l: Listener) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
  }),
}));
vi.mock("./notifications", () => ({ pushNotification: vi.fn() }));
const runSwap = vi.fn();
const finishSwap = vi.fn(async () => {});
vi.mock("../lib/dumpSwap", () => ({
  runSwap: (...a: unknown[]) => runSwap(...a),
  finishSwap: (...a: unknown[]) => finishSwap(...(a as [])),
}));
vi.mock("../lib/dumpSwapConsole", () => ({ consoleSwapDeps: () => ({}) }));
const remoteFetch = vi.fn();
const cleanupFetched = vi.fn(async () => {});
vi.mock("../api/remote", () => ({
  remoteApi: {
    fetch: (...a: unknown[]) => remoteFetch(...a),
    cleanupFetched: (...a: unknown[]) => cleanupFetched(...(a as [])),
  },
}));
// The console of the moment: what the connection bar says when the install starts.
const conn = { host: "10.0.0.2", payloadStatus: "up" };
vi.mock("./connection", () => ({ useConnectionStore: { getState: () => conn } }));

import { POLL_MS, useFpkgConversion } from "./fpkgConversion";
import { commandTask, taskCapabilities } from "./taskControls";
import { useTaskStore } from "./tasks";

const req = { source: "/games/a", outputDir: "/out" };
const tick = () => vi.advanceTimersByTimeAsync(POLL_MS + 50);

describe("fpkg pipeline", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useFpkgConversion.setState({ pipeline: { phase: "idle" } });
    useTaskStore.setState({ tasks: [] });
    build.mockReset().mockResolvedValue({ job_id: "j1" });
    extract.mockReset().mockResolvedValue({ job_id: "x1" });
    runSwap.mockReset();
    finishSwap.mockClear();
    cleanupExtract.mockClear();
    jobStatus.mockReset();
    installStream.mockReset();
    uploadInstall.mockReset();
    listeners.clear();
    remoteFetch.mockReset().mockResolvedValue({ job_id: "c1" });
    cleanupFetched.mockClear();
    deletePackage.mockClear();
    conn.host = "10.0.0.2";
    conn.payloadStatus = "up";
  });

  it("converts through the stages and ends done", async () => {
    jobStatus
      .mockResolvedValueOnce({
        status: "running",
        stage: { id: "compress", index: 2, count: 5, done: 5, total: 10 },
      })
      .mockResolvedValueOnce({ status: "done", dest: "/out/a.pkg", bytes_sent: 99 });
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "running", stage: "check" });
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "running",
      stage: "compress",
      stageDone: 5,
      stageTotal: 10,
    });
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "done",
      mode: "convert",
      packagePath: "/out/a.pkg",
      packageBytes: 99,
    });
  });

  it("chains into the install on the host of the moment, then ends done", async () => {
    jobStatus.mockResolvedValue({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    installStream.mockImplementation(async (_p: string, _h: string, opts?: { onTask?: (id: string) => void }) => {
      opts?.onTask?.("task-7");
      return { ok: true };
    });
    await useFpkgConversion.getState().start(req, { install: true, host: "10.0.0.2" });
    await tick();
    await tick();
    expect(installStream).toHaveBeenCalledWith("/out/a.pkg", "10.0.0.2", expect.anything());
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "done",
      mode: "convert-install",
      host: "10.0.0.2",
    });
  });

  it("keeps the package when the install fails, and retries on the new host", async () => {
    jobStatus.mockResolvedValue({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    installStream
      .mockResolvedValueOnce({ ok: false, message: "unreachable" })
      .mockResolvedValueOnce({ ok: true });
    await useFpkgConversion.getState().start(req, { install: true, host: "10.0.0.2" });
    await tick();
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "failed",
      stage: "install",
      message: "unreachable",
      packagePath: "/out/a.pkg",
    });
    await useFpkgConversion.getState().retryInstall("10.0.0.3");
    expect(installStream).toHaveBeenLastCalledWith("/out/a.pkg", "10.0.0.3", expect.anything());
    expect(build).toHaveBeenCalledTimes(1);
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done", host: "10.0.0.3" });
  });

  it("uploads then installs a converted package, with the upload's bytes on Send", async () => {
    jobStatus.mockResolvedValue({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    let seen: unknown;
    uploadInstall.mockImplementation(
      async (_p: string, _h: string, opts?: { onDest?: (d: string) => void }) => {
        opts?.onDest?.("/data/pkg/A.pkg");
        emitRow({ path: "/data/pkg/A.pkg", status: "uploading", bytes: 30, totalBytes: 100 });
        seen = useFpkgConversion.getState().pipeline;
        emitRow({ path: "/data/pkg/A.pkg", status: "installing" });
        return { ok: true };
      },
    );
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    await tick();
    await useFpkgConversion.getState().retryInstall("10.0.0.2", "upload");
    expect(uploadInstall).toHaveBeenCalledWith("/out/a.pkg", "10.0.0.2", expect.anything());
    expect(installStream).not.toHaveBeenCalled();
    expect(seen).toMatchObject({ stage: "send", stageDone: 30, stageTotal: 100 });
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "done",
      mode: "install",
      packagePath: "/out/a.pkg",
    });
    expect(listeners.size).toBe(0);
  });

  it("keeps the package when an upload install fails", async () => {
    jobStatus.mockResolvedValue({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    uploadInstall.mockResolvedValue({ ok: false, message: "Upload failed: disk full" });
    await useFpkgConversion.getState().start(req, { install: true, host: "10.0.0.2", method: "upload" });
    await tick();
    await tick();
    expect(uploadInstall).toHaveBeenCalledTimes(1);
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "failed",
      message: "Upload failed: disk full",
      packagePath: "/out/a.pkg",
    });
  });

  it("installs again from a finished result, but not after the package is deleted", async () => {
    jobStatus.mockResolvedValue({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    installStream.mockResolvedValue({ ok: true });
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    await tick();
    await useFpkgConversion.getState().retryInstall("10.0.0.2");
    expect(installStream).toHaveBeenCalledTimes(1);
    await useFpkgConversion.getState().deletePackage();
    expect(deletePackage).toHaveBeenCalledWith("/out/a.pkg");
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done", deleted: true });
    await useFpkgConversion.getState().retryInstall("10.0.0.2");
    expect(installStream).toHaveBeenCalledTimes(1);
  });

  it("installs on the console selected when the install starts, not when Convert was pressed", async () => {
    jobStatus
      .mockResolvedValueOnce({ status: "running" })
      .mockResolvedValue({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    installStream.mockResolvedValue({ ok: true });
    await useFpkgConversion.getState().start(req, { install: true, host: "10.0.0.2" });
    await tick();
    conn.host = "10.0.0.9"; // the user switched consoles during the build
    await tick();
    await tick();
    expect(installStream).toHaveBeenCalledWith("/out/a.pkg", "10.0.0.9", expect.anything());
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done", host: "10.0.0.9" });
  });

  it("keeps the built package's title id for Launch", async () => {
    jobStatus.mockResolvedValue({
      status: "done",
      dest: "/out/a.pkg",
      bytes_sent: 1,
      tx_id_hex: "UP4433-PPSA17221_00-MINECRAFTPS50000",
    });
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done", titleId: "PPSA17221" });
  });

  it("shows a compression job's progress though it reports no stages", async () => {
    const compressJob = vi.fn().mockResolvedValue({ job_id: "c1" });
    const { fpkg } = await import("../api/fpkg");
    (fpkg as unknown as { compress: unknown }).compress = compressJob;
    jobStatus.mockResolvedValueOnce({ status: "running", bytes_sent: 5, total_bytes: 10 });
    await useFpkgConversion.getState().compress("/games/a.exfat");
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "running",
      mode: "ffpfsc",
      stage: "compress",
      stageDone: 5,
      stageTotal: 10,
    });
  });

  it("makes a game image from a folder, showing the bytes written", async () => {
    const buildImage = vi.fn().mockResolvedValue({ job_id: "i1" });
    const { fpkg } = await import("../api/fpkg");
    (fpkg as unknown as { buildImage: unknown }).buildImage = buildImage;
    jobStatus.mockResolvedValueOnce({
      status: "running",
      bytes_sent: 3,
      total_bytes: 9,
      stage: { id: "write", index: 1, count: 3, done: 3, total: 9 },
    });
    await useFpkgConversion.getState().buildImage("/games/PPSA1-app", "/out", false);
    await tick();
    expect(buildImage).toHaveBeenCalledWith("/games/PPSA1-app", "/out", "exfat", false);
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "running",
      mode: "image",
      stage: "write",
      stageDone: 3,
      stageTotal: 9,
    });
    jobStatus.mockResolvedValueOnce({ status: "done", dest: "/out/PPSA1-app.exfat", bytes_sent: 9 });
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "done",
      mode: "image",
      packagePath: "/out/PPSA1-app.exfat",
    });
  });

  it("puts the finished image in the Upload queue when the run was asked to", async () => {
    const buildImage = vi.fn().mockResolvedValue({ job_id: "i9" });
    const { fpkg } = await import("../api/fpkg");
    (fpkg as unknown as { buildImage: unknown }).buildImage = buildImage;
    const { useUploadQueueStore } = await import("./uploadQueue");
    const added: unknown[] = [];
    const started: string[] = [];
    useUploadQueueStore.setState({
      add: (i: unknown) => void added.push(i),
      startHost: async (h: string) => void started.push(h),
    } as never);
    await useFpkgConversion.getState().buildImage("/games/W-app", "/out", true, "ffpkg", {
      host: "10.0.0.2",
      volume: null,
      subpath: "homebrew",
      deleteAfter: true,
    });
    jobStatus.mockResolvedValueOnce({ status: "done", dest: "/out/W-app.ffpfsc", bytes_sent: 9 });
    await tick();
    expect(added).toEqual([
      expect.objectContaining({
        sourceKind: "image",
        sourcePath: "/out/W-app.ffpfsc",
        resolvedDest: "/data/homebrew/W-app.ffpfsc",
        deleteSourceAfterUpload: true,
      }),
    ]);
    expect(started).toEqual(["10.0.0.2"]);
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done", uploadQueued: true });
    // A plain image run after it queues nothing.
    added.length = 0;
    useFpkgConversion.setState({ pipeline: { phase: "idle" } });
    await useFpkgConversion.getState().buildImage("/games/X-app", "/out", false);
    jobStatus.mockResolvedValueOnce({ status: "done", dest: "/out/X-app.exfat", bytes_sent: 9 });
    await tick();
    expect(added).toEqual([]);
  });

  it("asks the engine for the chosen image format", async () => {
    const buildImage = vi.fn().mockResolvedValue({ job_id: "i2" });
    const { fpkg } = await import("../api/fpkg");
    (fpkg as unknown as { buildImage: unknown }).buildImage = buildImage;
    jobStatus.mockResolvedValueOnce({ status: "done", dest: "/out/G.ffpkg", bytes_sent: 9 });
    await useFpkgConversion.getState().buildImage("/games/G", "/out", false, "ffpkg");
    await tick();
    expect(buildImage).toHaveBeenCalledWith("/games/G", "/out", "ffpkg", false);
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "done",
      packagePath: "/out/G.ffpkg",
    });
  });

  it("compresses in the same engine job, with no second job or leftover image", async () => {
    const buildImage = vi.fn().mockResolvedValue({ job_id: "i1" });
    const compressJob = vi.fn().mockResolvedValue({ job_id: "c1" });
    const { fpkg } = await import("../api/fpkg");
    (fpkg as unknown as { buildImage: unknown }).buildImage = buildImage;
    (fpkg as unknown as { compress: unknown }).compress = compressJob;
    jobStatus.mockResolvedValueOnce({
      status: "running",
      bytes_sent: 4,
      total_bytes: 9,
      stage: { id: "compress", index: 1, count: 3, done: 4, total: 9 },
    });
    await useFpkgConversion.getState().buildImage("/games/G", "/out", true, "ffpkg");
    await tick();
    expect(buildImage).toHaveBeenCalledWith("/games/G", "/out", "ffpkg", true);
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "running",
      mode: "image",
      stage: "compress",
      stageDone: 4,
    });
    jobStatus.mockResolvedValueOnce({ status: "done", dest: "/out/G.ffpfsc", bytes_sent: 5 });
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "done",
      packagePath: "/out/G.ffpfsc",
    });
    expect(compressJob).not.toHaveBeenCalled();
    expect(deletePackage).not.toHaveBeenCalled();
  });

  it("without a console, Convert & install stops at send with the package kept", async () => {
    conn.payloadStatus = "down";
    jobStatus.mockResolvedValue({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    await useFpkgConversion.getState().start(req, { install: true, host: null });
    await tick();
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "failed",
      stage: "send",
      packagePath: "/out/a.pkg",
    });
    expect(installStream).not.toHaveBeenCalled();
  });

  it("fails, not spins, when the engine stops answering", async () => {
    jobStatus.mockRejectedValue(new Error("connection refused"));
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    for (let i = 0; i < 7; i++) await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "failed",
      message: expect.stringContaining("stopped responding"),
    });
  });

  it("reports a failed build at the stage it reached, with no package", async () => {
    jobStatus
      .mockResolvedValueOnce({ status: "running", stage: { id: "write", index: 3, count: 5, done: 1, total: 2 } })
      .mockResolvedValueOnce({ status: "failed", error: "disk full" });
    await useFpkgConversion.getState().start(req, { install: true, host: "10.0.0.2" });
    await tick();
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "failed",
      stage: "write",
      message: "disk full",
      packagePath: null,
      titleId: null,
    });
    expect(installStream).not.toHaveBeenCalled();
  });

  it("ignores a reset or a second start while running, and resets a result", async () => {
    jobStatus.mockResolvedValue({ status: "running" });
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    useFpkgConversion.getState().reset();
    await useFpkgConversion.getState().start({ source: "/games/b" }, { install: false, host: null });
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "running", source: "/games/a" });
    expect(build).toHaveBeenCalledTimes(1);
    useFpkgConversion.setState({
      pipeline: {
        phase: "failed",
        mode: "convert",
        source: "/games/a",
        host: null,
        stage: "write",
        message: "x",
        packagePath: null,
        stageMs: {},
        titleId: null,
      },
    });
    useFpkgConversion.getState().reset();
    expect(useFpkgConversion.getState().pipeline.phase).toBe("idle");
  });

  it("reports the run as a task through its stages to done", async () => {
    jobStatus
      .mockResolvedValueOnce({
        status: "running",
        stage: { id: "compress", index: 2, count: 5, done: 5, total: 10 },
      })
      .mockResolvedValueOnce({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    const t = () => useTaskStore.getState().tasks.find((x) => x.kind === "fpkg-convert");
    await tick();
    expect(t()).toMatchObject({
      status: "running",
      stage: "Compress",
      label: "Convert a",
      progress: { current: 5, total: 10 },
    });
    await tick();
    expect(t()?.status).toBe("done");
  });

  it("offers Cancel only while the build runs", async () => {
    jobStatus.mockResolvedValue({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    const t = () => useTaskStore.getState().tasks.find((x) => x.kind === "fpkg-convert")!;
    expect(taskCapabilities(t()).canCancel).toBe(true);
    await tick();
    expect(taskCapabilities(t()).canCancel).toBe(false);
    expect(await commandTask(t(), "cancel")).toBe(false); // a no-op, and says so
  });

  it("ends a cancelled build as cancelled and a broken one as failed", async () => {
    jobStatus.mockResolvedValueOnce({ status: "failed", error: "the build was cancelled" });
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    await tick();
    expect(useTaskStore.getState().tasks[0]?.status).toBe("cancelled");
    useFpkgConversion.setState({ pipeline: { phase: "idle" } });
    jobStatus.mockResolvedValueOnce({ status: "failed", error: "disk full" });
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    await tick();
    const failed = useTaskStore.getState().tasks.find((x) => x.status === "failed");
    expect(failed?.lastError?.message).toBe("disk full");
  });

  it("names a compression run after its image", async () => {
    await useFpkgConversion.getState().compress("/games/b.exfat");
    expect(useTaskStore.getState().tasks[0]).toMatchObject({
      kind: "ffpfsc-compress",
      label: "Compress b.exfat",
    });
  });

  it("never lets a finished Convert row cancel the conversion running now", async () => {
    jobStatus.mockResolvedValueOnce({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    await tick();
    const first = useTaskStore.getState().tasks.find((x) => x.kind === "fpkg-convert")!;
    useFpkgConversion.getState().reset();
    jobStatus.mockResolvedValue({ status: "running" });
    build.mockResolvedValueOnce({ job_id: "j2" });
    await useFpkgConversion.getState().start({ ...req, source: "/games/b" }, { install: false, host: null });
    const second = useTaskStore.getState().tasks.find((x) => x.label === "Convert b")!;
    expect(taskCapabilities(second).canCancel).toBe(true);
    expect(taskCapabilities(first).canCancel).toBe(false);
    expect(await commandTask(first, "cancel")).toBe(false);
  });

  it("builds a server folder in place, with no copy", async () => {
    jobStatus.mockResolvedValueOnce({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    await useFpkgConversion
      .getState()
      .start({ source: "remote://nas-1/games/a", outputDir: "/out" }, { install: false, host: null });
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "running", stage: "check" });
    expect(remoteFetch).not.toHaveBeenCalled();
    expect(build).toHaveBeenCalledWith(
      expect.objectContaining({ source: "remote://nas-1/games/a", outputDir: "/out" }),
    );
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done", packagePath: "/out/a.pkg" });
    expect(cleanupFetched).not.toHaveBeenCalled();
  });

  it("fails at the copy when a server archive's copy fails, and keeps nothing to clean", async () => {
    jobStatus.mockResolvedValueOnce({ status: "failed", error: "Can't reach 10.0.0.9" });
    await useFpkgConversion
      .getState()
      .start({ source: "remote://nas-1/dl/a.zip", outputDir: "/out" }, { install: false, host: null });
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "failed",
      stage: "copy",
      message: "Can't reach 10.0.0.9",
    });
    expect(extract).not.toHaveBeenCalled();
    expect(build).not.toHaveBeenCalled();
  });

  it("copies each server archive into its own folder", async () => {
    jobStatus.mockResolvedValue({ status: "failed", error: "x" });
    await useFpkgConversion
      .getState()
      .start({ source: "remote://nas-1/dl/a.zip", outputDir: "/out" }, { install: false, host: null });
    await tick();
    const first = remoteFetch.mock.calls[0][1] as string;
    useFpkgConversion.setState({ pipeline: { phase: "idle" } });
    remoteFetch.mockClear();
    await useFpkgConversion
      .getState()
      .start({ source: "remote://nas-1/dl/a.zip", outputDir: "/out" }, { install: false, host: null });
    const second = remoteFetch.mock.calls[0][1] as string;
    expect(first.startsWith("/out/.ps5upload-source/")).toBe(true);
    expect(second).not.toBe(first);
  });

  it("unpacks a local archive, builds the game inside, then removes the unpack", async () => {
    const game = "/out/.ps5upload-extract-x1/My Game";
    jobStatus
      .mockResolvedValueOnce({ status: "running", bytes_sent: 5, total_bytes: 10 })
      .mockResolvedValueOnce({ status: "done", dest: game, bytes_sent: 10 })
      .mockResolvedValueOnce({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    await useFpkgConversion
      .getState()
      .start({ source: "/dl/game.7z", outputDir: "/out" }, { install: false, host: null });
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "running", stage: "extract" });
    expect(extract).toHaveBeenCalledWith("/dl/game.7z", "/out", undefined);
    await vi.advanceTimersByTimeAsync(1);
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      stage: "extract",
      stageDone: 5,
      stageTotal: 10,
    });
    await tick();
    expect(build).toHaveBeenCalledWith(expect.objectContaining({ source: game, outputDir: "/out" }));
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "done",
      source: "/dl/game.7z",
      packagePath: "/out/a.pkg",
    });
    expect(cleanupExtract).toHaveBeenCalledWith(game);
  });

  it("copies a server archive, unpacks the copy, and removes both", async () => {
    const game = "/out/.ps5upload-extract-x1/G";
    jobStatus
      .mockResolvedValueOnce({ status: "done", dest: "/out/.ps5upload-source/r/g.zip", bytes_sent: 1 })
      .mockResolvedValueOnce({ status: "done", dest: game, bytes_sent: 1 })
      .mockResolvedValueOnce({ status: "failed", error: "disk full" });
    await useFpkgConversion
      .getState()
      .start({ source: "remote://nas-1/dl/g.zip", outputDir: "/out" }, { install: false, host: null });
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ stage: "copy" });
    await tick();
    await tick();
    expect(extract).toHaveBeenCalledWith("/out/.ps5upload-source/r/g.zip", "/out", undefined);
    await tick();
    await tick();
    expect(build).toHaveBeenCalledWith(expect.objectContaining({ source: game }));
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "failed", message: "disk full" });
    expect(cleanupFetched).toHaveBeenCalledWith("/out/.ps5upload-source/r/g.zip");
    expect(cleanupExtract).toHaveBeenCalledWith(game);
  });

  it("passes a RAR password, and says plainly when one is needed", async () => {
    jobStatus.mockResolvedValueOnce({ status: "failed", error: "rar_password_required" });
    await useFpkgConversion
      .getState()
      .start(
        { source: "/dl/g.part1.rar", outputDir: "/out" },
        { install: false, host: null, password: "pw" },
      );
    expect(extract).toHaveBeenCalledWith("/dl/g.part1.rar", "/out", "pw");
    await tick();
    const p = useFpkgConversion.getState().pipeline;
    expect(p).toMatchObject({ phase: "failed", stage: "extract" });
    expect(p.phase === "failed" && p.message).toMatch(/password/i);
    expect(p.phase === "failed" && p.message).not.toMatch(/rar_password/);
    expect(build).not.toHaveBeenCalled();
  });

  it("copies a server image before compressing it (compression reads local disk)", async () => {
    jobStatus.mockResolvedValueOnce({ status: "failed", error: "x" });
    await useFpkgConversion.getState().compress("remote://nas-1/g.exfat", "/out");
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ stage: "copy" });
    expect(remoteFetch).toHaveBeenCalled();
  });

  it("swaps a console dump for its package instead of installing beside it", async () => {
    const journal = { v: 1, titleId: "PPSA30528", dump: "/data/homebrew/G.exfat", parked: "/data/ps5upload/parked/G.exfat" };
    jobStatus.mockResolvedValue({
      status: "done",
      dest: "/out/a.pkg",
      bytes_sent: 1,
      tx_id_hex: "UP0000-PPSA30528_00-X000000000000000",
    });
    runSwap.mockImplementation(async (_i: unknown, _d: unknown, onStep: (s: string) => void) => {
      onStep("park");
      onStep("install");
      return { ok: true, journal };
    });
    await useFpkgConversion
      .getState()
      .start({ source: "ps5://10.0.0.2/data/homebrew/G.exfat", outputDir: "/out" }, { install: true, host: "10.0.0.2" });
    await tick();
    await tick();
    expect(installStream).not.toHaveBeenCalled();
    expect(runSwap).toHaveBeenCalledWith(
      { titleId: "PPSA30528", dump: "/data/homebrew/G.exfat", packagePath: "/out/a.pkg" },
      expect.anything(),
      expect.any(Function),
    );
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done", swap: journal });
    await useFpkgConversion.getState().finishReplace("delete");
    expect(finishSwap).toHaveBeenCalledWith(journal, "delete", expect.anything());
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done", swap: null });
  });

  it("reports a swap that was rolled back as a failed install", async () => {
    jobStatus.mockResolvedValue({
      status: "done",
      dest: "/out/a.pkg",
      bytes_sent: 1,
      tx_id_hex: "UP0000-PPSA30528_00-X000000000000000",
    });
    runSwap.mockResolvedValue({ ok: false, rolledBack: true, message: "The install did not finish. The dump is back where it was." });
    await useFpkgConversion
      .getState()
      .start({ source: "ps5://10.0.0.2/data/homebrew/G.exfat", outputDir: "/out" }, { install: true, host: "10.0.0.2" });
    await tick();
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "failed",
      message: "The install did not finish. The dump is back where it was.",
      packagePath: "/out/a.pkg",
    });
  });
});

