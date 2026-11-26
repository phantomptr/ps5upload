import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Volume } from "../api/ps5";

const fetchVolumes = vi.fn<(addr: string) => Promise<Volume[]>>();
vi.mock("../api/ps5", () => ({ fetchVolumes: (addr: string) => fetchVolumes(addr) }));

import { useTaskStore } from "./tasks";
import {
  refreshVolumes,
  refreshVolumesAfterChange,
  resetVolumesForTests,
  useVolumesStore,
  volumesForHost,
  VOLUMES_SETTLE_MS,
  watchConsoleVolumes,
} from "./volumes";

const GIB = 1024 ** 3;
const data = (freeGib: number): Volume => ({
  path: "/data",
  fs_type: "ufs",
  total_bytes: 800 * GIB,
  free_bytes: freeGib * GIB,
  writable: true,
});

const HOST = "192.168.1.50";
/** What a screen showing this console's drives would show as free on /data, in GiB. */
const shown = (host: string = HOST): number | null => {
  const v = volumesForHost(useVolumesStore.getState(), host).volumes;
  return v ? v[0].free_bytes / GIB : null;
};
/** Let the reads that were started settle. */
const settle = () => vi.advanceTimersByTimeAsync(0);

beforeEach(() => {
  fetchVolumes.mockReset();
  resetVolumesForTests();
  useTaskStore.setState({ tasks: [] });
});
afterEach(() => {
  vi.useRealTimers();
});

describe("the shared reading of a console's drives (#944)", () => {
  it("two screens read one slot: one read serves both, and a refresh from either updates both", async () => {
    fetchVolumes.mockResolvedValueOnce([data(98)]);
    // Files, and Volumes with the address in another shape: the same console.
    const leaveFiles = watchConsoleVolumes(HOST);
    const leaveVolumes = watchConsoleVolumes(`${HOST}:9113`);
    await refreshVolumes(HOST);
    // A second caller joins the read in flight.
    expect(fetchVolumes).toHaveBeenCalledTimes(1);
    expect(shown(HOST)).toBe(98);
    expect(shown(`${HOST}:9113`)).toBe(98);

    fetchVolumes.mockResolvedValueOnce([data(163)]);
    await refreshVolumes(`${HOST}:9113`);
    expect(shown(HOST)).toBe(163);
    leaveFiles();
    leaveVolumes();
  });

  it("keeps each console's drives apart", async () => {
    fetchVolumes.mockImplementation(async (addr) => [data(addr.startsWith("10.") ? 5 : 98)]);
    await refreshVolumes(HOST);
    await refreshVolumes("10.0.0.9");
    expect(shown(HOST)).toBe(98);
    expect(shown("10.0.0.9")).toBe(5);
  });

  it("a recent reading is kept only when the caller says how recent is enough", async () => {
    fetchVolumes.mockResolvedValue([data(98)]);
    await refreshVolumes(HOST);
    await refreshVolumes(HOST, { maxAgeMs: 30_000 });
    expect(fetchVolumes).toHaveBeenCalledTimes(1);
    // Refresh has no such allowance.
    await refreshVolumes(HOST);
    expect(fetchVolumes).toHaveBeenCalledTimes(2);
    // An old reading is read again.
    useVolumesStore.getState().patch(HOST, { loadedAtMs: Date.now() - 31_000 });
    await refreshVolumes(HOST, { maxAgeMs: 30_000 });
    expect(fetchVolumes).toHaveBeenCalledTimes(3);
  });

  it("reads once more after a change that a read in flight may have missed", async () => {
    let release: (v: Volume[]) => void = () => {};
    fetchVolumes.mockImplementationOnce(() => new Promise((r) => (release = r)));
    const first = refreshVolumes(HOST);
    // The delete finished while that read was on its way.
    fetchVolumes.mockResolvedValueOnce([data(163)]);
    const after = refreshVolumesAfterChange(HOST);
    release([data(98)]);
    await first;
    expect((await after)?.[0].free_bytes).toBe(163 * GIB);
    expect(fetchVolumes).toHaveBeenCalledTimes(2);
    expect(shown()).toBe(163);
  });

  it("a read that fails keeps the last list and says so", async () => {
    fetchVolumes.mockResolvedValueOnce([data(98)]);
    await refreshVolumes(HOST);
    fetchVolumes.mockRejectedValueOnce(new Error("fs_list_volumes_getmntinfo_failed"));
    expect(await refreshVolumes(HOST)).toBeNull();
    const slot = volumesForHost(useVolumesStore.getState(), HOST);
    expect(slot.error).toBe("fs_list_volumes_getmntinfo_failed");
    expect(slot.volumes?.[0].free_bytes).toBe(98 * GIB);
    expect(slot.loading).toBe(false);
    // The next good read clears it.
    fetchVolumes.mockResolvedValueOnce([data(97)]);
    await refreshVolumes(HOST);
    expect(volumesForHost(useVolumesStore.getState(), HOST).error).toBeNull();
    expect(shown()).toBe(97);
  });

  it("a reading that changed nothing keeps the same list, so nothing that follows it runs again", async () => {
    fetchVolumes.mockImplementation(async () => [data(98)]);
    await refreshVolumes(HOST);
    const first = volumesForHost(useVolumesStore.getState(), HOST);
    await refreshVolumes(HOST);
    const second = volumesForHost(useVolumesStore.getState(), HOST);
    expect(second.volumes).toBe(first.volumes);
    // It is still a newer reading.
    expect(second.loadedAtMs).not.toBeNull();
    fetchVolumes.mockImplementation(async () => [data(97)]);
    await refreshVolumes(HOST);
    expect(volumesForHost(useVolumesStore.getState(), HOST).volumes).not.toBe(first.volumes);
    expect(shown()).toBe(97);
  });

  it("reads nothing without a console", async () => {
    watchConsoleVolumes("")();
    watchConsoleVolumes(null)();
    expect(await refreshVolumes("  ")).toBeNull();
    expect(await refreshVolumesAfterChange(undefined)).toBeNull();
    expect(fetchVolumes).not.toHaveBeenCalled();
  });
});

