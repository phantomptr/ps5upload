import { beforeEach, describe, expect, it, vi } from "vitest";

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

import { POLL_MS, useFpkgConversion } from "./fpkgConversion";

const req = { source: "/games/a", outputDir: "/out" };
const tick = () => vi.advanceTimersByTimeAsync(POLL_MS + 50);

describe("fpkg pipeline", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useFpkgConversion.setState({ pipeline: { phase: "idle" } });
    build.mockReset().mockResolvedValue({ job_id: "j1" });
    jobStatus.mockReset();
    installStream.mockReset();
    deletePackage.mockClear();
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

  it("without a console, Convert & install stops at send with the package kept", async () => {
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
      },
    });
    useFpkgConversion.getState().reset();
    expect(useFpkgConversion.getState().pipeline.phase).toBe("idle");
  });
});
