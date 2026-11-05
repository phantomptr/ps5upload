import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api", async (importOriginal) => {
  const real = await importOriginal<typeof import("../api")>();
  return { ...real, call: vi.fn() };
});

import { ApiError, call } from "../api";
import { t } from "../strings";
import type { Conflict, Decision } from "./paste";
import { ReplaceError } from "./replace";
import {
  isSmbPath,
  jobOfError,
  shareAddress,
  smbClean,
  smbErrorText,
  smbParts,
  smbSegments,
  smbUp,
  splitShare,
  SmbJobError,
  transferItems,
  watchSmbJob,
  type SmbJob,
  type SmbShare,
} from "./smb";

const mockCall = vi.mocked(call);
const ID = "0a1b2c3d";
const CJK = "中文 文件夹";

const share = (over: Partial<SmbShare> = {}): SmbShare => ({
  id: ID,
  name: "NAS",
  host: "192.168.1.10",
  port: 445,
  share: "Media",
  user: "",
  domain: "",
  start_path: "",
  has_password: false,
  path: `smb://${ID}/`,
  ...over,
});

const job = (over: Partial<SmbJob> = {}): SmbJob => ({
  job: "j1",
  kind: "download",
  state: "running",
  bytes_done: 0,
  bytes_total: 100,
  files_done: 0,
  files_total: 1,
  scanning: false,
  current: "",
  error: null,
  failed: "",
  reconnects: 0,
  ...over,
});

beforeEach(() => {
  mockCall.mockReset();
});

describe("share paths", () => {
  it("parses, cleans and climbs smb://<id>/<path>", () => {
    expect(isSmbPath(`smb://${ID}/x`)).toBe(true);
    expect(isSmbPath("/data/x")).toBe(false);
    expect(isSmbPath(null)).toBe(false);
    expect(smbParts(`smb://${ID}/a/${CJK}/`)).toEqual({ id: ID, rel: `a/${CJK}` });
    expect(smbParts(`smb://${ID}`)).toEqual({ id: ID, rel: "" });
    expect(smbParts("smb://NOTANID/x")).toBeNull();
    expect(smbParts("/data")).toBeNull();
    expect(smbClean(`smb://${ID}/`)).toBe(`smb://${ID}`);
    expect(smbClean(`smb://${ID}/a/b/`)).toBe(`smb://${ID}/a/b`);
    expect(smbUp(`smb://${ID}/a/b`)).toBe(`smb://${ID}/a`);
    expect(smbUp(`smb://${ID}/a`)).toBe(`smb://${ID}`);
    expect(smbUp(`smb://${ID}`)).toBeNull();
    expect(smbUp(`smb://${ID}/`)).toBeNull();
  });

  it("names the share's top in the path bar and shows non-ASCII folders as they are", () => {
    expect(smbSegments(`smb://${ID}/游戏/${CJK}`, [share({ name: "我的 NAS" })])).toEqual([
      { label: "我的 NAS", path: `smb://${ID}` },
      { label: "游戏", path: `smb://${ID}/游戏` },
      { label: CJK, path: `smb://${ID}/游戏/${CJK}` },
    ]);
    // An unknown share falls back to its id.
    expect(smbSegments(`smb://${ID}`, [])[0].label).toBe(ID);
  });

  it("writes the address the way a user typed it", () => {
    expect(shareAddress(share())).toBe("192.168.1.10/Media");
    expect(shareAddress(share({ port: 4450 }))).toBe("192.168.1.10:4450/Media");
    expect(shareAddress(share({ host: "fe80::1" }))).toBe("[fe80::1]/Media");
  });
});

