import { describe, expect, it, vi } from "vitest";

import { discardUploadLeftovers, leftoverDest, type LeftoverDeps } from "./uploadLeftovers";
import { removeUploadPart } from "./uploadPart";

vi.mock("../api/ps5", () => ({}));

type Row = Parameters<typeof leftoverDest>[0];
const ADDR = "192.168.1.10:9113";

const row = (over: Partial<Row> = {}): Row => ({
  id: "a",
  addr: ADDR,
  status: "failed",
  sourceKind: "pkg",
  sourcePath: "/src/g.pkg",
  resolvedDest: "/data/pkg/g.pkg",
  startedAt: 1,
  interrupted: false,
  uploadDone: false,
  ...over,
});

/** A console holding `folders` (path -> entries) and `files`. */
function consoleWith(folders: Record<string, string[]>, files: string[] = []) {
  const removed: string[] = [];
  const deps: LeftoverDeps = {
    listDir: vi.fn(async (_a: string, p: string) => {
      if (!(p in folders)) throw new Error("fs_list_dir_not_found");
      return folders[p];
    }),
    remove: vi.fn(async (_a: string, p: string) => {
      if (!(p in folders) && !files.includes(p)) throw new Error("fs_delete_not_found");
      removed.push(p);
      delete folders[p];
    }),
    jobRunning: vi.fn(async () => false),
    forgetResumeId: vi.fn(async () => {}),
    sleep: vi.fn(async () => {}),
  };
  return { deps, removed };
}

describe("leftoverDest", () => {
  it("names the destination of a row that ran and did not finish", () => {
    expect(leftoverDest(row())).toBe("/data/pkg/g.pkg");
    expect(leftoverDest(row({ status: "running" }))).toBe("/data/pkg/g.pkg");
    expect(leftoverDest(row({ status: "pending", interrupted: true, startedAt: null }))).toBe("/data/pkg/g.pkg");
  });
  it("leaves a row that never ran, finished, or does not upload", () => {
    expect(leftoverDest(row({ status: "pending", startedAt: null }))).toBeNull();
    expect(leftoverDest(row({ status: "done" }))).toBeNull();
    expect(leftoverDest(row({ uploadDone: true }))).toBeNull();
    expect(leftoverDest(row({ sourceKind: "install" }))).toBeNull();
    expect(leftoverDest(row({ sourceKind: "ps5" }))).toBeNull();
  });
});

describe("removeUploadPart", () => {
  it("deletes a single-file part and touches nothing else", async () => {
    const { deps, removed } = consoleWith({ "/data/g": ["save"] }, ["/data/g.ava-part"]);
    await removeUploadPart(ADDR, "/data/g", deps);
    expect(removed).toEqual(["/data/g.ava-part"]);
  });
  it("deletes a staging tree and the empty folder holding its place", async () => {
    const { deps, removed } = consoleWith({ "/data/G.ava-part": ["eboot.bin"], "/data/G": [] });
    await removeUploadPart(ADDR, "/data/G", deps);
    expect(removed).toEqual(["/data/G.ava-part", "/data/G"]);
  });
  it("never deletes a destination that holds anything", async () => {
    const { deps, removed } = consoleWith({ "/data/G.ava-part": ["x"], "/data/G": ["mine.txt"] });
    await removeUploadPart(ADDR, "/data/G", deps);
    expect(removed).toEqual(["/data/G.ava-part"]);
  });
  it("leaves an empty folder alone when no staging tree proves it is the job's", async () => {
    const { deps, removed } = consoleWith({ "/data/G": [] });
    await expect(removeUploadPart(ADDR, "/data/G", deps)).rejects.toThrow();
    expect(removed).toEqual([]);
  });
});

describe("discardUploadLeftovers", () => {
  it("deletes an idle row's part at once and forgets its resume id", async () => {
    const { deps, removed } = consoleWith({}, ["/data/pkg/g.pkg.ava-part"]);
    await discardUploadLeftovers(row(), () => [], deps);
    expect(removed).toEqual(["/data/pkg/g.pkg.ava-part"]);
    expect(deps.forgetResumeId).toHaveBeenCalledWith("192.168.1.10", "/src/g.pkg", "/data/pkg/g.pkg");
    expect(deps.sleep).not.toHaveBeenCalled();
  });
  it("waits for a stopped job to end before deleting, then sweeps once more", async () => {
    const { deps, removed } = consoleWith({}, ["/data/pkg/g.pkg.ava-part"]);
    const order: string[] = [];
    vi.mocked(deps.jobRunning)
      .mockImplementationOnce(async () => (order.push("running"), true))
      .mockImplementationOnce(async () => (order.push("ended"), false));
    vi.mocked(deps.remove).mockImplementation(async (_a, p) => {
      order.push("remove");
      removed.push(p);
    });
    await discardUploadLeftovers(row({ status: "running", jobId: "j" }), () => [], deps);
    expect(order).toEqual(["running", "ended", "remove", "remove"]);
  });
  it("deletes nothing while the stopped job will not end", async () => {
    const { deps, removed } = consoleWith({}, ["/data/pkg/g.pkg.ava-part"]);
    vi.mocked(deps.jobRunning).mockResolvedValue(true);
    await discardUploadLeftovers(row({ status: "running", jobId: "j" }), () => [], deps);
    expect(removed).toEqual([]);
  });
  it("leaves the part when another row counts on the same destination", async () => {
    const { deps, removed } = consoleWith({}, ["/data/G.ava-part"]);
    const it0 = row({ sourceKind: "archive", resolvedDest: "/data/G" });
    await discardUploadLeftovers(it0, () => [row({ id: "b", status: "pending", resolvedDest: "/data/G" })], deps);
    expect(removed).toEqual([]);
    expect(deps.forgetResumeId).not.toHaveBeenCalled();
  });
  it("does not delete a re-add's part on the late sweep", async () => {
    const { deps, removed } = consoleWith({}, ["/data/pkg/g.pkg.ava-part"]);
    let readd = false;
    vi.mocked(deps.sleep).mockImplementation(async (ms) => {
      if (ms > 5_000) readd = true;
    });
    await discardUploadLeftovers(
      row({ status: "running" }),
      () => (readd ? [row({ id: "b", status: "running" })] : []),
      deps,
    );
    expect(removed).toEqual(["/data/pkg/g.pkg.ava-part"]);
  });
  it("swallows a console that is away", async () => {
    const { deps } = consoleWith({});
    vi.mocked(deps.remove).mockRejectedValue(new Error("connect refused"));
    await expect(discardUploadLeftovers(row(), () => [], deps)).resolves.toBeUndefined();
  });
});
