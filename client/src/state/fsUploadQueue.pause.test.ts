import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { fsUploadForHost, runFsUpload, useFsUploadStore, type FsUploadDeps, type FsUploadRequest } from "./fsUpload";
import {
  enqueueFsUpload,
  fsUploadQueueFor,
  pauseFsUpload,
  resumeFsUploadItem,
  sendFsUploadNow,
  useFsUploadQueue,
} from "./fsUploadQueue";

// Pause / Resume / Send now on the Files screen's upload line. Each job is named after its
// file and runs until the test lets it finish; a cancelled job reads "failed", the way the
// engine ends it.
// A console per test: a copy a test leaves running keeps its loop, which must not see the next
// test's picks.
let hostN = 30;
let host = "";
const request = (name: string): FsUploadRequest => ({
  host, addr: host, destDir: "/data", srcPaths: [], steps: [{ kind: "one", src: `/pc/${name}`, name }],
});
/** Every fake engine a test made: after the test each one fails its jobs, so no copy loop is
 *  left polling (and logging) once the test file ends. */
const engines: { stop: boolean }[] = [];
function engine() {
  const life = { stop: false };
  engines.push(life);
  const finish = new Set<string>();
  const failing = new Set<string>();
  const cancelled = new Set<string>();
  let n = 0;
  const startFile = vi.fn<FsUploadDeps["startFile"]>(async (src) => `${src.split("/").pop()}#${++n}`);
  const deps: FsUploadDeps = {
    pathKind: async () => "file", startFile, startDir: startFile,
    jobStatus: async (id) => {
      const name = id.split("#")[0];
      if (life.stop || cancelled.has(id)) return { status: "failed", error: "cancelled" };
      if (failing.has(name)) return { status: "failed", error: "disk full" };
      return { status: finish.has(name) ? "done" : "running", bytes_sent: 1, total_bytes: 10 };
    },
    jobCancel: async (id) => { cancelled.add(id); },
    sleep: () => new Promise((r) => setTimeout(r, 1)),
    listDir: async () => [],
  };
  /** Files whose upload was started, in order. */
  const started = () => startFile.mock.calls.map((c) => c[0].split("/").pop());
  return { deps, startFile, finish, failing, started };
}
const active = () => fsUploadForHost(useFsUploadStore.getState(), host).active?.name ?? null;
const queued = () => fsUploadQueueFor(host).items.map((i) => ({
  name: i.request.steps?.[0]?.kind === "one" ? i.request.steps[0].name : "",
  paused: i.paused ?? null,
}));

afterEach(async () => {
  for (const e of engines) e.stop = true;
  engines.length = 0;
  await vi.waitFor(() => expect(active()).toBeNull());
});

beforeEach(() => {
  host = `192.0.2.${++hostN}`;
  useFsUploadQueue.setState({ byHost: {} });
  useFsUploadStore.setState({ byHost: {} });
});

it("Pause keeps what is left as a paused pick and lets the next drop run", async () => {
  const e = engine();
  enqueueFsUpload(request("big.bin"), e.deps, []);
  await vi.waitFor(() => expect(active()).toBe("big.bin"));
  pauseFsUpload(host, e.deps);
  await vi.waitFor(() => expect(queued()).toEqual([{ name: "big.bin", paused: "user" }]));
  // No stopped card, and the line is not held.
  expect(fsUploadForHost(useFsUploadStore.getState(), host).stopped).toBeNull();
  expect(fsUploadQueueFor(host).paused).toBe(false);

  enqueueFsUpload(request("small.txt"), e.deps, []);
  await vi.waitFor(() => expect(active()).toBe("small.txt"));
  e.finish.add("small.txt");
  await vi.waitFor(() => expect(queued()).toEqual([{ name: "big.bin", paused: "user" }]));
  expect(active()).toBeNull();

  // Resume: the same steps again; the console keeps what landed.
  resumeFsUploadItem(host, fsUploadQueueFor(host).items[0].id, e.deps);
  await vi.waitFor(() => expect(active()).toBe("big.bin"));
  e.finish.add("big.bin");
  await vi.waitFor(() => expect(queued()).toEqual([]));
  expect(e.started()).toEqual(["big.bin", "small.txt", "big.bin"]);
  // A paused pick is never treated as a conflict with its own partial file.
  expect(fsUploadQueueFor(host).error).toBeNull();
});

