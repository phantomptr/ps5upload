import { describe, expect, it, vi } from "vitest";
import { ApiError } from "../../api";
import type { Volume } from "../../home/Home";
import {
  covers,
  createSearchFlow,
  driveRoots,
  namePattern,
  POLL_MS,
  toResults,
  toStatus,
  type IndexStatus,
  type Results,
  type SearchApi,
  type SearchState,
} from "./search";

const vol = (path: string, mount_from = "/dev/x"): Volume => ({ path, mount_from, fs_type: "exfatfs", total_bytes: 1, free_bytes: 1, writable: true });

describe("search helpers", () => {
  it("a word matches anywhere in the name; a pattern is used as typed", () => {
    expect(namePattern(" eboot ")).toBe("*eboot*");
    expect(namePattern("*.pkg")).toBe("*.pkg");
    expect(namePattern("CUSA0784?")).toBe("CUSA0784?");
    expect(namePattern("**a***b?**")).toBe("*a*b?*"); // runs of stars are one star
    expect(namePattern("")).toBeNull();
    expect(namePattern("   ")).toBeNull();
  });

  it("refuses what the helper's quote-to-quote scan would misread, and long patterns", () => {
    expect(namePattern('a"b')).toBeNull();
    expect(namePattern("a\\b")).toBeNull();
    expect(namePattern("a\tb")).toBeNull();
    expect(namePattern("x".repeat(253))).toBe(`*${"x".repeat(253)}*`); // 255 bytes: the helper's limit
    expect(namePattern("x".repeat(254))).toBeNull(); // 256 bytes
    expect(namePattern("é".repeat(127))).toBeNull(); // 254 UTF-8 bytes + 2
  });

  it("all drives = the drive list's drives (internal, extended, USB), never the system volumes", () => {
    const v = [vol("/data", "/dev/ssd0.user"), vol("/system"), vol("/mnt/ext0", "/dev/nvme0"), vol("/mnt/usb0", "/dev/da0s1"), vol("/mnt/ps5upload/img")];
    expect(driveRoots(v)).toEqual(["/data", "/mnt/ext0", "/mnt/usb0"]);
  });

  it("an index covers a scope inside one of its roots", () => {
    expect(covers(["/data", "/mnt/usb0"], ["/data/games"])).toBe(true);
    expect(covers(["/data", "/mnt/usb0"], ["/data", "/mnt/usb0"])).toBe(true);
    expect(covers(["/data/games"], ["/data"])).toBe(false);
    expect(covers(["/data"], ["/datax"])).toBe(false);
    expect(covers(null, ["/data"])).toBe(false);
    expect(covers(["/data"], [])).toBe(false);
  });

  it("status and results replies are checked", () => {
    expect(toStatus({ phase: "ready", files: 12, truncated: true, cancelled: false, roots: ["/data", 3] })).toEqual({
      phase: "ready",
      files: 12,
      truncated: true,
      cancelled: false,
      roots: ["/data"],
    });
    expect(toStatus({ phase: "done" })).toBeNull();
    expect(toStatus(null)).toBeNull();
    expect(toResults({ results: [{ path: "/data/a/b.pkg", size: 5 }, { path: "rel" }, { size: 1 }, null], truncated: true })).toEqual({
      hits: [{ path: "/data/a/b.pkg", name: "b.pkg", folder: "/data/a", size: 5 }],
      truncated: true,
    });
    expect(toResults({})).toEqual({ hits: [], truncated: false });
  });
});

/* ---- the flow, over a fake helper and a manual clock ---- */

function fakeHelper(start: Partial<IndexStatus> = {}) {
  let status: IndexStatus = { phase: "idle", files: 0, truncated: false, cancelled: false, roots: null, ...start };
  const results: Results = { hits: [{ path: "/data/g/eboot.bin", name: "eboot.bin", folder: "/data/g", size: 9 }], truncated: false };
  const api = {
    status: vi.fn(async () => status),
    start: vi.fn(async (roots: string[]) => {
      status = { phase: "building", files: 0, truncated: false, cancelled: false, roots };
    }),
    cancel: vi.fn(async () => {
      status = { ...status, phase: "ready", cancelled: true };
    }),
    search: vi.fn(async () => results),
  } satisfies SearchApi;
  return {
    api,
    set(s: Partial<IndexStatus>) {
      status = { ...status, ...s };
    },
  };
}

