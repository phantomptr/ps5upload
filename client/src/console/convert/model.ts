/**
 * Convert on the console: what the page sends to the helper, what it reads back, and the one
 * conversion running.
 *
 * Both steps are helper jobs (CONSOLE_CONVERT, payload/src/console_convert.c), run by the
 * converter payload the helper starts: /api/convert/inspect reads what a game folder or image
 * is, /api/convert/start writes it in another format on the same console. Progress, the result
 * and Stop go through the job routes every other operation uses (files/opJob.ts).
 *
 * The conversion lives outside any component, like Files' batch: it keeps going while another
 * section is open, and Convert shows its progress again when it comes back.
 */
import { useEffect, useState } from "react";
import { ApiError } from "../api";
import { errorText } from "../home/Home";
import { opCause, opResult, startOp, watchOp, type OpEnd, type OpStatus } from "../files/opJob";
import { t } from "../strings";

/** What a conversion writes. The helper names the same four (cc_format_ok). */
export type OutFormat = "exfat" | "ffpkg" | "ffpfs" | "folder";
export const OUT_FORMATS: OutFormat[] = ["exfat", "ffpkg", "ffpfs", "folder"];

/** The zstd levels offered (the PC app's ImageOptions offers the same three). */
export const ZSTD_LEVELS = [1, 5, 19] as const;
export type ZstdLevel = (typeof ZSTD_LEVELS)[number];
export const DEFAULT_ZSTD_LEVEL: ZstdLevel = 5;

export type Verify = "fast" | "full";

/** An image the converter reads (and writes): the console mounts by extension. */
const IMAGE_EXT = /\.(exfat|ffpkg|ffpfs)$/i;

/** Whether a file named `name` can be converted (folders always can be picked). */
export function isConvertImage(name: string): boolean {
  return IMAGE_EXT.test(name);
}

/** The last part of a path. */
export function lastPart(path: string): string {
  return path.replace(/\/+$/, "").split("/").pop() ?? "";
}

/** The folder a path is in ("/data" for "/data/x"). */
export function parentOf(path: string): string {
  const p = path.replace(/\/+$/, "");
  const at = p.lastIndexOf("/");
  return at <= 0 ? "/" : p.slice(0, at);
}

/** The output's default name: the source's name without an image extension, plus the format's. */
export function outputName(source: string, format: OutFormat): string {
  const stem = lastPart(source).replace(IMAGE_EXT, "") || "game";
  return format === "folder" ? stem : `${stem}.${format}`;
}

/** What /api/convert/inspect's job answers (engine/crates/ps5upload-convert Inspection). */
export interface Inspection {
  path: string;
  format: string;
  size: number;
  files: number;
  dataBytes: number;
  titleId: string | null;
  contentId: string | null;
  title: string | null;
  contentVersion: string | null;
  masterVersion: string | null;
  /** "lz4" or "zstd": the game reads its files through AMPR from asset packs. */
  packed: string | null;
  hasEboot: boolean;
  warnings: string[];
  /** The zstd runtime on the console (the helper adds it), or null: a zstd build needs one, and
   *  only the desktop app puts it there. */
  zstdRuntime: string | null;
}

const str = (v: unknown) => (typeof v === "string" && v ? v : null);
const num = (v: unknown) => (typeof v === "number" && Number.isFinite(v) && v >= 0 ? v : 0);

/** The inspection read from a job's result, or null when it is not one. */
export function parseInspection(raw: unknown): Inspection | null {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.path !== "string" || typeof r.format !== "string") return null;
  return {
    path: r.path,
    format: r.format,
    size: num(r.size),
    files: num(r.files),
    dataBytes: num(r.data_bytes),
    titleId: str(r.title_id),
    contentId: str(r.content_id),
    title: str(r.title),
    contentVersion: str(r.content_version),
    masterVersion: str(r.master_version),
    packed: str(r.packed),
    hasEboot: r.has_eboot === true,
    warnings: Array.isArray(r.warnings) ? r.warnings.filter((w): w is string => typeof w === "string") : [],
    zstdRuntime: str(r.zstd_runtime),
  };
}

/** A format's name as the page shows it. */
export function formatLabel(format: string): string {
  switch (format) {
    case "folder":
      return t("cw_convert_format_folder");
    case "exfat":
      return t("cw_convert_format_exfat");
    case "ffpkg":
      return t("cw_convert_format_ffpkg");
    case "ffpfs":
      return t("cw_convert_format_ffpfs");
  }
  return format;
}