describe("watchSmbJob", () => {
  it("reports progress until the job ends", async () => {
    const seen: number[] = [];
    const states = [job({ bytes_done: 10 }), job({ bytes_done: 60 }), job({ state: "done", bytes_done: 100 })];
    const end = await watchSmbJob(
      "j1",
      { onStatus: (s) => seen.push(s.bytes_done) },
      { status: async () => states.shift()!, cancel: vi.fn(), sleep: async () => undefined },
    );
    expect(end.end).toBe("done");
    expect(seen).toEqual([10, 60, 100]);
  });

  it("Stop cancels the job once and waits for its end", async () => {
    const cancel = vi.fn(async () => undefined);
    let stop = false;
    let reads = 0;
    const end = await watchSmbJob(
      "j1",
      { stopped: () => stop },
      {
        status: async () => {
          reads++;
          if (reads === 2) stop = true;
          return reads >= 4 ? job({ state: "cancelled", error: "cancelled" }) : job();
        },
        cancel,
        sleep: async () => undefined,
      },
    );
    expect(cancel).toHaveBeenCalledTimes(1);
    expect(end.end).toBe("cancelled");
  });

  it("a job that failed ends as failed with the daemon's reason, whether the helper answers 200 or relays the error's status", async () => {
    const failed = job({ state: "failed", error: "smb_auth", failed: "a/b.pkg" });
    // the fixed helper: 200 with the job
    mockCall.mockResolvedValueOnce(failed as never);
    let end = await watchSmbJob("j1", {});
    expect(end).toEqual({ end: "failed", status: failed });
    // helpers before it: the error's HTTP status, the body still the job (not a miss, not "gone")
    mockCall.mockRejectedValueOnce(new ApiError(403, "smb_auth", failed));
    end = await watchSmbJob("j1", {});
    expect(end).toEqual({ end: "failed", status: failed });
    expect(mockCall).toHaveBeenCalledTimes(2);
    // an error without a job in it is still a miss
    expect(jobOfError(new ApiError(503, "smb_daemon_timeout", { error: "smb_daemon_timeout" }))).toBeNull();
    expect(jobOfError(new ApiError(0, "offline"))).toBeNull();
  });

  it("a daemon that no longer knows the job ends it as gone; a 401 is thrown", async () => {
    const gone = await watchSmbJob("j1", {}, {
      status: async () => {
        throw new ApiError(404, "unknown_job");
      },
      cancel: vi.fn(),
      sleep: async () => undefined,
    });
    expect(gone.end).toBe("gone");
    await expect(
      watchSmbJob("j1", {}, {
        status: async () => {
          throw new ApiError(401, "token");
        },
        cancel: vi.fn(),
        sleep: async () => undefined,
      }),
    ).rejects.toMatchObject({ status: 401 });
  });
});

/** A share and a console, as stat answers, and the copy jobs the transfer starts. */
function world(
  local: Record<string, "file" | "dir">,
  remote: Record<string, "file" | "dir">,
  renameFails: (from: string, to: string) => boolean = () => false,
) {
  const sent: string[] = [];
  const ends: Record<string, SmbJob> = {};
  let n = 0;
  const send = (async (route: string, body?: unknown) => {
    const b = body as Record<string, unknown>;
    sent.push(`${route} ${JSON.stringify(b)}`);
    if (route === "/api/smb/copy") {
      const dest = b.dest as string;
      if (!ends[`c${n + 1}`] || ends[`c${n + 1}`].state === "done") (dest.startsWith("smb://") ? remote : local)[dest] = "dir";
      return { job: `c${++n}` };
    }
    if (route === "/api/smb/delete") {
      delete remote[b.path as string];
      return { job: `d${++n}` };
    }
    if (route === "/api/fs/delete") {
      delete local[b.path as string];
      return { job: `l${++n}` };
    }
    if (route === "/api/fs/rename" || route === "/api/smb/rename") {
      const side = route === "/api/fs/rename" ? local : remote;
      if (renameFails(b.from as string, b.to as string)) throw new ApiError(409, "smb_not_supported");
      side[b.to as string] = side[b.from as string];
      delete side[b.from as string];
      return {};
    }
    throw new Error(`unexpected ${route}`);
  }) as unknown as typeof call;
  const watch = vi.fn(async (id: string, opts?: { onStatus?: (s: SmbJob) => void; stopped?: () => boolean }) => {
    const s = ends[id] ?? job({ job: id, state: "done", bytes_done: 100 });
    opts?.onStatus?.(s);
    return { end: s.state === "running" ? "done" : s.state, status: s } as never;
  });
  return {
    sent,
    ends,
    watch,
    deps: (decide: (c: Conflict) => Promise<Decision>, onBytes?: (d: number, t: number) => void) => ({
      decide,
      onBytes,
      call: send,
      watch: watch as never,
      localStat: async (p: string) => (local[p] ? { kind: local[p] } : null),
      remoteStat: async (p: string) => (remote[p] ? { kind: remote[p] } : null),
    }),
  };
}

