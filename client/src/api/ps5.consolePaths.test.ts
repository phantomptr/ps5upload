import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";

import {
  fsPathExists,
  fsPathPresent,
  isPathMissingError,
  probeDestination,
  smpManualInstall,
} from "./ps5";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));
vi.mock("../state/engine", () => ({ getEngineUrl: () => "http://engine.test:19113" }));

const call = vi.mocked(invoke);
const MISSING = "payload rejected FS_STAT(/x): fs_stat_failed_errno_2";

/** The engine's answer to /api/ps5/fs/stat, one per call (the last one repeats). */
function stats(...answers: Array<{ error: string } | "file">) {
  let i = 0;
  const fetcher = vi.fn(async () => {
    const a = answers[Math.min(i++, answers.length - 1)];
    return a === "file"
      ? new Response(JSON.stringify({ kind: "file", size: 7, mtime: 1, mode: 420, dev: 1 }), { status: 200 })
      : new Response(JSON.stringify(a), { status: 502 });
  });
  vi.stubGlobal("fetch", fetcher);
  return fetcher;
}

const b64 = (s: string) => btoa(s);

beforeEach(() => {
  call.mockReset();
});
afterEach(() => vi.unstubAllGlobals());

describe("isPathMissingError", () => {
  it("is true only for no such path", () => {
    expect(isPathMissingError(new Error(MISSING))).toBe(true);
    expect(isPathMissingError("fs_list_dir_opendir_errno_20")).toBe(true);
    expect(isPathMissingError("fs_list_dir_opendir_errno_13")).toBe(false);
    // errno 2 must not match the start of errno 22 or 20x.
    expect(isPathMissingError("x_errno_22")).toBe(false);
    expect(isPathMissingError("mgmt read timed out")).toBe(false);
  });
});

describe("is a path there", () => {
  it("tells absent from don't know", async () => {
    stats("file");
    await expect(fsPathPresent("10.0.0.5", "/data/x")).resolves.toBe(true);
    stats({ error: MISSING });
    await expect(fsPathPresent("10.0.0.5", "/data/x")).resolves.toBe(false);
    stats({ error: "mgmt read timed out" });
    await expect(fsPathPresent("10.0.0.5", "/data/x")).rejects.toThrow("timed out");
  });

  it("stays best effort in fsPathExists, and finds a file past the first page", async () => {
    stats("file");
    await expect(fsPathExists("10.0.0.5", "/data/x")).resolves.toBe(true);
    expect(call).not.toHaveBeenCalled();
    stats({ error: "mgmt read timed out" });
    await expect(fsPathExists("10.0.0.5", "/data/x")).resolves.toBe(false);
  });
});

describe("smpManualInstall", () => {
  function fakeList(initial: string | null) {
    let content = initial;
    const writes: string[] = [];
    call.mockImplementation(async (cmd, args) => {
      if (cmd === "fs_read_preview") return { size: 0, base64: b64(content ?? "") };
      if (cmd === "fs_write_bytes_run") {
        content = atob((args as { bytesB64: string }).bytesB64);
        writes.push(content);
        return { ok: true };
      }
      throw new Error(`unexpected ${cmd}`);
    });
    return { writes, read: () => content };
  }

  it("creates the list when it is really not there", async () => {
    stats({ error: MISSING });
    const { writes } = fakeList(null);
    await expect(smpManualInstall("10.0.0.5", "/data/g/A")).resolves.toEqual({ added: true });
    expect(writes).toEqual(["/data/g/A\n"]);
  });

  it("appends to what is there, and leaves a path already listed alone", async () => {
    stats("file");
    const { writes } = fakeList("/data/g/A\n");
    await expect(smpManualInstall("10.0.0.5", "/data/g/B")).resolves.toEqual({ added: true });
    expect(writes).toEqual(["/data/g/A\n/data/g/B\n"]);
    await expect(smpManualInstall("10.0.0.5", "/data/g/B")).resolves.toEqual({ added: false });
    expect(writes).toHaveLength(1);
  });

  it("never rewrites the list when a read fails", async () => {
    stats({ error: "mgmt read timed out" });
    const { writes } = fakeList("/data/g/A\n");
    await expect(smpManualInstall("10.0.0.5", "/data/g/B")).rejects.toThrow("timed out");
    stats("file");
    call.mockRejectedValueOnce(new Error("fs_read: busy"));
    await expect(smpManualInstall("10.0.0.5", "/data/g/B")).rejects.toThrow("busy");
    expect(writes).toEqual([]);
  });

  it("keeps both of two hand-offs at once", async () => {
    stats("file");
    const { read } = fakeList("");
    await Promise.all([smpManualInstall("10.0.0.5", "/a"), smpManualInstall("10.0.0.5:9113", "/b")]);
    expect(read()).toBe("/a\n/b\n");
  });

  it("carries on after a failed hand-off, and reports a refused write", async () => {
    stats({ error: "busy" });
    fakeList("");
    await expect(smpManualInstall("10.0.0.5", "/a")).rejects.toThrow("busy");
    stats("file");
    const { writes } = fakeList("");
    await smpManualInstall("10.0.0.5", "/b");
    expect(writes).toEqual(["/b\n"]);
    call.mockImplementation(async (cmd) =>
      cmd === "fs_read_preview" ? { size: 0, base64: "" } : { ok: false, err: "fs_write_denied" },
    );
    await expect(smpManualInstall("10.0.0.5", "/c")).rejects.toThrow("fs_write_denied");
  });
});

describe("probeDestination", () => {
  it("reports a file past any page of its folder, from a stat", async () => {
    stats("file");
    await expect(probeDestination("10.0.0.5", "/data/a.pkg", false)).resolves.toEqual({
      exists: true,
      entryCount: 1,
      totalBytes: 7,
    });
    expect(call).not.toHaveBeenCalled();
  });

  it("says not there only when it is not there, and rejects when it could not look", async () => {
    stats({ error: MISSING });
    await expect(probeDestination("10.0.0.5", "/data/a.pkg", false)).resolves.toMatchObject({ exists: false });
    stats({ error: "mgmt read timed out" });
    await expect(probeDestination("10.0.0.5", "/data/a.pkg", false)).rejects.toThrow("timed out");

    call.mockRejectedValueOnce(new Error("fs_list_dir_opendir_errno_2"));
    await expect(probeDestination("10.0.0.5", "/data/g", true)).resolves.toMatchObject({ exists: false });
    call.mockRejectedValueOnce(new Error("engine unreachable"));
    await expect(probeDestination("10.0.0.5", "/data/g", true)).rejects.toThrow("unreachable");
  });

  it("marks a folder with more than one page", async () => {
    call.mockResolvedValueOnce({ entries: [{ name: "a", size: 2 }], truncated: true });
    await expect(probeDestination("10.0.0.5", "/data/g", true)).resolves.toEqual({
      exists: true,
      entryCount: 1,
      totalBytes: 2,
      more: true,
    });
  });
});
