/**
 * Network shares (SMB) in Files: the saved shares, their folders, and copies
 * between a share and the console, run by the helper's SMB daemon
 * (payload/include/webui_smb.h).
 *
 * A place on a share is "smb://<id>/<folder in the share>"; the share's top is
 * "smb://<id>". Those paths go through the same Files views as the console's
 * own (list, select, clipboard, conflicts) but only Copy works with them: a
 * folder or file goes share -> console or console -> share, one job per item,
 * with byte progress, and Stop cancels the running one (the daemon removes its
 * part file). Cut and in-share operations are not offered.
 */
import { ApiError, call } from "../api";
import { t } from "../strings";
import type { ClipItem } from "./clipboard";
import { baseName, each, joinPath, type BatchOptions, type BatchResult } from "./move";
import { freeName, statOrNull, type ConflictChoice, type Decision, type Conflict, type PasteResult } from "./paste";
import { ReplaceError, replaceItem, type Placed, type ReplaceSide } from "./replace";

export const SMB_PREFIX = "smb://";

export interface SmbShare {
  id: string;
  name: string;
  host: string;
  port: number;
  share: string;
  user: string;
  domain: string;
  start_path: string;
  has_password: boolean;
  /** Where it opens: "smb://<id>/<start_path>". */
  path: string;
}

export function isSmbPath(p: string | null | undefined): p is string {
  return typeof p === "string" && p.startsWith(SMB_PREFIX);
}

/** "smb://<id>/<rel>" -> its parts; null when it is not a share path. */
export function smbParts(p: string): { id: string; rel: string } | null {
  if (!isSmbPath(p)) return null;
  const rest = p.slice(SMB_PREFIX.length);
  const slash = rest.indexOf("/");
  const id = slash < 0 ? rest : rest.slice(0, slash);
  if (!/^[0-9a-f]{8}$/.test(id)) return null;
  const rel = slash < 0 ? "" : rest.slice(slash + 1).replace(/\/+$/, "").replace(/^\/+/, "");
  return { id, rel };
}

/** The share's top as Files names it. */
export function smbRoot(id: string): string {
  return `${SMB_PREFIX}${id}`;
}

/** A share path without a trailing slash (the top is "smb://<id>"). */
export function smbClean(p: string): string {
  const parts = smbParts(p);
  if (!parts) return p;
  return parts.rel ? `${smbRoot(parts.id)}/${parts.rel}` : smbRoot(parts.id);
}

/** One level up inside the share; null at its top (back to the drive list). */
export function smbUp(p: string): string | null {
  const parts = smbParts(p);
  if (!parts || !parts.rel) return null;
  const i = parts.rel.lastIndexOf("/");
  return i < 0 ? smbRoot(parts.id) : `${smbRoot(parts.id)}/${parts.rel.slice(0, i)}`;
}

export interface SmbSegment {
  label: string;
  path: string;
}

/** The path bar for a share path: the share's name, then each folder. */
export function smbSegments(p: string, shares: SmbShare[]): SmbSegment[] {
  const parts = smbParts(p);
  if (!parts) return [{ label: p, path: p }];
  const s = shares.find((x) => x.id === parts.id);
  const out: SmbSegment[] = [{ label: s ? s.name : parts.id, path: smbRoot(parts.id) }];
  let at = smbRoot(parts.id);
  for (const name of parts.rel.split("/").filter(Boolean)) {
    at = `${at}/${name}`;
    out.push({ label: name, path: at });
  }
  return out;
}

/** "host/share" (with ":port" when it is not 445). */
export function shareAddress(s: Pick<SmbShare, "host" | "port" | "share">): string {
  const host = s.host.includes(":") ? `[${s.host}]` : s.host;
  return `${host}${s.port && s.port !== 445 ? `:${s.port}` : ""}/${s.share}`;
}

/* ---- the helper's routes ---- */

export async function listShares(): Promise<SmbShare[]> {
  const r = await call<{ shares?: unknown }>("/api/smb/shares");
  return Array.isArray(r?.shares) ? (r.shares as SmbShare[]) : [];
}

export interface ShareForm {
  id?: string;
  name: string;
  server: string;
  share: string;
  user: string;
  /** Left out when editing and the box was left empty: the saved password stays. */
  password?: string;
  domain: string;
  start_path: string;
}

/**
 * A share typed with folders after it (`Media/Games/PS5`, or a pasted `\\nas\Media\Games`): the
 * share is the first name and the folders go in front of the start folder. The server only knows
 * the share's own name, and the helper refuses anything else as "not a share name" (other PS5
 * tools take the whole path in one box, so people type it that way). The engine's SMB sources
 * do the same (remote/api.rs split_share).
 */