const never = async (): Promise<Decision> => {
  throw new Error("no conflict expected");
};

describe("transferItems", () => {
  it("copies share -> console, one job per item, with byte progress", async () => {
    const w = world({}, {});
    const bytes: number[] = [];
    const r = await transferItems(
      [
        { path: `smb://${ID}/${CJK}`, dir: true },
        { path: `smb://${ID}/big.bin`, dir: false },
      ],
      "/data/ps5upload-smb-test",
      w.deps(never, (d) => bytes.push(d)),
    );
    expect(r.done).toEqual([`smb://${ID}/${CJK}`, `smb://${ID}/big.bin`]);
    expect(w.sent).toEqual([
      `/api/smb/copy ${JSON.stringify({ src: `smb://${ID}/${CJK}`, dest: `/data/ps5upload-smb-test/${CJK}` })}`,
      `/api/smb/copy ${JSON.stringify({ src: `smb://${ID}/big.bin`, dest: "/data/ps5upload-smb-test/big.bin" })}`,
    ]);
    expect(bytes).toEqual([100, 100]);
  });

  it("copies console -> share into the share's folder", async () => {
    const w = world({}, {});
    const r = await transferItems([{ path: "/data/a.txt", dir: false }], `smb://${ID}/backup/`, w.deps(never));
    expect(r.done).toEqual(["/data/a.txt"]);
    expect(w.sent[0]).toBe(`/api/smb/copy ${JSON.stringify({ src: "/data/a.txt", dest: `smb://${ID}/backup/a.txt` })}`);
  });

  it("asks about a taken name: skip, keep both, replace a file, replace a folder", async () => {
    const w = world(
      { "/data/a.txt": "file", "/data/a (2).txt": "file", "/data/f": "dir", "/data/g": "file" },
      {},
    );
    const asked: Conflict[] = [];
    const replies: Decision[] = [
      { choice: "skip", all: false },
      { choice: "keep", all: false },
      { choice: "replace", all: false },
      { choice: "replace", all: false },
    ];
    const decide = async (c: Conflict) => {
      asked.push(c);
      return replies.shift()!;
    };
    const items = [
      { path: `smb://${ID}/a.txt`, dir: false },
      { path: `smb://${ID}/a.txt`, dir: false },
      { path: `smb://${ID}/g`, dir: false },
      { path: `smb://${ID}/f`, dir: true },
    ];
    const r = await transferItems(items, "/data", w.deps(decide), { waitJob: async () => "done" });
    expect(asked.map((c) => [c.dest, c.destDir, c.canReplace])).toEqual([
      ["/data/a.txt", false, true],
      ["/data/a.txt", false, true],
      ["/data/g", false, true],
      ["/data/f", true, true],
    ]);
    expect(r.declined).toEqual([`smb://${ID}/a.txt`]);
    expect(w.sent).toEqual([
      `/api/smb/copy ${JSON.stringify({ src: `smb://${ID}/a.txt`, dest: "/data/a (3).txt" })}`,
      `/api/smb/copy ${JSON.stringify({ src: `smb://${ID}/g`, dest: "/data/g", overwrite: true })}`,
      `/api/smb/copy ${JSON.stringify({ src: `smb://${ID}/f`, dest: "/data/f.ps5u-new" })}`,
      `/api/fs/rename ${JSON.stringify({ from: "/data/f", to: "/data/f.ps5u-old" })}`,
      `/api/fs/rename ${JSON.stringify({ from: "/data/f.ps5u-new", to: "/data/f" })}`,
      `/api/fs/delete ${JSON.stringify({ path: "/data/f.ps5u-old" })}`,
    ]);
  });

  it("replacing a folder on a share goes through a temporary name there (daemon renames and a delete job)", async () => {
    const remote: Record<string, "file" | "dir"> = { [`smb://${ID}/f`]: "dir" };
    const w = world({}, remote);
    const r = await transferItems([{ path: "/data/f", dir: true }], `smb://${ID}`, w.deps(async () => ({ choice: "replace", all: false })));
    expect(w.sent).toEqual([
      `/api/smb/copy ${JSON.stringify({ src: "/data/f", dest: `smb://${ID}/f.ps5u-new` })}`,
      `/api/smb/rename ${JSON.stringify({ from: `smb://${ID}/f`, to: `smb://${ID}/f.ps5u-old` })}`,
      `/api/smb/rename ${JSON.stringify({ from: `smb://${ID}/f.ps5u-new`, to: `smb://${ID}/f` })}`,
      `/api/smb/delete ${JSON.stringify({ path: `smb://${ID}/f.ps5u-old` })}`,
    ]);
    expect(r.done).toEqual(["/data/f"]);
    expect(Object.keys(remote)).toEqual([`smb://${ID}/f`]);
  });

  it("a share copy that fails during a Replace leaves the old folder: the reason and the name are reported", async () => {
    const local: Record<string, "file" | "dir"> = { "/data/f": "dir" };
    const w = world(local, {});
    w.ends.c1 = job({ job: "c1", state: "failed", error: "no_space", failed: "f/big.bin" });
    const r = await transferItems([{ path: `smb://${ID}/f`, dir: true }], "/data", w.deps(async () => ({ choice: "replace", all: false })));
    expect(w.sent).toEqual([`/api/smb/copy ${JSON.stringify({ src: `smb://${ID}/f`, dest: "/data/f.ps5u-new" })}`]);
    expect(local).toEqual({ "/data/f": "dir" });
    const err = r.failed[0].error as ReplaceError;
    expect(err).toBeInstanceOf(ReplaceError);
    expect(err).toMatchObject({ what: "copy", item: "f", temp: null });
    expect(err.why).toBeInstanceOf(SmbJobError);
    expect(r.done).toEqual([]);
  });

  it("a dropped share during a Replace's copy is the same: nothing of the old folder goes", async () => {
    const remote: Record<string, "file" | "dir"> = { [`smb://${ID}/f`]: "dir" };
    const w = world({}, remote);
    w.watch.mockImplementationOnce(async () => ({ end: "gone" }) as never);
    const r = await transferItems([{ path: "/data/f", dir: true }], `smb://${ID}`, w.deps(async () => ({ choice: "replace", all: false })));
    expect(w.sent.some((x) => x.startsWith("/api/smb/rename"))).toBe(false);
    expect(remote[`smb://${ID}/f`]).toBe("dir");
    expect((r.failed[0].error as ReplaceError).why).toMatchObject({ code: "smb_job_lost" });
  });

  it("Stop during a Replace's copy cancels it and clears the temporary; the old folder stays", async () => {
    const remote: Record<string, "file" | "dir"> = { [`smb://${ID}/f`]: "dir", [`smb://${ID}/f.ps5u-new`]: "dir" };
    const w = world({}, remote);
    w.ends.c1 = job({ job: "c1", state: "cancelled", error: "cancelled" });
    // A leftover from an earlier run takes the first name: it is kept, the next one is used.
    const r = await transferItems([{ path: "/data/f", dir: true }], `smb://${ID}`, w.deps(async () => ({ choice: "replace", all: false })));
    expect(w.sent).toEqual([
      `/api/smb/copy ${JSON.stringify({ src: "/data/f", dest: `smb://${ID}/f.ps5u-new2` })}`,
    ]);
    expect(remote[`smb://${ID}/f.ps5u-new`]).toBe("dir");
    expect(r.halted).toEqual(["/data/f"]);
    expect(r.skipped).toEqual(["/data/f"]);
    expect(r.done).toEqual([]);
  });

  it("a server that refuses the rename: the old folder keeps its name and the new copy goes", async () => {
    const remote: Record<string, "file" | "dir"> = { [`smb://${ID}/f`]: "dir" };
    const w = world({}, remote, (from) => from === `smb://${ID}/f`);
    const r = await transferItems([{ path: "/data/f", dir: true }], `smb://${ID}`, w.deps(async () => ({ choice: "replace", all: false })));
    expect(remote).toEqual({ [`smb://${ID}/f`]: "dir" });
    expect(w.sent[w.sent.length - 1]).toBe(`/api/smb/delete ${JSON.stringify({ path: `smb://${ID}/f.ps5u-new` })}`);
    expect(r.failed[0].error).toMatchObject({ what: "swap", temp: null });
  });

  it("Stop cancels the running item and leaves the rest", async () => {
    const w = world({}, {});
    w.ends.c1 = job({ job: "c1", state: "cancelled", error: "cancelled" });
    let stop = false;
    const r = await transferItems(
      [
        { path: `smb://${ID}/1.bin`, dir: false },
        { path: `smb://${ID}/2.bin`, dir: false },
      ],
      "/data",
      w.deps(never),
      {
        stopped: () => {
          const s = stop;
          stop = true;
          return s;
        },
      },
    );
    expect(r.done).toEqual([]);
    expect(r.cancelled).toEqual([`smb://${ID}/1.bin`]);
    expect(r.skipped).toEqual([`smb://${ID}/2.bin`]);
    expect(w.sent).toHaveLength(1);
  });

  it("a job that fails reports the daemon's reason", async () => {
    const w = world({}, {});
    w.ends.c1 = job({ job: "c1", state: "failed", error: "smb_disconnected", failed: "big.bin" });
    const r = await transferItems([{ path: `smb://${ID}/big.bin`, dir: false }], "/data", w.deps(never));
    expect(r.failed).toHaveLength(1);
    expect(smbErrorText(r.failed[0].error)).toBe(t("cw_files_smb_err_disconnected"));
  });
});

describe("words", () => {
  it("names the SMB failures and leaves others to the generic text", () => {
    expect(smbErrorText(new ApiError(403, "smb_auth"))).toBe(t("cw_files_smb_err_auth"));
    expect(smbErrorText(new ApiError(502, "smb_unreachable"))).toBe(t("cw_files_smb_err_unreachable"));
    expect(smbErrorText(new ApiError(503, "smb_loader_unavailable"))).toBe(t("cw_files_smb_err_loader"));
    expect(smbErrorText(new ApiError(404, "not_found"))).toBeNull();
    expect(smbErrorText(new Error("x"))).toBeNull();
  });
});

describe("splitShare", () => {
  it("takes the share's own name and moves typed folders into the start folder", () => {
    expect(splitShare("Media", "Games")).toEqual({ share: "Media", start_path: "Games" });
    expect(splitShare(" Media/Games/PS5 ", "")).toEqual({ share: "Media", start_path: "Games/PS5" });
    expect(splitShare("Media/Games", "PS5/New")).toEqual({ share: "Media", start_path: "Games/PS5/New" });
    expect(splitShare("\\\\nas\\Media\\Games", "")).toEqual({ share: "Media", start_path: "Games" });
    expect(splitShare("//nas/Media", "x")).toEqual({ share: "Media", start_path: "x" });
    expect(splitShare("/Media/", "")).toEqual({ share: "Media", start_path: "" });
    expect(splitShare("smb://nas/Media/Games", "x")).toEqual({ share: "Media", start_path: "Games/x" });
    expect(splitShare("CIFS://nas/Media", "")).toEqual({ share: "Media", start_path: "" });
  });
});
