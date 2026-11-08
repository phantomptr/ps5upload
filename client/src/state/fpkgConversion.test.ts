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
const jobCancel = vi.fn();
const jobPause = vi.fn();
const jobResume = vi.fn();
const installStream = vi.fn();
const runningJobs = vi.fn(async (..._a: unknown[]): Promise<unknown[]> => []);
const inspect = vi.fn(async (..._a: unknown[]): Promise<{ content_id?: string | null }> => ({}));
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
    runningJobs: (...a: unknown[]) => runningJobs(...a),
    inspect: (...a: unknown[]) => inspect(...a),
    deletePackage: (...a: unknown[]) => deletePackage(...(a as [])),
  },
}));
vi.mock("../api/ps5", () => ({
  jobStatus: (...a: unknown[]) => jobStatus(...a),
  jobCancel: (...a: unknown[]) => jobCancel(...a),
  jobPause: (...a: unknown[]) => jobPause(...a),
  jobResume: (...a: unknown[]) => jobResume(...a),
  fetchVolumes: async () => [],
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
// The roster the recorded install target is checked against.
const roster = { profiles: [] as { id: string; name: string; host: string }[] };
vi.mock("./roster", async (orig) => ({
  ...(await orig<typeof import("./roster")>()),
  useRosterStore: { getState: () => roster },
}));

import { POLL_MS, archiveStem, pausedSoFar, stagePausedSoFar, useFpkgConversion } from "./fpkgConversion";
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
    runningJobs.mockReset().mockResolvedValue([]);
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
    roster.profiles = [];
    jobCancel.mockReset().mockResolvedValue(undefined);
    jobPause.mockReset().mockResolvedValue(undefined);
    jobResume.mockReset().mockResolvedValue(undefined);
  });

  it("pauses and resumes the build job, keeping paused time out of its times (#631)", async () => {
    const running = {
      status: "running",
      stage: { id: "compress", index: 2, count: 5, done: 5, total: 10 },
    };
    jobStatus.mockResolvedValue(running);
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    await tick();
    await useFpkgConversion.getState().pause();
    expect(jobPause).toHaveBeenCalledWith("j1");
    let p = useFpkgConversion.getState().pipeline;
    expect(p).toMatchObject({ phase: "running", paused: true });
    const taskId = p.phase === "running" ? p.taskId : null;
    expect(useTaskStore.getState().tasks.find((t) => t.id === taskId)?.status).toBe("paused");
    // The engine keeps saying paused: the pause holds across polls.
    jobStatus.mockResolvedValue({ ...running, paused: true, paused_ms: 1000 });
    await vi.advanceTimersByTimeAsync(3000);
    p = useFpkgConversion.getState().pipeline;
    expect(p).toMatchObject({ phase: "running", paused: true });
    if (p.phase !== "running") throw new Error("not running");
    const now = Date.now();
    expect(pausedSoFar(p, now)).toBeGreaterThanOrEqual(3000);
    expect(stagePausedSoFar(p, now)).toBeGreaterThanOrEqual(3000);
    // A second Pause does nothing; Resume asks the engine.
    await useFpkgConversion.getState().pause();
    expect(jobPause).toHaveBeenCalledTimes(1);
    await useFpkgConversion.getState().resume();
    expect(jobResume).toHaveBeenCalledWith("j1");
    p = useFpkgConversion.getState().pipeline;
    expect(p).toMatchObject({ phase: "running", paused: false, pausedAtMs: null });
    if (p.phase !== "running") throw new Error("not running");
    const kept = pausedSoFar(p, Date.now());
    expect(kept).toBeGreaterThanOrEqual(3000);
    // Not paused any more: the paused total stands still.
    jobStatus.mockResolvedValue(running);
    await vi.advanceTimersByTimeAsync(2000);
    p = useFpkgConversion.getState().pipeline;
    if (p.phase !== "running") throw new Error("not running");
    expect(pausedSoFar(p, Date.now())).toBe(kept);
    expect(useTaskStore.getState().tasks.find((t) => t.id === taskId)?.status).toBe("running");
  });

  it("follows a pause made elsewhere (another window, the API)", async () => {
    jobStatus.mockResolvedValue({ status: "running", stage: { id: "write", index: 3, count: 5, done: 1, total: 4 }, paused: true });
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "running", paused: true });
    jobStatus.mockResolvedValue({ status: "done", dest: "/out/a.pkg", bytes_sent: 9 });
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done" });
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

  it("deletes the package once its install is verified when asked, and only then", async () => {
    jobStatus.mockResolvedValue({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    installStream.mockResolvedValueOnce({ ok: false, message: "unreachable" });
    await useFpkgConversion.getState().start(req, { install: true, host: "10.0.0.2", deleteAfter: true });
    await tick();
    await tick();
    expect(deletePackage).not.toHaveBeenCalled();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "failed", packagePath: "/out/a.pkg" });

    installStream.mockResolvedValue({ ok: true });
    await useFpkgConversion.getState().start(req, { install: true, host: "10.0.0.2", deleteAfter: true });
    await tick();
    await tick();
    await vi.waitFor(() =>
      expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done", deleted: true }),
    );
    expect(deletePackage).toHaveBeenCalledWith("/out/a.pkg");

    // A later run that did not ask keeps its package.
    deletePackage.mockClear();
    await useFpkgConversion.getState().start(req, { install: true, host: "10.0.0.2" });
    await tick();
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done", deleted: false });
    expect(deletePackage).not.toHaveBeenCalled();
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

  it("installs on the console chosen when Convert & install was pressed, not the one switched to", async () => {
    jobStatus
      .mockResolvedValueOnce({ status: "running" })
      .mockResolvedValue({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    installStream.mockResolvedValue({ ok: true });
    roster.profiles = [
      { id: "a", name: "Pro", host: "10.0.0.2" },
      { id: "b", name: "Phat", host: "10.0.0.9" },
    ];
    await useFpkgConversion.getState().start(req, { install: true, host: "10.0.0.2" });
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "running", host: "10.0.0.2" });
    await tick();
    conn.host = "10.0.0.9"; // the user switched consoles during the build
    await tick();
    await tick();
    expect(installStream).toHaveBeenCalledWith("/out/a.pkg", "10.0.0.2", expect.anything());
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done", host: "10.0.0.2" });
  });

  it("stops with the package kept when the chosen console left the roster during the build", async () => {
    jobStatus.mockResolvedValue({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    roster.profiles = [{ id: "b", name: "Phat", host: "10.0.0.9" }];
    await useFpkgConversion.getState().start(req, { install: true, host: "10.0.0.2" });
    await tick();
    await tick();
    expect(installStream).not.toHaveBeenCalled();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "failed",
      stage: "send",
      packagePath: "/out/a.pkg",
      message: expect.stringContaining("no longer in your consoles"),
    });
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

  it("sends the fast check by default and the full one when it is on", async () => {
    const { useConvertPrefs } = await import("./convertPrefs");
    const req = { source: "/games/a", outputDir: "/out" };
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    expect(build).toHaveBeenLastCalledWith(expect.objectContaining({ fullVerify: false }));
    useFpkgConversion.setState({ pipeline: { phase: "idle" } });
    useConvertPrefs.getState().setFullVerify(true);
    try {
      await useFpkgConversion.getState().start(req, { install: false, host: null });
      expect(build).toHaveBeenLastCalledWith(expect.objectContaining({ fullVerify: true }));
    } finally {
      useConvertPrefs.getState().setFullVerify(false);
    }
  });

  // A Discord report (on a build before 7.1.14): a game kept as a .rar could not become a
  // compressed image ("Make compressed image" greyed, the queue said "Skipped: 1"). A .rar is
  // unpacked first, then written straight into a .ffpfsc named after the archive.
  it("makes a compressed image from a .rar: unpacked first, named after the archive", async () => {
    const buildImage = vi.fn().mockResolvedValue({ job_id: "i7" });
    const { fpkg } = await import("../api/fpkg");
    (fpkg as unknown as { buildImage: unknown }).buildImage = buildImage;
    const game = "/out/.ps5upload-extract-x1/PPSA99999-app";
    jobStatus
      .mockResolvedValueOnce({ status: "done", dest: game, bytes_sent: 10 })
      .mockResolvedValueOnce({ status: "done", dest: "/out/My Game.ffpfsc", bytes_sent: 4 });
    await useFpkgConversion
      .getState()
      .buildImage("/dl/My Game.part1.rar", "/out", true, "ffpkg", undefined, undefined, "pw");
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "running", stage: "extract" });
    expect(extract).toHaveBeenCalledWith("/dl/My Game.part1.rar", "/out", "pw");
    await tick();
    expect(buildImage).toHaveBeenCalledWith(game, "/out", "ffpkg", true, undefined, 8192, false, "My Game", "balanced", undefined, { add: true, build: "ampr_emu" });
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "done",
      mode: "image",
      packagePath: "/out/My Game.ffpfsc",
    });
    expect(cleanupExtract).toHaveBeenCalledWith(game);
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
    expect(buildImage).toHaveBeenCalledWith("/games/PPSA1-app", "/out", "exfat", false, undefined, 8192, false, undefined, "balanced", undefined, { add: true, build: "ampr_emu" });
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

  it("unpacks an image or package into a folder: write, then verify, ending at the folder", async () => {
    const buildFolder = vi.fn().mockResolvedValue({ job_id: "f1" });
    const { fpkg } = await import("../api/fpkg");
    (fpkg as unknown as { buildFolder: unknown }).buildFolder = buildFolder;
    jobStatus.mockResolvedValueOnce({
      status: "running",
      bytes_sent: 4,
      total_bytes: 9,
      stage: { id: "verify", index: 1, count: 2, done: 2, total: 5 },
    });
    await useFpkgConversion.getState().buildFolder("/games/Game.ffpfsc", "/out");
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "running", mode: "folder", stage: "write" });
    await tick();
    expect(buildFolder).toHaveBeenCalledWith("/games/Game.ffpfsc", "/out", undefined, false, undefined, undefined, undefined);
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ stage: "verify", stageDone: 2, stageTotal: 5 });
    jobStatus.mockResolvedValueOnce({ status: "done", dest: "/out/Game", bytes_sent: 9 });
    await tick();
    const p = useFpkgConversion.getState().pipeline;
    expect(p).toMatchObject({ phase: "done", mode: "folder", packagePath: "/out/Game" });
    // A folder is never installed.
    await useFpkgConversion.getState().retryInstall("10.0.0.2");
    expect(useFpkgConversion.getState().pipeline).toBe(p);
  });

  it("sends a PS4 update to merge with the game's folder or image build", async () => {
    const buildFolder = vi.fn().mockResolvedValue({ job_id: "m1" });
    const buildImage = vi.fn().mockResolvedValue({ job_id: "m2" });
    const { fpkg } = await import("../api/fpkg");
    (fpkg as unknown as { buildFolder: unknown }).buildFolder = buildFolder;
    (fpkg as unknown as { buildImage: unknown }).buildImage = buildImage;
    jobStatus.mockResolvedValue({ status: "done", dest: "/out/G", bytes_sent: 9 });
    await useFpkgConversion.getState().buildFolder("/games/G.pkg", "/out", false, { update: "/games/U.pkg" });
    expect(buildFolder).toHaveBeenCalledWith(
      "/games/G.pkg", "/out", undefined, false, undefined, false, undefined, "/games/U.pkg",
    );
    await tick();
    useFpkgConversion.getState().reset();
    await useFpkgConversion
      .getState()
      .buildImage("/games/G.pkg", "/out", false, "ffpkg", undefined, undefined, undefined, false, { update: "/games/U.pkg" });
    const call = buildImage.mock.calls[0] ?? [];
    expect(call[call.length - 1]).toBe("/games/U.pkg");
    await tick();
    jobStatus.mockReset();
  });

  it("sends a PS4 update to merge with the game's package build, which offers install like any package", async () => {
    build.mockResolvedValueOnce({ job_id: "p1" });
    jobStatus.mockResolvedValue({
      status: "done",
      dest: "/out/UP0000-CUSA99990_00-TESTGAME00000000.pkg",
      bytes_sent: 9,
      tx_id_hex: "UP0000-CUSA99990_00-TESTGAME00000000",
    });
    await useFpkgConversion
      .getState()
      .start({ source: "/games/G.pkg", outputDir: "/out", update: "/games/U.pkg" }, { install: false, host: null });
    expect(build).toHaveBeenCalledWith(expect.objectContaining({ source: "/games/G.pkg", update: "/games/U.pkg" }));
    await tick();
    const p = useFpkgConversion.getState().pipeline;
    expect(p).toMatchObject({ phase: "done", mode: "convert", packagePath: "/out/UP0000-CUSA99990_00-TESTGAME00000000.pkg" });
    useFpkgConversion.getState().reset();
    jobStatus.mockReset();
  });

  it("packs a game folder into a folder in zstd packs: pack first, the packs sent with the build", async () => {
    const buildFolder = vi.fn().mockResolvedValue({ job_id: "f2" });
    const { fpkg } = await import("../api/fpkg");
    (fpkg as unknown as { buildFolder: unknown }).buildFolder = buildFolder;
    jobStatus.mockResolvedValueOnce({
      status: "running",
      bytes_sent: 4,
      total_bytes: 9,
      stage: { id: "pack", index: 0, count: 3, done: 4, total: 18 },
    });
    const zstd = { level: 19, runtime: "pinned" as const };
    await useFpkgConversion.getState().buildFolder("/games/PPSA1-app", "/out", true, { zstd });
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "running", mode: "folder", stage: "pack" });
    await tick();
    expect(buildFolder).toHaveBeenCalledWith("/games/PPSA1-app", "/out", undefined, false, undefined, true, zstd);
    jobStatus.mockResolvedValueOnce({ status: "done", dest: "/out/PPSA1-app", bytes_sent: 9 });
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done", mode: "folder" });
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
      startAdded: (h: string) => void started.push(h),
    } as never);
    await useFpkgConversion.getState().buildImage("/games/W-app", "/out", true, "ffpkg", {
      host: "10.0.0.2",
      volume: null,
      subpath: "homebrew",
      deleteAfter: true,
    });
    jobStatus.mockResolvedValueOnce({ status: "done", dest: "/out/W-app.ffpfsc", bytes_sent: 9 });
    await tick();
    await tick(); // Its drive is decided with the console's drive list (#673).
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
    expect(buildImage).toHaveBeenCalledWith("/games/G", "/out", "ffpkg", false, undefined, 8192, false, undefined, "balanced", undefined, { add: true, build: "ampr_emu" });
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "done",
      packagePath: "/out/G.ffpkg",
    });
  });

  it("writes an image straight to the PS5 when it is saved there, and queues no upload", async () => {
    const buildImage = vi.fn().mockResolvedValue({ job_id: "i9" });
    const { fpkg } = await import("../api/fpkg");
    (fpkg as unknown as { buildImage: unknown }).buildImage = buildImage;
    jobStatus.mockResolvedValueOnce({ status: "done", dest: "/data/homebrew/G.exfat", bytes_sent: 9 });
    const plan = { host: "10.0.0.2", volume: "/data", subpath: "homebrew", deleteAfter: false };
    await useFpkgConversion
      .getState()
      .buildImage("/games/G", "/out", false, "exfat", undefined, undefined, undefined, false, { toConsole: plan });
    await tick();
    expect(buildImage).toHaveBeenCalledWith(
      "/games/G",
      "/out",
      "exfat",
      false,
      undefined,
      8192,
      false,
      undefined,
      "balanced",
      false,
      { add: true, build: "ampr_emu" },
      { kind: "console", addr: "10.0.0.2", dir: "/data/homebrew" },
    );
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "done",
      packagePath: "/data/homebrew/G.exfat",
      onConsole: true,
      uploadQueued: false,
    });
  });

  it("writes an image straight to a server folder, and queues no upload nor offers a delete", async () => {
    const buildImage = vi.fn().mockResolvedValue({ job_id: "s1" });
    const deletePackage = vi.fn().mockResolvedValue({ ok: true });
    const { fpkg } = await import("../api/fpkg");
    (fpkg as unknown as { buildImage: unknown }).buildImage = buildImage;
    (fpkg as unknown as { deletePackage: unknown }).deletePackage = deletePackage;
    jobStatus.mockResolvedValueOnce({ status: "done", dest: "remote://nas1/games/ps5/G.exfat", bytes_sent: 9 });
    const plan = { host: "10.0.0.2", volume: "/data", subpath: "homebrew", deleteAfter: false };
    await useFpkgConversion
      .getState()
      .buildImage("/games/G", "/out", false, "exfat", plan, undefined, undefined, false, {
        toServer: "remote://nas1/games/ps5",
      });
    await tick();
    const call = buildImage.mock.calls[0] ?? [];
    expect(call[call.length - 1]).toEqual({ kind: "server", source_id: "nas1", dir: "/games/ps5" });
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "done",
      packagePath: "remote://nas1/games/ps5/G.exfat",
      uploadQueued: false,
    });
    await useFpkgConversion.getState().deletePackage();
    expect(deletePackage).not.toHaveBeenCalled();
  });

  it("writes a game folder straight to a server folder: the destination after the update slot", async () => {
    const buildFolder = vi.fn().mockResolvedValue({ job_id: "s2" });
    const { fpkg } = await import("../api/fpkg");
    (fpkg as unknown as { buildFolder: unknown }).buildFolder = buildFolder;
    jobStatus.mockResolvedValueOnce({ status: "done", dest: "remote://nas1/games/G", bytes_sent: 9 });
    await useFpkgConversion
      .getState()
      .buildFolder("/games/G.ffpkg", "/out", false, { toServer: "remote://nas1/games" });
    await tick();
    expect(buildFolder).toHaveBeenCalledWith(
      "/games/G.ffpkg",
      "/out",
      undefined,
      false,
      undefined,
      false,
      undefined,
      undefined,
      { kind: "server", source_id: "nas1", dir: "/games" },
    );
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done", packagePath: "remote://nas1/games/G" });
  });

  it("sends a PS4 game merged with its update straight to the PS5: the destination, then the update", async () => {
    const buildImage = vi.fn().mockResolvedValue({ job_id: "i10" });
    const { fpkg } = await import("../api/fpkg");
    (fpkg as unknown as { buildImage: unknown }).buildImage = buildImage;
    jobStatus.mockResolvedValueOnce({ status: "done", dest: "/data/homebrew/G.ffpkg", bytes_sent: 9 });
    const plan = { host: "10.0.0.2", volume: "/data", subpath: "homebrew", deleteAfter: false };
    await useFpkgConversion
      .getState()
      .buildImage("/games/G.pkg", "/out", false, "ffpkg", undefined, undefined, undefined, false, {
        toConsole: plan,
        update: "/games/U.pkg",
      });
    await tick();
    const call = buildImage.mock.calls[0] ?? [];
    expect(call.slice(-2)).toEqual([{ kind: "console", addr: "10.0.0.2", dir: "/data/homebrew" }, "/games/U.pkg"]);
  });

  it("packs LZ4 assets first when asked, and follows the engine's pack stage", async () => {
    const buildImage = vi.fn().mockResolvedValue({ job_id: "i3" });
    const { fpkg } = await import("../api/fpkg");
    (fpkg as unknown as { buildImage: unknown }).buildImage = buildImage;
    jobStatus.mockResolvedValueOnce({
      status: "running",
      stage: { id: "pack", index: 0, count: 4, done: 5, total: 10 },
    });
    const lz4 = { profileToml: "[pack]\n" };
    await useFpkgConversion.getState().buildImage("/games/A", "/out", false, "exfat", undefined, lz4);
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "running", stage: "pack" });
    await tick();
    expect(buildImage).toHaveBeenCalledWith("/games/A", "/out", "exfat", false, lz4, 8192, false, undefined, "balanced", undefined, { add: true, build: "ampr_emu" });
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      stage: "pack",
      stageDone: 5,
      stageTotal: 10,
    });
    jobStatus.mockResolvedValueOnce({ status: "done", dest: "/out/A.exfat", bytes_sent: 9 });
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done", packagePath: "/out/A.exfat" });
  });

  it("fails at the pack stage when the engine refuses the packs", async () => {
    const buildImage = vi.fn().mockRejectedValue(new Error("the game's fakelib/libSceAmpr.sprx does not serve asset packs"));
    const { fpkg } = await import("../api/fpkg");
    (fpkg as unknown as { buildImage: unknown }).buildImage = buildImage;
    await useFpkgConversion.getState().buildImage("/games/B", "/out", false, "exfat", undefined, {});
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "failed",
      stage: "pack",
      message: expect.stringContaining("does not serve asset packs"),
    });
  });

  it("sends the chosen .ffpfsc level with a compressed image", async () => {
    const buildImage = vi.fn().mockResolvedValue({ job_id: "lv" });
    jobStatus.mockResolvedValue({ status: "done", dest: "/out/S.ffpfsc", bytes_sent: 1 });
    const { fpkg } = await import("../api/fpkg");
    (fpkg as unknown as { buildImage: unknown }).buildImage = buildImage;
    const { useConvertPrefs } = await import("./convertPrefs");
    useConvertPrefs.getState().setImageLevel("smallest");
    try {
      await useFpkgConversion.getState().buildImage("/games/S", "/out", true, "ffpkg");
      await tick();
      expect(buildImage).toHaveBeenCalledWith("/games/S", "/out", "ffpkg", true, undefined, 8192, false, undefined, "smallest", undefined, { add: true, build: "ampr_emu" });
    } finally {
      useConvertPrefs.getState().setImageLevel("balanced");
      useFpkgConversion.setState({ pipeline: { phase: "idle" } });
    }
  });

  it("compresses in the same engine job, with no second job or leftover image", async () => {
    const buildImage = vi.fn().mockResolvedValue({ job_id: "i1" });
    const { fpkg } = await import("../api/fpkg");
    (fpkg as unknown as { buildImage: unknown }).buildImage = buildImage;
    jobStatus.mockResolvedValueOnce({
      status: "running",
      bytes_sent: 4,
      total_bytes: 9,
      stage: { id: "compress", index: 1, count: 3, done: 4, total: 9 },
    });
    await useFpkgConversion.getState().buildImage("/games/G", "/out", true, "ffpkg");
    await tick();
    expect(buildImage).toHaveBeenCalledWith("/games/G", "/out", "ffpkg", true, undefined, 8192, false, undefined, "balanced", undefined, { add: true, build: "ampr_emu" });
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

  it("fails, not spins, when the engine stops answering, and stops the build it gave up on", async () => {
    jobStatus.mockRejectedValue(new Error("connection refused"));
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    for (let i = 0; i < 7; i++) await tick();
    expect(jobCancel).toHaveBeenCalledWith("j1");
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "failed",
      message: expect.stringContaining("conversion was stopped"),
    });
  });

  it("says the build may still run when even the stop cannot reach the engine", async () => {
    jobStatus.mockRejectedValue(new Error("connection refused"));
    jobCancel.mockRejectedValue(new Error("connection refused"));
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    for (let i = 0; i < 7; i++) await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "failed",
      message: expect.stringContaining("may still be running"),
    });
  });

  it("tolerates a few missed polls without failing", async () => {
    jobStatus
      .mockRejectedValueOnce(new Error("timeout"))
      .mockRejectedValueOnce(new Error("timeout"))
      .mockResolvedValue({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    for (let i = 0; i < 4; i++) await tick();
    expect(jobCancel).not.toHaveBeenCalled();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done" });
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

  it("a cancel is said as a cancel: no error notification", async () => {
    const { pushNotification } = await import("./notifications");
    vi.mocked(pushNotification).mockClear();
    jobStatus.mockResolvedValueOnce({ status: "failed", error: "the build was cancelled" });
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "failed", cancelled: true });
    expect(pushNotification).not.toHaveBeenCalled();
  });

  it("has nothing to follow when the engine answers no list", async () => {
    for (const answer of [null, {}, undefined]) {
      runningJobs.mockResolvedValueOnce(answer as never);
      await expect(useFpkgConversion.getState().attachRunning()).resolves.toBeUndefined();
      expect(useFpkgConversion.getState().pipeline.phase).toBe("idle");
    }
  });

  it("follows a build left running by a reload, and leaves its result for the queue", async () => {
    runningJobs.mockResolvedValueOnce([{ job_id: "j9", kind: "image", source: "/g/A", started_at_ms: 5 }]);
    await useFpkgConversion.getState().attachRunning();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "running",
      mode: "image",
      source: "/g/A",
      jobId: "j9",
      attached: true,
    });
    jobStatus.mockResolvedValueOnce({ status: "done", dest: "/out/A.exfat", bytes_sent: 3 });
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "done",
      packagePath: "/out/A.exfat",
      attached: true,
    });
  });

  it("does not take over a window that is already busy", async () => {
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    runningJobs.mockResolvedValueOnce([{ job_id: "j9", kind: "image", source: "/g/A", started_at_ms: 5 }]);
    await useFpkgConversion.getState().attachRunning();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ source: "/games/a" });
  });

  it("an unpack whose page closed is dropped and the user is told", async () => {
    runningJobs.mockResolvedValueOnce([{ job_id: "x9", kind: "extract", source: "/g/A.rar", started_at_ms: 5 }]);
    jobStatus.mockResolvedValueOnce({ status: "done", dest: "/out/.ps5upload-extract-1/G" });
    await useFpkgConversion.getState().attachRunning();
    await tick();
    expect(cleanupExtract).toHaveBeenCalled();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "failed", source: "/g/A.rar" });
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

  it("says after unpacking when the game inside would not play as a package here", async () => {
    const { pushNotification } = await import("./notifications");
    vi.mocked(pushNotification).mockClear();
    const was = (conn as { ps5Kernel?: string | null }).ps5Kernel;
    (conn as { ps5Kernel?: string | null }).ps5Kernel = "r229358/releases/13.60";
    inspect.mockResolvedValueOnce({ content_id: "UP9000-PPSA03671_00-X" });
    jobStatus
      .mockResolvedValueOnce({ status: "done", dest: "/out/.ps5upload-extract-x2/G", bytes_sent: 1 })
      .mockResolvedValueOnce({ status: "failed", error: "stop here" });
    await useFpkgConversion.getState().start({ source: "/dl/g.7z", outputDir: "/out" }, { install: false, host: null });
    await tick();
    await tick();
    expect(inspect).toHaveBeenCalledWith("/out/.ps5upload-extract-x2/G", "/out");
    expect(pushNotification).toHaveBeenCalledWith("warning", expect.anything(), expect.objectContaining({ link: "/workshop" }));
    (conn as { ps5Kernel?: string | null }).ps5Kernel = was;
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

  it("unpacks a .rar before making a compressed image of it, named after the archive", async () => {
    // Discord report: an archive source could not become an image at all.
    const buildImage = vi.fn().mockResolvedValue({ job_id: "i7" });
    const { fpkg } = await import("../api/fpkg");
    (fpkg as unknown as { buildImage: unknown }).buildImage = buildImage;
    const game = "/out/.ps5upload-extract-x1";
    jobStatus
      .mockResolvedValueOnce({ status: "done", dest: game, bytes_sent: 10 })
      .mockResolvedValueOnce({ status: "done", dest: "/out/My Game.ffpfsc", bytes_sent: 4 });
    await useFpkgConversion
      .getState()
      .buildImage("/dl/My Game.part1.rar", "/out", true, "exfat", undefined, undefined, "pw");
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "running", mode: "image", stage: "extract" });
    expect(extract).toHaveBeenCalledWith("/dl/My Game.part1.rar", "/out", "pw");
    await tick();
    expect(buildImage).toHaveBeenCalledWith(game, "/out", "exfat", true, undefined, 8192, false, "My Game", "balanced", undefined, { add: true, build: "ampr_emu" });
    await tick();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({
      phase: "done",
      mode: "image",
      source: "/dl/My Game.part1.rar",
      packagePath: "/out/My Game.ffpfsc",
    });
    expect(cleanupExtract).toHaveBeenCalledWith(game);
  });

  it("names an archive's image after the archive", () => {
    expect(archiveStem("/dl/My Game.part01.rar")).toBe("My Game");
    expect(archiveStem("C:\\dl\\G.v1.02.7z")).toBe("G.v1.02");
    expect(archiveStem("remote://nas-1/dl/a.ZIP")).toBe("a");
    expect(archiveStem("/dl/Game..rar")).toBe("Game");
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