export function splitShare(share: string, startPath: string): { share: string; start_path: string } {
  const typed = share.trim();
  const scheme = /^(smb|cifs):\/\//i.exec(typed);
  const raw = scheme ? typed.slice(scheme[0].length) : typed;
  const unc = scheme !== null || raw.startsWith("\\\\") || raw.startsWith("//");
  const segs = raw.split(/[\\/]/).filter((x) => x.trim() !== "");
  if (unc && segs.length >= 2) segs.shift();
  if (segs.length <= 1) return { share: (segs[0] ?? raw).trim(), start_path: startPath };
  const start = startPath.split(/[\\/]/).filter((x) => x !== "");
  return { share: segs[0].trim(), start_path: [...segs.slice(1), ...start].join("/") };
}

/** Sign in, open the folder, then save. The share as saved (the helper never sends a password back). */
export async function saveShare(f: ShareForm): Promise<{ share: SmbShare; dialect: string }> {
  return call<{ share: SmbShare; dialect: string }>("/api/smb/save", { ...f, ...splitShare(f.share, f.start_path) });
}

export async function removeShare(id: string): Promise<void> {
  await call("/api/smb/remove", { id });
}

export interface Discovered {
  host: string;
  port: number;
  /** A share named in the server text ("smb://nas/Media"), else "". */
  share: string;
  path: string;
  shares: { name: string; remark: string }[];
}

/**
 * The disk shares a server offers (servers that refuse a listing answer an error: type the share).
 * Editing a saved share with the password box left empty sends its `id` and no password: the
 * helper looks with the saved one (same server and user, as Save does).
 */
export async function discoverShares(f: { id?: string; server: string; user: string; password?: string; domain: string }): Promise<Discovered> {
  return call<Discovered>("/api/smb/discover", f);
}

export interface SmbStat {
  kind: "file" | "dir" | "other";
  size: number;
  mtime: number;
}

export async function smbStatOrNull(path: string): Promise<SmbStat | null> {
  try {
    const st = await call<SmbStat | { missing: true }>("/api/smb/stat", { path, missing_ok: true });
    return "missing" in st && st.missing === true ? null : (st as SmbStat);
  } catch (e) {
    if (e instanceof ApiError && e.status === 404 && e.code === "not_found") return null;
    throw e;
  }
}

export async function smbMkdir(path: string): Promise<void> {
  await call("/api/smb/mkdir", { path });
}

/** One daemon job (/api/smb/job). */
export interface SmbJob {
  job: string;
  kind: "download" | "upload" | "remove";
  state: "running" | "done" | "failed" | "cancelled";
  bytes_done: number;
  bytes_total: number;
  files_done: number;
  files_total: number;
  scanning: boolean;
  current: string;
  error: string | null;
  failed: string;
  reconnects: number;
}

export interface SmbJobDeps {
  status: (job: string) => Promise<SmbJob>;
  cancel: (job: string) => Promise<void>;
  sleep: (ms: number) => Promise<void>;
  pollMs?: number;
}

export const SMB_POLL_MS = 1000;
/** After Cancel, the job is read this many more times for its end (it stops within a chunk). */
export const SMB_CANCEL_READS = 20;
/** Reads that fail in a row (helper busy, network) before the job counts as lost. */
export const SMB_LOST_READS = 30;

const JOB_STATES = new Set<SmbJob["state"]>(["running", "done", "failed", "cancelled"]);

/**
 * A job's status out of a failed read: helpers before the fix relayed a failed
 * job with its error's HTTP status (403 smb_auth…), the body still being the job
 * itself. Null when the body is not a job (unknown_job, the daemon gone…).
 */
export function jobOfError(err: unknown): SmbJob | null {
  if (!(err instanceof ApiError) || !err.body || typeof err.body !== "object") return null;
  const b = err.body as Record<string, unknown>;
  if (typeof b.job !== "string" || typeof b.state !== "string" || !JOB_STATES.has(b.state as SmbJob["state"])) return null;
  return b as unknown as SmbJob;
}

const defaultJobDeps: SmbJobDeps = {
  status: async (job) => {
    try {
      return await call<SmbJob>("/api/smb/job", { job });
    } catch (e) {
      const s = jobOfError(e);
      if (s) return s;
      throw e;
    }
  },
  cancel: async (job) => {
    await call("/api/smb/cancel", { job });
  },
  sleep: (ms) => new Promise((res) => setTimeout(res, ms)),
};

