/**
 * The helper's jobs, every owner's (the PC app's uploads and copies as well as
 * the page's own), read from /api/jobs: about once a second while one runs,
 * every five seconds otherwise, not at all while the page is hidden (the
 * browser in the background). The poller stops when the section unmounts and
 * for good on a 401 (the token is gone; the reload banner takes over).
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, call } from "../api";
import type { VisibilityDoc } from "../ui/visiblePoll";

/** One /api/jobs entry (payload/src/webui_codec.c webui_jobs_to_json). */
export interface Job {
  /** 32 lowercase hex chars. */
  job: string;
  kind: "upload" | "download" | "copy" | "op" | "other";
  state: "running" | "done" | "failed";
  files_done: number;
  files_total: number;
  bytes_done: number;
  bytes_total: number;
  /** Started from this page (false: a paired PC's job). */
  mine: boolean;
  /** What it works on, when the job says (a network-share copy names its file or folder). */
  name?: string;
}

/** Network-share jobs carry this in front of the daemon's id, so cancel goes to the right place. */
export const SMB_JOB_PREFIX = "smb:";
/** A finished network-share job stays listed this long (the daemon keeps its last 16 jobs). */
export const SMB_DONE_KEEP_S = 15 * 60;

/** One /api/smb/jobs entry (payload/smb/smbd_jobs.c status_locked). */
interface SmbDaemonJob {
  job?: unknown;
  kind?: unknown;
  state?: unknown;
  bytes_done?: unknown;
  bytes_total?: unknown;
  files_done?: unknown;
  files_total?: unknown;
  started?: unknown;
  ended?: unknown;
  src?: unknown;
  dest?: unknown;
}

const num = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? v : 0);
const lastName = (p: unknown) =>
  typeof p === "string" ? (p.split("/").filter(Boolean).pop() ?? "") : "";

/**
 * A network-share (NAS) copy or removal the page started, as a Tasks row: the
 * SMB daemon runs these, not the helper, so /api/jobs never lists them (#658).
 * Library scans are left out (Library shows those), as is a finished job older
 * than SMB_DONE_KEEP_S.
 */
export function smbJobToJob(s: SmbDaemonJob, nowS: number): Job | null {
  if (typeof s.job !== "string" || !s.job) return null;
  const kind = s.kind === "download" || s.kind === "upload" ? "copy" : s.kind === "remove" ? "op" : null;
  if (!kind) return null;
  const state = s.state === "running" ? "running" : s.state === "done" ? "done" : "failed";
  // From when it ended (a daemon before "ended": when it started, which hid a long copy as it finished).
  const endedS = num(s.ended) || num(s.started);
  if (state !== "running" && endedS > 0 && nowS - endedS > SMB_DONE_KEEP_S) return null;
  const name = lastName(s.kind === "download" ? s.dest : s.src);
  return {
    job: SMB_JOB_PREFIX + s.job,
    kind,
    state,
    files_done: num(s.files_done),
    files_total: num(s.files_total),
    bytes_done: num(s.bytes_done),
    bytes_total: num(s.bytes_total),
    mine: true,
    ...(name ? { name } : {}),
  };
}

/** The network-share daemon's jobs; none when it is not running or cannot be read. */
async function fetchSmbJobs(): Promise<Job[]> {
  try {
    const r = await call<{ jobs?: unknown }>("/api/smb/jobs");
    const nowS = Math.floor(Date.now() / 1000);
    return Array.isArray(r?.jobs)
      ? (r.jobs as SmbDaemonJob[]).map((j) => smbJobToJob(j, nowS)).filter((j): j is Job => j != null)
      : [];
  } catch {
    return [];
  }
}

export const JOBS_FAST_MS = 1000;
export const JOBS_SLOW_MS = 5000;

/** How long until the next read after `jobs`. */
export function nextJobsDelay(jobs: Job[]): number {
  return jobs.some((j) => j.state === "running") ? JOBS_FAST_MS : JOBS_SLOW_MS;
}

/**
 * The Tasks list: every running job whoever started it, but a finished,
 * failed or cancelled one only when this page started it (the PC app reports
 * its own outcomes). Running ones first. Polling still watches all jobs.
 */
export function visibleJobs(jobs: Job[]): Job[] {
  const running = jobs.filter((j) => j.state === "running");
  return running.concat(jobs.filter((j) => j.state !== "running" && j.mine));
}

/** One read of /api/jobs (and of the network-share jobs): the jobs, and where running uploads
 *  (a PC's) are writing. */
