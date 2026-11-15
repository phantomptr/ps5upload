/**
 * The helper's operation jobs the page starts on one file (Verify: HASH and
 * CRC32; Decompress: FFPFSC_UNPACK; Extract: ARCHIVE_EXTRACT; Convert: CONSOLE_CONVERT, see
 * convert/model.ts) and their outcome, read from
 * /api/jobs/status (progress, the operation's message, and its result once it
 * ended well). Reading a finished job there releases it (after a short grace),
 * so it never holds one of the job table's slots.
 */
import { ApiError, call } from "../api";
import { cancelJob } from "../home/useJobs";

export interface OpStatus {
  job: string;
  state: "running" | "done" | "failed";
  files_done: number;
  files_total: number;
  bytes_done: number;
  bytes_total: number;
  /** The operation's line; on a failure its cause token (fs_unpack_no_space: …). */
  message: string | null;
  /** The operation's result (JSON text), once it ended well. */
  result: string | null;
  code: number | null;
}

/** How often a running operation is read. */
export const OP_POLL_MS = 1000;
/** After Cancel, the job is read this many more times for its end (it stops within a block). */
export const OP_CANCEL_READS = 10;

export interface OpDeps {
  status: (job: string) => Promise<OpStatus>;
  cancel: (job: string) => Promise<void>;
  sleep: (ms: number) => Promise<void>;
  pollMs?: number;
}

const defaultDeps: OpDeps = {
  status: (job) => call<OpStatus>("/api/jobs/status", { job }),
  cancel: cancelJob,
  sleep: (ms) => new Promise((res) => setTimeout(res, ms)),
};

/** POST `route` {path, …} and return the job it started. */
export async function startOp(
  route: "/api/fs/hash" | "/api/fs/crc32" | "/api/fs/unpack" | "/api/fs/extract" | "/api/convert/inspect" | "/api/convert/start",
  body: { path: string; name?: string } | ExtractBody | { path: string; dest: string; format: string; zstd: boolean; zstd_level?: number; verify: string },
): Promise<string> {
  const r = await call<{ job?: unknown }>(route, body);
  if (!r || typeof r.job !== "string") throw new ApiError(500, "bad_reply");
  return r.job;
}

/** How a watched operation ended: its last status, "gone" (no longer listed), or "cancelled" (Cancel asked and it stopped). */
export type OpEnd = { end: "done" | "failed" | "cancelled"; status: OpStatus } | { end: "gone" };

/**
 * Read job `job` until it is no longer running. `stopped()` turning true asks
 * the helper to cancel it, then waits (bounded) for it to stop. A read that
 * fails (offline) is retried; a 401 is thrown.
 */
export async function watchOp(
  job: string,
  opts: { onStatus?: (s: OpStatus) => void; stopped?: () => boolean } = {},
  deps: OpDeps = defaultDeps,
): Promise<OpEnd> {
  const pollMs = deps.pollMs ?? OP_POLL_MS;
  let cancelAsked = false;
  let after = 0;
  for (;;) {
    if (!cancelAsked && opts.stopped?.()) {
      cancelAsked = true;
      await deps.cancel(job).catch((e: unknown) => {
        if (e instanceof ApiError && e.status === 401) throw e;
      });
    }
    let s: OpStatus | null = null;
    try {
      s = await deps.status(job);
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) throw e;
      if (e instanceof ApiError && e.status === 404) return { end: "gone" };
    }
    if (s) {
      opts.onStatus?.(s);
      if (s.state !== "running") {
        if (s.state === "failed" && cancelAsked) return { end: "cancelled", status: s };
        return { end: s.state, status: s };
      }
    }
    if (cancelAsked && ++after > OP_CANCEL_READS) return { end: "gone" };
    await deps.sleep(pollMs);
  }
}

/** The operation's result parsed, or null. */
export function opResult<T>(s: OpStatus | null | undefined): T | null {
  if (!s?.result) return null;
  try {
    return JSON.parse(s.result) as T;
  } catch {
    return null;
  }
}

/** A failed operation's cause token: the message up to its first ":" or space ("fs_unpack_no_space"). */
export function opCause(s: OpStatus | null | undefined): string {
  const m = s?.message ?? "";
  const cut = m.search(/[:\s]/);
  return cut < 0 ? m : m.slice(0, cut);
}

/** CRC-32 as 8 uppercase hex digits. */
export function crcHex(n: number): string {
  const h = (n >>> 0).toString(16).toUpperCase();
  return h.length >= 8 ? h : `${"0".repeat(8 - h.length)}${h}`;
}

/** "x.ffpfsc" (any case) can be decompressed. */
export function isFfpfsc(name: string): boolean {
  return /\.ffpfsc$/i.test(name);
}

/* ---- decompress ---- */

/** /api/fs/unpack/info: what a decompress would write, before it starts. */
export interface UnpackInfo {
  /** The image's own file name (written beside the .ffpfsc). */
  name: string;
  size: number;
  /** Room it needs: the image plus the helper's 1 GiB margin. */
  need: number;
  /** Free space beside it; null when the helper could not read it. */
  free: number | null;
  /** A file with that name is already there. */
  exists: boolean;
  compressed: boolean;
}

export async function unpackInfo(path: string): Promise<UnpackInfo> {
  return call<UnpackInfo>("/api/fs/unpack/info", { path });
}

/** Why a decompress cannot start (checked on the page before it is sent), or null. */
export function unpackBlocker(info: UnpackInfo, name: string | null): "exists" | "space" | null {
  if (info.exists && (name == null || name === info.name)) return "exists";
  if (info.free != null && info.free < info.need) return "space";
  return null;
}

/* ---- extract an archive ---- */

/** What /api/fs/extract is sent: the archive (its first volume), the folder, and how. */
export interface ExtractBody {
  path: string;
  dest: string;
  password?: string;
  /** Write into `dest` although it is there; files already there are skipped unless `replace`. */
  merge?: boolean;
  replace?: boolean;
}

/** /api/fs/extract/info: where an extract would go, before it starts. */
export interface ExtractInfo {
  /** The default folder's name (the archive's name without its extensions). */
  name: string;
  /** The default folder's path, next to the archive. */
  dest: string;
  /** The archive's size, every volume of a split one. */
  size: number;
  volumes: number;
  /** Free space beside it; null when the helper could not read it. */
  free: number | null;
  /** The default folder is already there. */
  exists: boolean;
  kind: string;
}

export async function extractInfo(path: string): Promise<ExtractInfo> {
  return call<ExtractInfo>("/api/fs/extract/info", { path });
}