/** How a watched job ended: its last status, or "gone" (the daemon no longer knows it). */
export type SmbJobEnd = { end: "done" | "failed" | "cancelled"; status: SmbJob } | { end: "gone" };

/**
 * Read `job` until it ends. `stopped()` turning true cancels it (the daemon
 * stops at the next chunk and removes its part file), then waits for its end.
 */
export async function watchSmbJob(
  job: string,
  opts: { onStatus?: (s: SmbJob) => void; stopped?: () => boolean } = {},
  deps: SmbJobDeps = defaultJobDeps,
): Promise<SmbJobEnd> {
  const pollMs = deps.pollMs ?? SMB_POLL_MS;
  let cancelAsked = false;
  let after = 0;
  let misses = 0;
  for (;;) {
    if (!cancelAsked && opts.stopped?.()) {
      cancelAsked = true;
      await deps.cancel(job).catch((e: unknown) => {
        if (e instanceof ApiError && e.status === 401) throw e;
      });
    }
    let s: SmbJob | null = null;
    try {
      s = await deps.status(job);
      misses = 0;
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) throw e;
      if (e instanceof ApiError && e.status === 404) return { end: "gone" };
      if (++misses >= SMB_LOST_READS) return { end: "gone" };
    }
    if (s) {
      opts.onStatus?.(s);
      if (s.state !== "running") return { end: s.state, status: s };
    }
    if (cancelAsked && ++after > SMB_CANCEL_READS) return { end: "gone" };
    await deps.sleep(pollMs);
  }
}

/* ---- copies ---- */

export interface TransferDeps {
  /** Ask about one conflict (the page's dialog). */
  decide: (c: Conflict) => Promise<Decision>;
  /** Bytes of the item being copied. */
  onBytes?: (done: number, total: number) => void;
  /** Overrides (tests). */
  call?: typeof call;
  watch?: typeof watchSmbJob;
  localStat?: (path: string) => Promise<{ kind: string } | null>;
  remoteStat?: (path: string) => Promise<{ kind: string } | null>;
}

/** A job that ended failed, as the error the batch reports ("smb_disconnected" …). */
export class SmbJobError extends ApiError {
  readonly failedAt: string;
  constructor(code: string, failedAt: string) {
    super(502, code);
    this.name = "SmbJobError";
    this.failedAt = failedAt;
  }
}

/**
 * Copy each item into `destDir`, where exactly one of the two is on a share.
 * A name that is taken is asked about: Skip, Keep both ("name (2).ext"), or
 * Replace (a file over a file: the daemon replaces it through its part file; a
 * folder, or a file against a folder: through a temporary name beside the old
 * one, which goes only once the new one has its place: replace.ts). Stop
 * cancels the job running and leaves the rest.
 */
