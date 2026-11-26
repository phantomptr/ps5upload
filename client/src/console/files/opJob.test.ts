import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api", async (importOriginal) => {
  const real = await importOriginal<typeof import("../api")>();
  return { ...real, call: vi.fn() };
});

import { ApiError, call } from "../api";
import { checkUnavailable, crcHex, isFfpfsc, opCause, opResult, startOp, unpackBlocker, watchOp, type OpDeps, type OpStatus } from "./opJob";
import { runVerify, type VerifyState } from "./VerifyDialog";

const mockCall = vi.mocked(call);

const st = (over: Partial<OpStatus> = {}): OpStatus => ({
  job: "j",
  state: "running",
  files_done: 0,
  files_total: 1,
  bytes_done: 0,
  bytes_total: 10,
  message: null,
  result: null,
  code: null,
  ...over,
});

function deps(seq: (OpStatus | Error)[], cancel = vi.fn(async () => undefined)): OpDeps & { cancel: typeof cancel } {
  let i = 0;
  return {
    status: vi.fn(async () => {
      const s = seq[Math.min(i++, seq.length - 1)];
      if (s instanceof Error) throw s;
      return s;
    }),
    cancel,
    sleep: async () => undefined,
  };
}

beforeEach(() => mockCall.mockReset());

describe("operation routes", () => {
  it("hash, crc32 and unpack send {path} (and unpack's Keep both name) and return the job", async () => {
    mockCall.mockResolvedValue({ job: "ab" });
    expect(await startOp("/api/fs/hash", { path: "/data/a b.bin" })).toBe("ab");
    expect(await startOp("/api/fs/crc32", { path: "/data/a b.bin" })).toBe("ab");
    expect(await startOp("/api/fs/unpack", { path: "/mnt/usb0/x.ffpfsc", name: "x (2).exfat" })).toBe("ab");
    expect(mockCall.mock.calls).toEqual([
      ["/api/fs/hash", { path: "/data/a b.bin" }],
      ["/api/fs/crc32", { path: "/data/a b.bin" }],
      ["/api/fs/unpack", { path: "/mnt/usb0/x.ffpfsc", name: "x (2).exfat" }],
    ]);
    mockCall.mockResolvedValue({});
    await expect(startOp("/api/fs/hash", { path: "/x" })).rejects.toBeInstanceOf(ApiError);
  });
});

describe("watchOp", () => {
  it("reads until the job ends and reports each status", async () => {
    const seen: number[] = [];
    const d = deps([st({ bytes_done: 2 }), new ApiError(0, "offline"), st({ bytes_done: 6 }), st({ state: "done", bytes_done: 10, result: '{"x":1}' })]);
    const end = await watchOp("j", { onStatus: (s) => seen.push(s.bytes_done) }, d);
    expect(end.end).toBe("done");
    expect(seen).toEqual([2, 6, 10]);
    expect(opResult<{ x: number }>(end.end === "done" ? end.status : null)).toEqual({ x: 1 });
  });

  it("a failed job keeps its cause", async () => {
    const end = await watchOp("j", {}, deps([st({ state: "failed", message: "fs_unpack_no_space: needs 5 bytes, 1 free", code: 1 })]));
    expect(end.end).toBe("failed");
    expect(opCause(end.end === "failed" ? end.status : null)).toBe("fs_unpack_no_space");
  });

  it("Stop cancels the job once and waits for it to stop", async () => {
    const d = deps([st(), st(), st({ state: "failed", message: "fs_hash_cancelled" })]);
    const end = await watchOp("j", { stopped: () => true }, d);
    expect(d.cancel).toHaveBeenCalledTimes(1);
    expect(d.cancel).toHaveBeenCalledWith("j");
    expect(end.end).toBe("cancelled");
  });

  it("a job that never stops after Cancel is given up on (bounded)", async () => {
    const d = deps([st()]);
    const end = await watchOp("j", { stopped: () => true }, d);
    expect(end.end).toBe("gone");
    expect(vi.mocked(d.status).mock.calls.length).toBeLessThanOrEqual(12);
  });

  it("a job no longer listed is gone; a 401 is thrown", async () => {
    expect((await watchOp("j", {}, deps([new ApiError(404, "unknown_job")]))).end).toBe("gone");
    await expect(watchOp("j", {}, deps([new ApiError(401, "token")]))).rejects.toBeInstanceOf(ApiError);
  });
});

