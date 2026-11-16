/**
 * Packages and payloads on the console (payload/src/webui_pkg.c): a .pkg's
 * preview (/api/pkg/info, its icon from /api/pkg/icon), its install as one of
 * the helper's operation jobs (/api/pkg/install, read like any other through
 * /api/jobs/status), and loading a .elf/.bin into the console's payload loader
 * (/api/payload/load).
 */
import { formatBytes } from "../../lib/format";
import { ApiError, call, callBlob } from "../api";
import { t } from "../strings";
import { opCause, opResult, type OpEnd, type OpStatus } from "./opJob";

/** /api/pkg/info. */
export interface PkgInfo {
  title: string;
  title_id: string;
  content_id: string;
  version: string;
  category: string;
  kind: "game" | "patch" | "dlc" | "app" | "other";
  platform: "ps4" | "ps5" | "";
  /** The required system software ("7.50"), "" when the package does not say. */
  fw: string;
  size: number;
  /** A PS5 package signed for debug consoles (a fake package). */
  fake: boolean;
  /** icon0.png can be shown. */
  icon: boolean;
  /** The game is installed already (a full game only). */
  installed: boolean;
  /** The game runs from a disk image (ShadowMount+): installing over it is refused. */
  image_mounted: boolean;
  /** On a USB drive or extended storage: copied to internal storage first. */
  staged: boolean;
  /** Another install runs now. */
  installing: boolean;
}

/** "x.pkg" (any case). */
export function isPkg(name: string): boolean {
  return /\.pkg$/i.test(name);
}

/** "x.elf" / "x.bin" (any case): offered to the payload loader. */
export function isPayload(name: string): boolean {
  return /\.(elf|bin)$/i.test(name);
}

export async function pkgInfo(path: string): Promise<PkgInfo> {
  return call<PkgInfo>("/api/pkg/info", { path });
}

/** The package's icon as an object URL (the caller revokes it). */
export async function pkgIconUrl(path: string, signal?: AbortSignal): Promise<string> {
  const blob = await callBlob("/api/pkg/icon", { path }, signal);
  return URL.createObjectURL(blob);
}

export async function startInstall(path: string, replace: boolean): Promise<string> {
  const r = await call<{ job?: unknown }>("/api/pkg/install", replace ? { path, replace: true } : { path });
  if (!r || typeof r.job !== "string") throw new ApiError(500, "bad_reply");
  return r.job;
}

export async function loadPayload(path: string): Promise<number> {
  const r = await call<{ bytes?: unknown }>("/api/payload/load", { path });
  return typeof r?.bytes === "number" ? r.bytes : 0;
}

/** The kind as the page says it. */
export function kindText(kind: string): string {
  if (kind === "game") return t("cw_pkg_kind_game");
  if (kind === "patch") return t("cw_pkg_kind_patch");
  if (kind === "dlc") return t("cw_pkg_kind_dlc");
  if (kind === "app") return t("cw_pkg_kind_app");
  return t("cw_pkg_kind_other");
}

/** The install's phase (its job message while it runs) as a line, or null. */
export function installPhaseText(message: string | null | undefined): string | null {
  const m = message ?? "";
  if (m.startsWith("pkg_install_staging")) return t("cw_pkg_phase_staging");
  if (m.startsWith("pkg_install_starting")) return t("cw_pkg_phase_starting");
  if (m.startsWith("pkg_install_installing")) return t("cw_pkg_phase_installing");
  if (m.startsWith("pkg_install_finishing")) return t("cw_pkg_phase_finishing");
  return null;
}

/** Stop is offered only while the package is being copied: once Sony has it, an install cannot be stopped. */
export function installStoppable(message: string | null | undefined): boolean {
  const m = message ?? "";
  return m === "" || m.startsWith("pkg_install_staging");
}

/** "0x80B2116F" out of "pkg_install_refused: 0x80B2116F", or "". */
function refusalCode(message: string): string {
  const m = /0x[0-9a-f]{8}/i.exec(message);
  return m ? m[0].toUpperCase().replace("0X", "0x") : "";
}