/**
 * Every cause the helper or the converter ends a convert job (or refuses a convert route) with,
 * as a sentence: console_convert.c's convert_*, the converter's own (ps5upload-convert job.rs),
 * and /api/convert/start's bad_*. Exported for the test that checks each has its own words.
 */
export const CONVERT_CAUSES: Record<string, string> = {
  convert_not_built_yet: "cw_convert_err_not_built_yet",
  convert_not_bundled: "cw_convert_err_not_bundled",
  convert_busy: "cw_convert_err_busy",
  convert_loader_unreachable: "cw_convert_err_loader",
  convert_not_started: "cw_convert_err_not_started",
  convert_died: "cw_convert_err_died",
  convert_no_memory: "cw_convert_err_no_memory",
  convert_no_space: "cw_convert_err_no_space",
  convert_partial_in_the_way: "cw_convert_err_partial_in_the_way",
  convert_no_zstd_runtime: "cw_convert_zstd_no_runtime",
  convert_not_packable: "cw_convert_err_not_packable",
  convert_dest_read_only: "cw_convert_err_dest_read_only",
  convert_too_big_for_drive: "cw_convert_err_too_big_for_drive",
  convert_drive_gone: "cw_convert_err_drive_gone",
  convert_build_failed: "cw_convert_err_build_failed",
  convert_source_unreadable: "cw_convert_err_unreadable",
  convert_unsupported_source: "cw_convert_err_unsupported",
  convert_open_failed: "cw_files_err_read",
  convert_path_not_allowed: "cw_files_err_denied",
  convert_bad_dest: "cw_convert_err_bad_dest",
  convert_destination_exists: "cw_files_err_exists",
  convert_cancelled: "cw_convert_stopped",
  convert_failed: "cw_convert_err_failed",
  convert_write_failed: "cw_convert_err_failed",
  convert_bad_status: "cw_convert_err_failed",
  // A request the helper or the converter did not take: an older or newer page than the helper.
  convert_missing_path: "cw_convert_err_bad_request",
  convert_missing_dest: "cw_convert_err_bad_request",
  convert_missing: "cw_convert_err_bad_request",
  convert_bad_action: "cw_convert_err_bad_request",
  convert_bad_format: "cw_convert_err_bad_request",
  convert_bad_verify: "cw_convert_err_bad_request",
  convert_bad_zstd_level: "cw_convert_err_bad_request",
  convert_bad_request: "cw_convert_err_bad_request",
  bad_format: "cw_convert_err_bad_request",
  bad_verify: "cw_convert_err_bad_request",
  bad_zstd: "cw_convert_err_bad_request",
};

/** A convert cause token (up to its first ":" or space) as a sentence, or null when it is not one. */
export function convertCauseText(code: string): string | null {
  const token = code.replace(/[:\s].*$/, "");
  const key = CONVERT_CAUSES[token];
  return key ? t(key) : null;
}

/** Why a convert route refused or a job failed, as a sentence (null for a 401: the reload banner says it). */
export function convertErrorText(err: unknown): string | null {
  if (err instanceof ApiError) {
    const why = convertCauseText(err.code);
    if (why) return why;
    if (err.status === 403) return t("cw_files_err_denied");
    if (err.status === 404) return t("cw_files_err_missing");
  }
  return errorText(err);
}

/** A failed job's cause as a sentence: its own words, else the token itself in the generic line. */
export function convertFailureText(cause: string): string {
  return convertCauseText(cause) ?? t("cw_home_err_failed", { code: cause || "?" });
}

/** A running job's message (convert_starting, convert_stage_<stage>) as a line, or null. */
export function stageText(message: string | null | undefined): string | null {
  const m = message ?? "";
  if (m === "convert_starting") return t("cw_convert_stage_starting");
  const stage = /^convert_stage_([a-z_]+)$/.exec(m)?.[1];
  if (!stage) return null;
  switch (stage) {
    case "plan":
      return t("cw_convert_stage_plan");
    case "pack":
      return t("cw_convert_stage_pack");
    case "read":
      return t("cw_convert_stage_read");
    case "write":
      return t("cw_convert_stage_write");
    case "verify":
      return t("cw_convert_stage_verify");
  }
  return t("cw_convert_stage_other");
}

/** What /api/convert/start is sent. The level goes only with zstd. */
export interface StartBody {
  path: string;
  dest: string;
  format: OutFormat;
  zstd: boolean;
  zstd_level?: number;
  verify: Verify;
}

export function startBody(o: { source: string; dest: string; format: OutFormat; zstd: boolean; level: ZstdLevel; verify: Verify }): StartBody {
  const body: StartBody = { path: o.source, dest: o.dest, format: o.format, zstd: o.zstd, verify: o.verify };
  if (o.zstd) body.zstd_level = o.level;
  return body;
}