describe("drives are read again when a job on the console ends (#944)", () => {
  const start = (kind: "upload-file" | "fs-delete" | "icon-fetch", consoleId = HOST) =>
    useTaskStore.getState().registerTask({ kind, origin: "test", label: "job", consoleId, status: "running" });

  it("an upload that finishes updates the free space on the screen showing it", async () => {
    vi.useFakeTimers();
    fetchVolumes.mockResolvedValueOnce([data(163)]);
    const leave = watchConsoleVolumes(HOST);
    await settle();
    expect(shown()).toBe(163);

    const id = start("upload-file");
    // Progress is not an end: nothing is read while it runs.
    useTaskStore.getState().updateTask(id, { progress: { current: 1, total: 2, unit: "bytes" } });
    await vi.advanceTimersByTimeAsync(VOLUMES_SETTLE_MS * 2);
    expect(fetchVolumes).toHaveBeenCalledTimes(1);

    fetchVolumes.mockResolvedValueOnce([data(120)]);
    useTaskStore.getState().finishTask(id, "done");
    await vi.advanceTimersByTimeAsync(VOLUMES_SETTLE_MS + 10);
    expect(fetchVolumes).toHaveBeenCalledTimes(2);
    expect(shown()).toBe(120);
    leave();
  });

  it("many jobs ending together cost one read", async () => {
    vi.useFakeTimers();
    fetchVolumes.mockResolvedValue([data(163)]);
    const leave = watchConsoleVolumes(HOST);
    await settle();
    const ids = [start("fs-delete"), start("fs-delete"), start("upload-file")];
    for (const id of ids) useTaskStore.getState().finishTask(id, "done");
    await vi.advanceTimersByTimeAsync(VOLUMES_SETTLE_MS + 10);
    expect(fetchVolumes).toHaveBeenCalledTimes(2);
    leave();
  });

  it("a failed or stopped job counts too: it may have written before it ended", async () => {
    vi.useFakeTimers();
    fetchVolumes.mockResolvedValue([data(163)]);
    const leave = watchConsoleVolumes(HOST);
    await settle();
    useTaskStore.getState().finishTask(start("upload-file"), "failed");
    await vi.advanceTimersByTimeAsync(VOLUMES_SETTLE_MS + 10);
    expect(fetchVolumes).toHaveBeenCalledTimes(2);
    useTaskStore.getState().finishTask(start("fs-delete"), "cancelled");
    await vi.advanceTimersByTimeAsync(VOLUMES_SETTLE_MS + 10);
    expect(fetchVolumes).toHaveBeenCalledTimes(3);
    leave();
  });

  it("leaves alone a job on another console, and one that writes nothing", async () => {
    vi.useFakeTimers();
    fetchVolumes.mockResolvedValue([data(163)]);
    const leave = watchConsoleVolumes(HOST);
    await settle();
    useTaskStore.getState().finishTask(start("upload-file", "10.0.0.9"), "done");
    useTaskStore.getState().finishTask(start("icon-fetch"), "done");
    await vi.advanceTimersByTimeAsync(VOLUMES_SETTLE_MS * 3);
    expect(fetchVolumes).toHaveBeenCalledTimes(1);
    leave();
  });

  it("reads nothing for a console no screen is showing", async () => {
    vi.useFakeTimers();
    fetchVolumes.mockResolvedValue([data(163)]);
    const leave = watchConsoleVolumes(HOST);
    await settle();
    // The screen went away, or was hidden (a hidden screen's effects are torn down).
    leave();
    leave();
    useTaskStore.getState().finishTask(start("upload-file"), "done");
    await vi.advanceTimersByTimeAsync(VOLUMES_SETTLE_MS * 3);
    expect(fetchVolumes).toHaveBeenCalledTimes(1);
  });

  it("a job that ends just before the screen is left still does not read for nobody", async () => {
    vi.useFakeTimers();
    fetchVolumes.mockResolvedValue([data(163)]);
    const leave = watchConsoleVolumes(HOST);
    await settle();
    useTaskStore.getState().finishTask(start("upload-file"), "done");
    leave();
    await vi.advanceTimersByTimeAsync(VOLUMES_SETTLE_MS * 3);
    expect(fetchVolumes).toHaveBeenCalledTimes(1);
  });

  it("coming back to a screen reads the drives again", async () => {
    vi.useFakeTimers();
    fetchVolumes.mockResolvedValueOnce([data(98)]);
    watchConsoleVolumes(HOST)();
    await settle();
    expect(shown()).toBe(98);
    // Shown again after being hidden: the effect runs again.
    fetchVolumes.mockResolvedValueOnce([data(163)]);
    const leave = watchConsoleVolumes(HOST);
    await settle();
    expect(shown()).toBe(163);
    leave();
  });
});
