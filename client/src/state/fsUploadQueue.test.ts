import { beforeEach, expect, it, vi } from "vitest";
import { useFsUploadStore, type FsUploadDeps, type FsUploadRequest } from "./fsUpload";
import { drainFsUploadQueue, enqueueFsUpload, fsUploadQueueFor, fsUploadReservations, pauseFsUploadQueue, removeFsUploadQueued, resumeFsUploadQueue, useFsUploadQueue } from "./fsUploadQueue";

const host = "192.0.2.22";
const request = (name: string, destDir = "/data/a"): FsUploadRequest => ({
  host, addr: host, destDir, srcPaths: [], steps: [{ kind: "one", src: `/pc/${name}`, name }],
});
function engine() {
  let release!: () => void;
  const gate = new Promise<void>((r) => { release = r; });
  const startFile = vi.fn<FsUploadDeps["startFile"]>(async () => "job");
  const deps: FsUploadDeps = {
    pathKind: async () => "file", startFile, startDir: startFile,
    jobStatus: async () => { await gate; return { status: "done" }; },
    jobCancel: async () => {}, sleep: async () => {}, listDir: async () => [],
  };
  return { deps, startFile, release };
}
beforeEach(() => {
  useFsUploadQueue.setState({ byHost: {} });
  useFsUploadStore.setState({ byHost: {} });
});

it("serializes picks and keeps each destination and queued name reservation", async () => {
  const e = engine();
  enqueueFsUpload(request("one"), e.deps, []);
  const second = enqueueFsUpload(request("two", "/data/b"), e.deps, []);
  await vi.waitFor(() => expect(e.startFile).toHaveBeenCalledTimes(1));
  expect(fsUploadReservations(host, "/data/b").map((x) => x.name)).toEqual(["two"]);
  expect(fsUploadQueueFor(host).items[1].id).toBe(second);
  e.release();
  await vi.waitFor(() => expect(fsUploadQueueFor(host).items).toHaveLength(0));
  expect(e.startFile.mock.calls.map((c) => c[1])).toEqual(["/data/a/one", "/data/b/two"]);
});

it("allows removing a waiting pick without touching the active pick", async () => {
  const e = engine();
  const first = enqueueFsUpload(request("one"), e.deps, []);
  const second = enqueueFsUpload(request("two"), e.deps, []);
  removeFsUploadQueued(host, first);
  removeFsUploadQueued(host, second);
  expect(fsUploadQueueFor(host).items.map((i) => i.id)).toEqual([first]);
  e.release();
  await vi.waitFor(() => expect(fsUploadQueueFor(host).items).toHaveLength(0));
});

it("pauses before overwriting a destination created while a pick waited", async () => {
  const e = engine();
  pauseFsUploadQueue(host);
  enqueueFsUpload(request("new.bin"), e.deps, []);
  e.deps.listDir = async () => [{ name: "new.bin", kind: "file", size: 8 }];
  resumeFsUploadQueue(host, e.deps);
  await vi.waitFor(() => expect(fsUploadQueueFor(host).error).toContain("Destination changed"));
  expect(e.startFile).not.toHaveBeenCalled();
  expect(fsUploadQueueFor(host).items).toHaveLength(1);
  expect(fsUploadQueueFor(host).paused).toBe(true);
});

it("does not start a paused queue until explicitly resumed", async () => {
  const e = engine();
  pauseFsUploadQueue(host);
  enqueueFsUpload(request("one"), e.deps, []);
  await drainFsUploadQueue(host, e.deps);
  expect(e.startFile).not.toHaveBeenCalled();
  e.release();
  resumeFsUploadQueue(host, e.deps);
  await vi.waitFor(() => expect(fsUploadQueueFor(host).items).toHaveLength(0));
});