/* ---- inspect ---- */

export interface JobDeps {
  start: typeof startOp;
  watch: typeof watchOp;
}
const realDeps: JobDeps = { start: startOp, watch: watchOp };

export type InspectEnd = { ok: true; info: Inspection } | { ok: false; error: string | null };

/** Read what `path` is (a job: the converter is started for it). */
export async function inspect(path: string, deps: JobDeps = realDeps): Promise<InspectEnd> {
  let end: OpEnd;
  try {
    const job = await deps.start("/api/convert/inspect", { path });
    end = await deps.watch(job);
  } catch (e) {
    return { ok: false, error: convertErrorText(e) };
  }
  if (end.end === "done") {
    const info = parseInspection(opResult<unknown>(end.status));
    return info ? { ok: true, info } : { ok: false, error: convertFailureText("convert_bad_status") };
  }
  if (end.end === "gone") return { ok: false, error: t("cw_convert_lost") };
  return { ok: false, error: convertFailureText(opCause(end.status)) };
}

/* ---- the conversion running ---- */

export interface ConvertRun {
  source: string;
  dest: string;
  /** null until the helper took it. */
  job: string | null;
  bytesDone: number;
  bytesTotal: number;
  /** The running job's line (convert_stage_write …). */
  message: string | null;
  stopping: boolean;
  /** Set once it ended; the run stays shown until the next one or dismiss. */
  end: { tone: "info" | "error"; text: string } | null;
}

let current: ConvertRun | null = null;
const listeners = new Set<(r: ConvertRun | null) => void>();

function set(next: ConvertRun | null): void {
  current = next;
  for (const fn of listeners) fn(current);
}
function patch(p: Partial<ConvertRun>): void {
  if (current) set({ ...current, ...p });
}

/** The conversion now (running or just ended), or null. */
export function currentConvert(): ConvertRun | null {
  return current;
}

/** The conversion, re-rendered on each change. */
export function useConvertRun(): ConvertRun | null {
  const [run, setRun] = useState(current);
  useEffect(() => {
    listeners.add(setRun);
    setRun(current);
    return () => {
      listeners.delete(setRun);
    };
  }, []);
  return run;
}

/** Whether a conversion is running (not ended). */
export function converting(run: ConvertRun | null = current): boolean {
  return run != null && run.end == null;
}

/** How a finished job reads on the page. */
export function endOf(end: OpEnd, dest: string): NonNullable<ConvertRun["end"]> {
  if (end.end === "done") {
    const r = opResult<{ path?: unknown }>(end.status);
    return { tone: "info", text: t("cw_convert_done", { path: typeof r?.path === "string" ? r.path : dest }) };
  }
  if (end.end === "cancelled") return { tone: "info", text: t("cw_convert_stopped") };
  if (end.end === "gone") return { tone: "info", text: t("cw_convert_lost") };
  const cause = opCause(end.status);
  if (cause === "convert_cancelled") return { tone: "info", text: t("cw_convert_stopped") };
  return { tone: "error", text: t("cw_convert_failed", { reason: convertFailureText(cause) }) };
}

/**
 * Start a conversion and follow it to its end. One at a time (the helper allows one too); a call
 * while one runs does nothing. A 401 drops the run (the reload banner takes over).
 */
export async function startConvert(body: StartBody, deps: JobDeps = realDeps): Promise<void> {
  if (converting()) return;
  set({ source: body.path, dest: body.dest, job: null, bytesDone: 0, bytesTotal: 0, message: null, stopping: false, end: null });
  try {
    const job = await deps.start("/api/convert/start", body);
    patch({ job });
    const end = await deps.watch(job, {
      stopped: () => current?.stopping === true,
      onStatus: (s: OpStatus) => {
        if (s.state === "running") patch({ bytesDone: s.bytes_done, bytesTotal: s.bytes_total, message: s.message });
      },
    });
    patch({ end: endOf(end, body.dest) });
  } catch (e) {
    const text = convertErrorText(e);
    if (text == null) set(null);
    else patch({ end: { tone: "error", text: t("cw_convert_failed", { reason: text }) } });
  }
}

/** Stop the conversion running: the helper asks the converter to stop, and removes what it wrote. */
export function stopConvert(): void {
  // Before the helper answered with the job too: the watch asks for the stop as it begins.
  if (converting()) patch({ stopping: true });
}

/** Forget a conversion that ended (its line goes). */
export function dismissConvert(): void {
  if (current && current.end) set(null);
}

/** Tests: start from nothing. */
export function resetConvertForTest(): void {
  set(null);
}