describe("helpers", () => {
  it("CRC32 as 8 hex digits", () => {
    expect(crcHex(0)).toBe("00000000");
    expect(crcHex(0xcbf43926)).toBe("CBF43926");
    expect(crcHex(255)).toBe("000000FF");
  });

  it(".ffpfsc in any case", () => {
    expect(isFfpfsc("game.ffpfsc")).toBe(true);
    expect(isFfpfsc("GAME.FFPFSC")).toBe(true);
    expect(isFfpfsc("game.ffpfs")).toBe(false);
    expect(isFfpfsc("game.exfat")).toBe(false);
  });

  it("a decompress is held for a taken name (until Keep both) and for too little room", () => {
    const info = { name: "g.exfat", size: 100, need: 200, free: 500, exists: false, compressed: true };
    expect(unpackBlocker(info, null)).toBeNull();
    expect(unpackBlocker({ ...info, exists: true }, null)).toBe("exists");
    expect(unpackBlocker({ ...info, exists: true }, "g (2).exfat")).toBeNull();
    expect(unpackBlocker({ ...info, free: 199 }, null)).toBe("space");
    expect(unpackBlocker({ ...info, free: null }, null)).toBeNull();
  });
});

describe("runVerify", () => {
  it("runs HASH then CRC32 and shows both", async () => {
    const start = vi.fn(async (route: string, _body: { path: string }) => (route === "/api/fs/hash" ? "h" : "c"));
    const status = vi.fn(async (job: string) =>
      job === "h"
        ? st({ job, state: "done", bytes_done: 10, result: '{"path":"/x","size":10,"hash":"af1349b9"}' })
        : st({ job, state: "done", bytes_done: 10, result: '{"crc32":3421780262,"size":10}' }),
    );
    const states: VerifyState["step"][] = [];
    const r = await runVerify("/data/x.bin", (s) => states.push(s.step), () => false, {
      start: start as never,
      op: { status, cancel: async () => undefined, sleep: async () => undefined },
    });
    expect(start.mock.calls.map((c) => c[0])).toEqual(["/api/fs/hash", "/api/fs/crc32"]);
    expect(start.mock.calls[0][1]).toEqual({ path: "/data/x.bin" });
    expect(r).toEqual({ step: "done", hash: "af1349b9", crc: "CBF43926", size: 10 });
    expect(states[0]).toBe("hash");
    expect(states).toContain("crc");
  });

  it("a failed hash says why; Stop is cancelled", async () => {
    const status = vi.fn(async () => st({ state: "failed", message: "fs_hash_not_regular_file" }));
    const r = await runVerify("/d", () => undefined, () => false, {
      start: (async () => "h") as never,
      op: { status, cancel: async () => undefined, sleep: async () => undefined },
    });
    expect(r.step).toBe("failed");
    const stopped = await runVerify("/d", () => undefined, () => true, { start: (async () => "h") as never });
    expect(stopped).toEqual({ step: "cancelled" });
  });
});

describe("the check before Decompress and Extract", () => {
  it("lets the helper decide when this helper has no check (404 not_found), not when the file is gone", () => {
    expect(checkUnavailable(new ApiError(404, "not_found"))).toBe(true);
    expect(checkUnavailable(new ApiError(404, "http_404"))).toBe(true);
    expect(checkUnavailable(new ApiError(0, "offline"))).toBe(true);
    expect(checkUnavailable(new ApiError(503, "busy"))).toBe(true);
    expect(checkUnavailable(new ApiError(404, "fs_unpack_open_failed"))).toBe(false);
    expect(checkUnavailable(new ApiError(403, "path_not_allowed"))).toBe(false);
  });
});