export async function transferItems(
  items: ClipItem[],
  destDir: string,
  deps: TransferDeps,
  opts?: BatchOptions,
): Promise<PasteResult> {
  const send = deps.call ?? call;
  const watch = deps.watch ?? watchSmbJob;
  const down = isSmbPath(items[0]?.path);
  const destStat = down ? (deps.localStat ?? statOrNull) : (deps.remoteStat ?? smbStatOrNull);
  const dir = destDir.startsWith(SMB_PREFIX) ? smbClean(destDir) : destDir.replace(/\/+$/, "") || "/";
  const declined: string[] = [];
  const cancelled: string[] = [];
  const halted: string[] = [];
  const notes: ReplaceError[] = [];
  const kinds = new Map(items.map((i) => [i.path, i.dir]));
  const paths = items.map((i) => i.path);
  let always: ConflictChoice | null = null;

  const side: ReplaceSide = {
    taken: async (p) => (await destStat(p)) != null,
    rename: async (from, to) => {
      await send(down ? "/api/fs/rename" : "/api/smb/rename", { from, to });
    },
    remove: async (p) => {
      const { job } = await send<{ job: string }>(down ? "/api/fs/delete" : "/api/smb/delete", { path: p });
      if (!down) await watch(job, {});
      else if (opts?.waitJob) await opts.waitJob(job, true);
      return (await destStat(p)) == null;
    },
  };

  const copyJob = async (item: string, dest: string, res: BatchResult, overwrite: boolean) => {
    const { job } = await send<{ job: string }>("/api/smb/copy", overwrite ? { src: item, dest, overwrite } : { src: item, dest });
    res.started.push(job);
    return watch(job, {
      stopped: opts?.stopped,
      onStatus: (s) => deps.onBytes?.(s.bytes_done, s.bytes_total),
    });
  };

  /** Replace `dest` with `item`, through a temporary name (replace.ts). */
  const replace = async (item: string, dest: string, res: BatchResult): Promise<void> => {
    const place = async (temp: string): Promise<Placed> => {
      const end = await copyJob(item, temp, res, false);
      if (end.end === "done") return { end: "done" };
      if (end.end === "cancelled") return { end: "stopped" };
      if (end.end === "gone") return { end: "failed", why: new ApiError(502, "smb_job_lost") };
      return { end: "failed", why: new SmbJobError(end.status.error ?? "io_error", end.status.failed) };
    };
    const r = await replaceItem(side, dir, dest, place);
    const name = baseName(dest);
    if (r.end === "stopped") {
      halted.push(item);
      if (r.temp) notes.push(new ReplaceError("temp_left", name, { temp: r.temp }));
    } else if (r.old) {
      notes.push(new ReplaceError("old_left", name, { old: r.old }));
    }
  };

  const r = await each(paths, { onProgress: opts?.onProgress, stopped: opts?.stopped }, async (item, res) => {
    const isDir = kinds.get(item) ?? false;
    let dest = joinPath(dir, baseName(item));
    let overwrite = false;
    const existing = await destStat(dest);
    if (existing) {
      let choice = always;
      if (!choice) {
        const d = await deps.decide({
          item,
          dest,
          destDir: existing.kind === "dir",
          canReplace: true,
          left: paths.length - 1 - paths.indexOf(item),
        });
        choice = d.choice;
        if (d.all) always = d.choice;
      }
      if (choice === "skip") {
        declined.push(item);
        return;
      }
      if (choice === "keep") {
        dest = joinPath(dir, await freeName(dir, baseName(item), isDir, async (p) => (await destStat(p)) != null));
      } else if (!isDir && existing.kind !== "dir") {
        overwrite = true;
      } else {
        return replace(item, dest, res);
      }
    }
    const end = await copyJob(item, dest, res, overwrite);
    if (end.end === "done") return;
    if (end.end === "cancelled") {
      cancelled.push(item);
      return;
    }
    if (end.end === "gone") throw new ApiError(502, "smb_job_lost");
    throw new SmbJobError(end.status.error ?? "io_error", end.status.failed);
  });

  const notDone = new Set<string>(declined.concat(cancelled, halted, r.skipped, r.jobFailed, r.failed.map((f) => f.item)));
  return {
    ...r,
    cancelled,
    skipped: halted.concat(r.skipped),
    done: paths.filter((p) => !notDone.has(p)),
    left: [],
    halted,
    declined,
    here: [],
    notes,
  };
}

/* ---- words ---- */

/** Why a share call or copy failed, by the helper's / daemon's token; null when it is not an SMB one. */
export function smbErrorText(err: unknown): string | null {
  if (!(err instanceof ApiError)) return null;
  switch (err.code) {
    case "smb_auth":
      return t("cw_files_smb_err_auth");
    case "smb_unreachable":
      return t("cw_files_smb_err_unreachable");
    case "smb_timeout":
      return t("cw_files_smb_err_timeout");
    case "smb_disconnected":
    case "smb_job_lost":
      return t("cw_files_smb_err_disconnected");
    case "smb_no_share":
      return t("cw_files_smb_err_no_share");
    case "smb_loader_unavailable":
      return t("cw_files_smb_err_loader");
    case "smb_daemon_unavailable":
    case "smb_daemon_timeout":
      return t("cw_files_smb_err_daemon");
    case "unknown_share":
      return t("cw_files_smb_err_unknown");
    case "shares_full":
      return t("cw_files_smb_err_full");
    case "smb_not_supported":
      return t("cw_files_smb_err_not_supported");
    default:
      return null;
  }
}

/**
 * smbErrorText, plus the tokens other routes use too (busy, no_space…), worded for a share: only
 * where the error surely came from a share call (the share dialog, a share copy's job).
 */
export function smbCallErrorText(err: unknown): string | null {
  const own = smbErrorText(err);
  if (own || !(err instanceof ApiError)) return own;
  switch (err.code) {
    case "no_space":
      return t("cw_files_smb_err_no_space");
    case "bad_name":
      return t("cw_files_smb_err_bad_name");
    case "bad_host":
      return t("cw_files_smb_err_bad_host");
    case "bad_share":
      return t("cw_files_smb_err_bad_share");
    case "busy":
      return t("cw_files_smb_err_busy");
    default:
      return null;
  }
}