it("a resumed pick whose own file is on the console is not a conflict", async () => {
  const e = engine();
  enqueueFsUpload(request("big.bin"), e.deps, []);
  await vi.waitFor(() => expect(active()).toBe("big.bin"));
  pauseFsUpload(host, e.deps);
  await vi.waitFor(() => expect(queued()).toHaveLength(1));
  e.deps.listDir = async () => [{ name: "big.bin", kind: "file", size: 5 }];
  resumeFsUploadItem(host, fsUploadQueueFor(host).items[0].id, e.deps);
  await vi.waitFor(() => expect(active()).toBe("big.bin"));
  expect(fsUploadQueueFor(host).error).toBeNull();
});

it("Send now pauses the running copy and resumes it by itself afterwards", async () => {
  const e = engine();
  enqueueFsUpload(request("big.bin"), e.deps, []);
  enqueueFsUpload(request("other.bin"), e.deps, []);
  const small = enqueueFsUpload(request("small.txt"), e.deps, []);
  await vi.waitFor(() => expect(active()).toBe("big.bin"));

  sendFsUploadNow(host, small, e.deps);
  await vi.waitFor(() => expect(active()).toBe("small.txt"));
  expect(queued()).toEqual([
    { name: "big.bin", paused: "sendNow" },
    { name: "other.bin", paused: null },
    { name: "small.txt", paused: null },
  ]);
  e.finish.add("small.txt");
  // big is first in line again, ahead of other.
  await vi.waitFor(() => expect(active()).toBe("big.bin"));
  expect(queued()[0]).toEqual({ name: "big.bin", paused: null });
  expect(e.started()).toEqual(["big.bin", "small.txt", "big.bin"]);
});

it("Send now resumes the paused copy even when the sent pick fails", async () => {
  const e = engine();
  enqueueFsUpload(request("big.bin"), e.deps, []);
  const small = enqueueFsUpload(request("small.txt"), e.deps, []);
  await vi.waitFor(() => expect(active()).toBe("big.bin"));
  e.failing.add("small.txt");
  sendFsUploadNow(host, small, e.deps);
  await vi.waitFor(() => expect(e.started()).toEqual(["big.bin", "small.txt", "big.bin"]));
  await vi.waitFor(() => expect(active()).toBe("big.bin"));
  // The failed pick waits, paused, with its error; the line is not held.
  const failed = fsUploadQueueFor(host).items.find((i) => i.error);
  expect(failed?.paused).toBe("user");
  expect(failed?.error).toContain("disk full");
  expect(fsUploadQueueFor(host).paused).toBe(false);
});

it("stacked Send nows run in click order; the paused copy resumes after the last", async () => {
  const e = engine();
  enqueueFsUpload(request("big.bin"), e.deps, []);
  const one = enqueueFsUpload(request("one.txt"), e.deps, []);
  const two = enqueueFsUpload(request("two.txt"), e.deps, []);
  await vi.waitFor(() => expect(active()).toBe("big.bin"));
  sendFsUploadNow(host, two, e.deps);
  await vi.waitFor(() => expect(active()).toBe("two.txt"));
  sendFsUploadNow(host, one, e.deps);
  await new Promise((r) => setTimeout(r, 20));
  // "two" was sent now itself: it keeps going.
  expect(active()).toBe("two.txt");
  e.finish.add("two.txt");
  await vi.waitFor(() => expect(active()).toBe("one.txt"));
  expect(queued()[0]).toEqual({ name: "big.bin", paused: "sendNow" });
  e.finish.add("one.txt");
  await vi.waitFor(() => expect(active()).toBe("big.bin"));
  expect(e.started()).toEqual(["big.bin", "two.txt", "one.txt", "big.bin"]);
});