function flowOver(api: SearchApi) {
  const timers: (() => void)[] = [];
  const states: SearchState[] = [];
  const flow = createSearchFlow(
    {
      api,
      setTimer: (fn, ms) => {
        expect(ms).toBe(POLL_MS);
        timers.push(fn);
        return timers.length;
      },
      clearTimer: () => undefined,
    },
    (s) => states.push(s),
  );
  const tick = async () => {
    const fn = timers.shift();
    if (fn) fn();
    await new Promise((r) => setTimeout(r, 0));
  };
  return { flow, timers, states, tick };
}

describe("search flow", () => {
  it("a search sent before the first status read answered waits for the build it started", async () => {
    const h = fakeHelper();
    // the dialog's first read is slow: it answers "idle" only after the build has started
    let answerFirst: (s: IndexStatus) => void = () => undefined;
    h.api.status.mockImplementationOnce(() => new Promise<IndexStatus>((res) => (answerFirst = res)));
    const { flow, tick } = flowOver(h.api);
    const opened = flow.open();
    await flow.submit("eboot", "all", ["/data"], null);
    expect(h.api.start).toHaveBeenCalledWith(["/data"]);
    answerFirst({ phase: "idle", files: 0, truncated: false, cancelled: false, roots: null });
    await opened;
    // the old answer is dropped: no search on the index not built yet, and it is still pending
    expect(h.api.search).not.toHaveBeenCalled();
    expect(flow.state().index?.phase).toBe("building");
    expect(flow.state().pending).toBe("*eboot*");
    h.set({ phase: "ready", files: 9000 });
    await tick();
    expect(h.api.search).toHaveBeenCalledWith("*eboot*", null);
    expect(flow.state().done?.results.hits).toHaveLength(1);
  });

  it("first use builds the index for the scope (progress), then searches", async () => {
    const h = fakeHelper();
    const { flow, timers, tick } = flowOver(h.api);
    await flow.open();
    expect(flow.state().index?.phase).toBe("idle");
    await flow.submit("eboot", "all", ["/data", "/mnt/usb0"], null);
    expect(h.api.start).toHaveBeenCalledWith(["/data", "/mnt/usb0"]);
    expect(flow.state().index?.phase).toBe("building");
    expect(flow.state().pending).toBe("*eboot*");
    expect(h.api.search).not.toHaveBeenCalled();
    h.set({ files: 5000 });
    await tick();
    expect(flow.state().index?.files).toBe(5000);
    h.set({ phase: "ready", files: 9000 });
    await tick();
    expect(h.api.search).toHaveBeenCalledWith("*eboot*", null);
    expect(flow.state().done?.results.hits[0].name).toBe("eboot.bin");
    expect(flow.state().pending).toBeNull();
    expect(timers).toHaveLength(0); // no polling once ready
  });

  it("an index that covers the scope is searched at once (a folder scope filters by `under`)", async () => {
    const h = fakeHelper({ phase: "ready", files: 10, roots: ["/data", "/mnt/usb0"] });
    const { flow } = flowOver(h.api);
    await flow.open();
    await flow.submit("*.pkg", "folder", ["/data/games"], "/data/games");
    expect(h.api.start).not.toHaveBeenCalled();
    expect(h.api.search).toHaveBeenCalledWith("*.pkg", "/data/games");
    expect(flow.state().done?.scope).toBe("folder");
  });

  it("an index that does not cover the scope (or another client's) is rebuilt for it", async () => {
    const h = fakeHelper({ phase: "ready", files: 10, roots: ["/data/games"] });
    const { flow } = flowOver(h.api);
    await flow.open();
    await flow.submit("a", "all", ["/data"], null);
    expect(h.api.start).toHaveBeenCalledWith(["/data"]);
  });

  it("a search while it builds waits for the build; another client's build gets one build of ours", async () => {
    const h = fakeHelper({ phase: "building", roots: null });
    const { flow, tick } = flowOver(h.api);
    await flow.open();
    await flow.submit("x", "all", ["/data"], null);
    expect(h.api.start).not.toHaveBeenCalled();
    h.set({ phase: "ready", roots: null });
    await tick(); // not ours: build once for the scope
    expect(h.api.start).toHaveBeenCalledTimes(1);
    h.set({ phase: "ready" });
    await tick();
    expect(h.api.search).toHaveBeenCalledTimes(1);
    expect(h.api.start).toHaveBeenCalledTimes(1);
  });

  it("Cancel stops the build: the partial index stays, nothing is searched", async () => {
    const h = fakeHelper();
    const { flow, tick } = flowOver(h.api);
    await flow.open();
    await flow.submit("x", "all", ["/data"], null);
    await flow.cancel();
    expect(h.api.cancel).toHaveBeenCalled();
    expect(flow.state().stopping).toBe(true);
    expect(flow.state().pending).toBeNull();
    await tick();
    expect(flow.state().index).toMatchObject({ phase: "ready", cancelled: true });
    expect(flow.state().stopping).toBe(false);
    expect(h.api.search).not.toHaveBeenCalled();
    // the partial index covers the scope: the next search uses it
    await flow.submit("x", "all", ["/data"], null);
    expect(h.api.search).toHaveBeenCalledTimes(1);
    expect(h.api.start).toHaveBeenCalledTimes(1);
  });

  it("another build already running (409 busy) is waited on instead", async () => {
    const h = fakeHelper();
    h.api.start.mockImplementationOnce(async () => {
      h.set({ phase: "building", roots: ["/data"] });
      throw new ApiError(409, "busy");
    });
    const { flow, tick } = flowOver(h.api);
    await flow.open();
    await flow.submit("x", "all", ["/data"], null);
    expect(flow.state().error).toBeNull();
    h.set({ phase: "ready" });
    await tick();
    expect(h.api.search).toHaveBeenCalled();
  });

  it("an older helper (no index routes) says so; nothing else is tried", async () => {
    const h = fakeHelper();
    h.api.status.mockRejectedValue(new ApiError(404, "not_found"));
    const { flow } = flowOver(h.api);
    await flow.open();
    expect(flow.state().unsupported).toBe(true);
    await flow.submit("x", "all", ["/data"], null);
    expect(h.api.start).not.toHaveBeenCalled();
  });

  it("a failed start shows its error and forgets the pending search", async () => {
    const h = fakeHelper();
    h.api.start.mockRejectedValueOnce(new ApiError(403, "path_not_allowed"));
    const { flow } = flowOver(h.api);
    await flow.open();
    await flow.submit("x", "folder", ["/data/ps5upload"], "/data/ps5upload");
    expect(flow.state().error).toBeInstanceOf(ApiError);
    expect(flow.state().pending).toBeNull();
  });

  it("closing stops the polling (the build itself goes on in the helper)", async () => {
    const h = fakeHelper();
    const { flow, timers, states } = flowOver(h.api);
    await flow.open();
    await flow.submit("x", "all", ["/data"], null);
    expect(timers).toHaveLength(1);
    flow.close();
    const seen = states.length;
    timers.shift()?.();
    await new Promise((r) => setTimeout(r, 0));
    expect(states.length).toBe(seen);
    expect(h.api.cancel).not.toHaveBeenCalled();
  });

  it("an invalid pattern is not sent", async () => {
    const h = fakeHelper({ phase: "ready", roots: ["/data"] });
    const { flow } = flowOver(h.api);
    await flow.open();
    await flow.submit('say "hi"', "all", ["/data"], null);
    expect(h.api.search).not.toHaveBeenCalled();
    expect(h.api.start).not.toHaveBeenCalled();
  });
});