/** An install's (or its start's) cause token as a sentence, or null when it has none. */
export function installCauseText(code: string, message = ""): string | null {
  if (code === "pkg_install_busy" || code === "pkg_install_daemon_busy") return t("cw_pkg_err_busy");
  // A conversion (or Convert reading an archive) holds the drive: not "another install".
  if (code === "convert_busy") return t("cw_convert_err_busy");
  if (code === "pkg_install_already_installed") return t("cw_pkg_err_installed");
  if (code === "pkg_install_image_mounted") return t("cw_pkg_err_image");
  if (code === "pkg_install_no_space") return t("cw_pkg_err_no_space");
  if (code === "pkg_install_stage_failed") return t("cw_pkg_err_stage");
  if (code === "loader_unreachable") return t("cw_pkg_err_loader");
  if (code === "pkg_install_daemon_not_up" || code === "pkg_install_daemon_lost" || code === "pkg_install_daemon_not_ready")
    return t("cw_pkg_err_daemon");
  if (code === "pkg_install_refused") return t("cw_pkg_err_refused", { code: refusalCode(message) || "?" });
  if (code === "pkg_install_stalled") return t("cw_pkg_err_stalled");
  if (code === "pkg_install_daemon_bad_path") return t("cw_pkg_err_where");
  if (/^pkg_(not_pkg|truncated|bad_header)$/.test(code)) return t("cw_pkg_err_not_pkg");
  if (code === "path_not_allowed") return t("cw_files_err_denied");
  return null;
}

/** Why /api/pkg/info or /api/pkg/install refused, as a sentence (null: the reload banner says it). */
export function pkgErrorText(err: unknown, fallback: (e: unknown) => string | null): string | null {
  if (err instanceof ApiError) {
    const why = installCauseText(err.code);
    if (why) return why;
  }
  return fallback(err);
}

export interface InstallLine {
  tone: "info" | "error";
  text: string;
}

/** The line a finished install leaves. `name` is the package's title (or file name). */
export function installMessage(end: OpEnd | { end: "error"; error: unknown }, name: string, fallback: (e: unknown) => string | null): InstallLine | null {
  if (end.end === "done") {
    const r = opResult<{ verdict?: unknown }>(end.status);
    return r?.verdict === "installed"
      ? { tone: "info", text: t("cw_pkg_done_installed", { name }) }
      : { tone: "info", text: t("cw_pkg_done_delivered", { name }) };
  }
  if (end.end === "cancelled") return { tone: "info", text: t("cw_pkg_stopped") };
  if (end.end === "gone") return { tone: "info", text: t("cw_files_op_gone") };
  if (end.end === "error") {
    const why = pkgErrorText(end.error, fallback);
    return why ? { tone: "error", text: t("cw_pkg_failed", { reason: why }) } : null;
  }
  const s: OpStatus = end.status;
  const cause = opCause(s);
  if (cause === "pkg_install_cancelled") return { tone: "info", text: t("cw_pkg_stopped") };
  const why = installCauseText(cause, s.message ?? "") ?? t("cw_pkg_failed_generic", { cause: s.message || cause || "?" });
  return { tone: "error", text: t("cw_pkg_failed", { reason: why }) };
}

/** Why a payload load was refused, as a sentence. */
export function payloadErrorText(err: unknown, fallback: (e: unknown) => string | null): string | null {
  if (err instanceof ApiError) {
    if (err.code === "payload_not_elf" || err.code === "payload_not_payload") return t("cw_payload_err_not_elf");
    if (err.code === "payload_too_large") return t("cw_payload_err_too_large");
    if (err.code === "payload_is_helper") return t("cw_payload_err_helper");
    if (err.code === "pkg_install_busy") return t("cw_payload_err_installing");
    if (err.code === "loader_unreachable") return t("cw_pkg_err_loader");
    if (err.code === "loader_send_failed") return t("cw_payload_err_send");
  }
  return fallback(err);
}

/** "Copying 40% · 1.2 GiB / 3.0 GiB": the install's progress line for the batch status. */
export function installBusyText(phase: string | null, done: number, total: number): string {
  const line = phase ?? t("cw_pkg_phase_starting");
  if (total <= 0) return line;
  const d = Math.min(done, total);
  const pct = Math.min(100, Math.floor((d / total) * 100));
  return `${line} · ${t("cw_home_percent", { n: pct })} · ${formatBytes(d)} / ${formatBytes(total)}`;
}
