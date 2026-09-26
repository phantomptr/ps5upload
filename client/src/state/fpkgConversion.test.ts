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
const deletePackage = vi.fn(async () => ({ ok: true }));
const jobStatus = vi.fn();
const installStream = vi.fn();

vi.mock("../api/fpkg", () => ({
  fpkg: {
    build: (...a: unknown[]) => build(...a),
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
    getState: () => ({ installStream: (...a: unknown[]) => installStream(...a) }),
  }),
}));
vi.mock("./notifications", () => ({ pushNotification: vi.fn() }));
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
    jobStatus.mockReset();
    installStream.mockReset();
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

  it("copies a server source first, builds the copy, then removes the copy", async () => {
    jobStatus
      .mockResolvedValueOnce({ status: "running", bytes_sent: 5, total_bytes: 10 })
      .mockResolvedValueOnce({ status: "done", dest: "/out/.ps5upload-source/a", bytes_sent: 10 })
      .mockResolvedValueOnce({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    await useFpkgConversion
      .getState()
      .start({ source: "remote://nas-1/games/a", outputDir: "/out" }, { install: false, host: null });
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "running", stage: "copy" });
    expect(remoteFetch).toHaveBeenCalledWith("remote://nas-1/games/a", "/out/.ps5upload-source");
    await tick();
    await tick();
    expect(build).toHaveBeenCalledWith(
      expect.objectContaining({ source: "/out/.ps5upload-source/a", outputDir: "/out" }),
    );
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "done",
      source: "remote://nas-1/games/a",
    });
    expect(cleanupFetched).toHaveBeenCalledWith("/out/.ps5upload-source/a");
  });

  it("fails at the copy when the server copy fails, and keeps nothing to clean", async () => {
    jobStatus.mockResolvedValueOnce({ status: "failed", error: "Can't reach 10.0.0.9" });
    await useFpkgConversion
      .getState()
      .start({ source: "remote://nas-1/games/a", outputDir: "/out" }, { install: false, host: null });
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "failed",
      stage: "copy",
      message: "Can't reach 10.0.0.9",
    });
    expect(build).not.toHaveBeenCalled();
  });
});