export async function fetchJobsAll(): Promise<{ jobs: Job[]; uploading: string[] }> {
  const r = await call<{ jobs?: unknown; uploading?: unknown }>("/api/jobs");
  const smb = await fetchSmbJobs();
  const uploading = Array.isArray(r?.uploading)
    ? r.uploading.filter((x): x is string => typeof x === "string" && x.startsWith("/"))
    : [];
  return { jobs: (Array.isArray(r?.jobs) ? (r.jobs as Job[]) : []).concat(smb), uploading };
}

/** One read of /api/jobs. */
export async function fetchJobs(): Promise<Job[]> {
  return (await fetchJobsAll()).jobs;
}

/** Ask the helper to stop job `id` (any owner's). */
export async function cancelJob(id: string): Promise<void> {
  if (id.startsWith(SMB_JOB_PREFIX)) await call("/api/smb/cancel", { job: id.slice(SMB_JOB_PREFIX.length) });
  else await call("/api/jobs/cancel", { job: id });
}

export interface JobsPoller {
  start(): void;
  stop(): void;
  /** Read now (after a cancel), never two reads at once. */
  refresh(): void;
}

/** The polling loop behind useJobs, with no React in it. */
export function createJobsPoller(opts: {
  onJobs: (jobs: Job[], uploading: string[]) => void;
  onError: (err: unknown) => void;
  /** Where "hidden" is read (tests); the page's document otherwise. */
  doc?: VisibilityDoc;
}): JobsPoller {
  const doc = opts.doc ?? (typeof document !== "undefined" ? document : null);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let stopped = false;
  let inFlight = false;
  let again = false;
  /** The page is hidden: nothing is scheduled until it shows again, then a read at once. */
  let paused = false;

  const schedule = (ms: number) => {
    if (stopped || paused) return;
    if (timer !== undefined) clearTimeout(timer);
    timer = setTimeout(tick, ms);
  };

  const onVisibility = () => {
    if (stopped || !doc) return;
    if (doc.hidden) {
      paused = true;
      if (timer !== undefined) clearTimeout(timer);
      timer = undefined;
    } else if (paused) {
      paused = false;
      schedule(0);
    }
  };

  async function tick(): Promise<void> {
    timer = undefined;
    if (stopped) return;
    if (inFlight) {
      again = true;
      return;
    }
    inFlight = true;
    let delay = JOBS_SLOW_MS;
    try {
      const { jobs, uploading } = await fetchJobsAll();
      if (stopped) return;
      opts.onJobs(jobs, uploading);
      delay = nextJobsDelay(jobs);
    } catch (e) {
      if (stopped) return;
      opts.onError(e);
      if (e instanceof ApiError && e.status === 401) {
        stopped = true;
        return;
      }
    } finally {
      inFlight = false;
    }
    if (again) {
      again = false;
      delay = 0;
    }
    schedule(delay);
  }

  return {
    start() {
      if (doc) {
        doc.addEventListener("visibilitychange", onVisibility);
        paused = doc.hidden;
      }
      schedule(0);
    },
    stop() {
      stopped = true;
      doc?.removeEventListener("visibilitychange", onVisibility);
      if (timer !== undefined) clearTimeout(timer);
      timer = undefined;
    },
    refresh() {
      if (stopped) return;
      if (inFlight) again = true;
      else schedule(0);
    },
  };
}

export interface JobsState {
  jobs: Job[];
  /** Where running uploads (a PC's) are writing: Files badges those folders. */
  uploading: string[];
  /** The first answer has come in. */
  loaded: boolean;
  /** The last read failed (null after a good one). */
  error: unknown;
  /** Stop job `id`; throws the ApiError when the helper refuses. Re-reads either way. */
  cancel(id: string): Promise<void>;
  /** Read the jobs again now (Create = refresh). */
  refresh(): void;
}

export function useJobs(): JobsState {
  const [jobs, setJobs] = useState<Job[]>([]);
  const [uploading, setUploading] = useState<string[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const poller = useRef<JobsPoller | null>(null);

  useEffect(() => {
    const p = createJobsPoller({
      onJobs: (j, u) => {
        setJobs(j);
        setUploading((prev) => (prev.length === u.length && prev.every((x, i) => x === u[i]) ? prev : u));
        setLoaded(true);
        setError(null);
      },
      onError: setError,
    });
    poller.current = p;
    p.start();
    return () => {
      p.stop();
      if (poller.current === p) poller.current = null;
    };
  }, []);

  const cancel = useCallback(async (id: string) => {
    try {
      await cancelJob(id);
    } finally {
      poller.current?.refresh();
    }
  }, []);

  const refresh = useCallback(() => {
    poller.current?.refresh();
  }, []);

  return { jobs, uploading, loaded, error, cancel, refresh };
}