it("a pick the person paused is not resumed by a Send now", async () => {
  const e = engine();
  enqueueFsUpload(request("big.bin"), e.deps, []);
  await vi.waitFor(() => expect(active()).toBe("big.bin"));
  pauseFsUpload(host, e.deps);
  await vi.waitFor(() => expect(queued()).toEqual([{ name: "big.bin", paused: "user" }]));
  enqueueFsUpload(request("mid.bin"), e.deps, []);
  await vi.waitFor(() => expect(active()).toBe("mid.bin"));
  const small = enqueueFsUpload(request("small.txt"), e.deps, []);
  sendFsUploadNow(host, small, e.deps);
  await vi.waitFor(() => expect(active()).toBe("small.txt"));
  e.finish.add("small.txt");
  // mid (paused by the Send now) carries on; big stays paused.
  await vi.waitFor(() => expect(active()).toBe("mid.bin"));
  e.finish.add("mid.bin");
  await vi.waitFor(() => expect(queued()).toEqual([{ name: "big.bin", paused: "user" }]));
  expect(active()).toBeNull();
});

it("paused picks survive a restart; one paused for a finished Send now waits in line again", async () => {
  const saved = new Map<string, string>([
    ["ps5upload.fsUploadQueue.v1", JSON.stringify({ [host]: {
      items: [
        { id: "big", request: request("big.bin"), allowedExisting: ["big.bin"], paused: "user" },
        { id: "mid", request: request("mid.bin"), allowedExisting: [], paused: "sendNow" },
      ], running: true, paused: false, error: null,
    } })],
  ]);
  vi.stubGlobal("window", {
    localStorage: { getItem: (k: string) => saved.get(k) ?? null, setItem: (k: string, v: string) => saved.set(k, v) },
  });
  try {
    vi.resetModules();
    const restored = await import("./fsUploadQueue");
    expect(restored.fsUploadQueueFor(host).items.map((i) => [i.id, i.paused ?? null])).toEqual([
      ["big", "user"],
      ["mid", null],
    ]);
  } finally { vi.unstubAllGlobals(); }
});

it("pausing a copy started outside the line leaves no pause behind for the next pick", async () => {
  const e = engine();
  // A Replace / Resume copy: not one of the line's picks.
  void runFsUpload(request("outside.bin"), e.deps);
  await vi.waitFor(() => expect(active()).toBe("outside.bin"));
  enqueueFsUpload(request("a.bin"), e.deps, []);
  enqueueFsUpload(request("b.bin"), e.deps, []);
  pauseFsUpload(host, e.deps);
  await vi.waitFor(() => expect(active()).toBe("a.bin"));
  expect(queued()[0]).toEqual({ name: "outside.bin", paused: "user" });

  // a.bin fails: that is a failure (the line holds), not the pause of outside.bin.
  e.failing.add("a.bin");
  await vi.waitFor(() => expect(fsUploadQueueFor(host).paused).toBe(true));
  await vi.waitFor(() => expect(active()).toBeNull());
  expect(fsUploadForHost(useFsUploadStore.getState(), host).stopped?.why).toBe("failed");
  expect(e.started()).toEqual(["outside.bin", "a.bin"]);
});

it("Send now still runs when the copy it paused finished just as the stop landed", async () => {
  const e = engine();
  // The stop reaches the engine after the job's last byte: the job ends done.
  e.deps.jobCancel = async (id) => { e.finish.add(id.split("#")[0]); };
  enqueueFsUpload(request("big.bin"), e.deps, []);
  const small = enqueueFsUpload(request("small.txt"), e.deps, []);
  await vi.waitFor(() => expect(active()).toBe("big.bin"));
  sendFsUploadNow(host, small, e.deps);
  await vi.waitFor(() => expect(active()).toBe("small.txt"));
  // The stop landed: big waits as a pick the Send now paused (its resume re-sends nothing the
  // console has), and the line is not held.
  expect(fsUploadQueueFor(host).paused).toBe(false);
  e.finish.add("small.txt");
  await vi.waitFor(() => expect(queued()).toEqual([]));
  expect(e.started()).toEqual(["big.bin", "small.txt", "big.bin"]);
});

