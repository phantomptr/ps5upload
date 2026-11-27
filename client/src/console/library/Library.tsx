import { useCallback, useEffect, useMemo, useReducer, useRef, useState, type ReactNode } from "react";
import { Check, Copy, Download, FolderOpen, Gamepad2, HousePlus, Layers, RefreshCw, Search, Square, Wrench, X } from "lucide-react";
import { formatBytes } from "../../lib/format";
import { ApiError, call } from "../api";
import { errorText, isDrive, volumeLabel, type Message, type Volume } from "../home/Home";
import { installMessage, installPhaseText, installStoppable, kindText } from "../files/pkg";
import { watchOp } from "../files/opJob";
import { startFolderSize, type ListPage } from "../files/folderSize";
import { listShares, smbCallErrorText, watchSmbJob, type SmbJob } from "../files/smb";
import { loadFullImage, loadImage, useLazyImage } from "../games/covers";
import { assemble, loadInfo, loadSources } from "../games/model";
import { onNavAction } from "../nav";
import type { SectionContext } from "../sections";
import { t } from "../strings";
import { Modal } from "../ui/Modal";
import { compareVersions, filterGroups, gameSize, groupEntries, newestUpdate, type Format, type GameGroup, type Part, type Role } from "./group";
import {
  CONSOLE_SOURCE,
  LIB_DIR,
  STAGE_DIR,
  copyDest,
  displayName,
  entriesOf,
  readScan,
  stagePath,
  startScan,
  type Entry,
  type LibScan,
  type LibSource,
} from "./model";

/** A running scan, copy or install: what the page shows, and whether Stop can end it. */
interface Task {
  key: string;
  text: string;
  /** False once the console's installer has the package (or for a one-call task): no Stop. */
  stoppable: boolean;
}

/** What this console has installed, by title id (its version once the detail asked for it). */
export interface InstalledTitle {
  version: string | null;
}

/** A grid cover is drawn at this size: sharp on a 1080p TV card, still small in memory. */
export const CARD_PX = 320;

const iconPath = (icon: string) => `${LIB_DIR}/icons/${icon}`;
const loadIcon = (icon: string) => loadImage(`lib:${icon}@${CARD_PX}`, [iconPath(icon)], CARD_PX);
const noIcon = () => Promise.resolve(null);

/** An installed title's own icon: the cover of a copy the scan found none for (a disk image). */
const loadAppIcon = (titleId: string) => loadImage(`lib-app:${titleId}@${CARD_PX}`, [`/user/appmeta/${titleId}/icon0.png`], CARD_PX);
const appIconPath = (titleId: string) => `/user/appmeta/${titleId}/icon0.png`;

function CardCover({ icon, titleId }: { icon: string; titleId: string }) {
  const ref = useRef<HTMLSpanElement>(null);
  const url = useLazyImage(icon || titleId, ref, icon ? loadIcon : titleId ? loadAppIcon : noIcon);
  return (
    <span ref={ref} className="cw-gcover" aria-hidden>
      {url ? <img src={url} alt="" /> : <Gamepad2 />}
    </span>
  );
}

/** The detail's cover at full size (icon0.png is 512 px), its object URL dropped on close. */
function BigCover({ icon, titleId }: { icon: string; titleId: string }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    if (!icon && !titleId) return;
    let live = true;
    let made: string | null = null;
    void loadFullImage(icon ? iconPath(icon) : appIconPath(titleId)).then((u) => {
      made = u;
      if (live) setUrl(u);
      else if (u) URL.revokeObjectURL(u);
    });
    return () => {
      live = false;
      if (made) URL.revokeObjectURL(made);
    };
  }, [icon, titleId]);
  return <span className="cw-libview-cover" aria-hidden>{url ? <img src={url} alt="" /> : <Gamepad2 />}</span>;
}

export function formatText(f: Format): string {
  switch (f) {
    case "folder":
      return t("cw_lib_kind_folder");
    case "pkg":
      return t("cw_lib_fmt_pkg");
    case "fpkg":
      return t("cw_lib_fake");
    case "exfat":
      return t("cw_lib_fmt_exfat");
    case "ffpkg":
      return t("cw_lib_fmt_ffpkg");
    case "ffpfs":
      return t("cw_lib_fmt_ffpfs");
    default:
      return t("cw_lib_kind_image");
  }
}

export function roleText(r: Role): string {
  if (r === "base") return kindText("game");
  if (r === "update") return kindText("patch");
  if (r === "backport") return t("cw_lib_role_backport");
  return kindText(r);
}

const platformText = (p: string) => (p === "ps4" ? "PS4" : p === "ps5" ? "PS5" : "");

/** A failed call or job as one line. */
const failText = (e: unknown) => smbCallErrorText(e) ?? errorText(e) ?? "";

/** A folder's counted size: running (bytes so far) or done. */
export interface FolderSize {
  bytes: number;
  done: boolean;
}

export interface LibraryViewProps {
  /** Games' Installed / On Drives & Shares switch, under the title. */
  switcher?: ReactNode;
  sources: LibSource[];
  scans: ReadonlyMap<string, LibScan | null>;
  loaded: boolean;
  query: string;
  task: Task | null;
  message: Message | null;
  /** The game whose detail is open (by group key), or null. */
  open: string | null;
  /** Null while they are read; empty when they could not be (or none is writable). */
  drives: Volume[] | null;
  /** The folder a copy just put on the console, for Register. */
  copied: { key: string; path: string } | null;
  /** This console's titles; null until read (or when they could not be). */
  installed: ReadonlyMap<string, InstalledTitle> | null;
  /** The console's firmware ("13.60"), for "needs a newer system software". */
  firmware: string | null;
  /** Folder sizes counted so far, by entry key. */
  sizes: ReadonlyMap<string, FolderSize>;
  onQuery: (q: string) => void;
  onScan: () => void;
  onStop: () => void;
  onOpen: (g: GameGroup | null) => void;
  onInstall: (e: Entry) => void;
  onCopy: (e: Entry, drive: string) => void;
  onRegister: (path: string, name: string) => void;
  /** Show the folder holding `path` in Files (null: Files cannot be opened from here). */
  onShowInFiles: ((path: string) => void) | null;
}

/** The game's size with the folders counted so far; whether some are still uncounted. */
export function groupSize(g: GameGroup, sizes: ReadonlyMap<string, FolderSize>): { bytes: number; partial: boolean } {
  return gameSize(g.parts, sizes);
}

const parentOf = (path: string) => {
  const i = path.lastIndexOf("/");
  return i > 0 ? path.slice(0, i) : path;
};

function PartRow({ g, part, first, p, onCopyPick }: { g: GameGroup; part: Part; first: boolean; p: LibraryViewProps; onCopyPick: (e: Entry) => void }) {
  const e = part.entry;
  const busy = p.task !== null;
  const onShare = e.source !== CONSOLE_SOURCE;
  const registerPath = !onShare && e.kind === "folder" ? e.path : p.copied?.key === e.key ? p.copied.path : null;
  const counted = p.sizes.get(e.key);
  const size = e.size > 0 ? formatBytes(e.size) : counted ? (counted.done ? formatBytes(counted.bytes) : t("cw_lib_size_counting", { size: formatBytes(counted.bytes) })) : "—";
  const meta = [e.version ? `v${e.version}` : "", size, e.sourceName].filter(Boolean).join(" · ");
  let n = 0;
  const def = () => (first && n++ === 0 ? "" : undefined);
  return (
    <li className="cw-libpart">
      <div className="cw-libpart-text">
        <span className="cw-libpart-tags">
          <span className={`cw-tag${part.role === "base" ? " is-on" : ""}`}>{roleText(part.role)}</span>
          <span className="cw-tag">{formatText(part.format)}</span>
          {part.role === "dlc" && e.title && e.title !== g.title ? <span className="cw-libpart-name">{e.title}</span> : null}
        </span>
        <span className="cw-libpart-meta">{meta}</span>
        <span className="cw-libpart-path cw-mono cw-dim">{e.path}</span>
      </div>
      <span className="cw-libpart-actions">
        {e.kind === "pkg" ? (
          <button type="button" className="cw-focusable cw-button is-primary" data-nav-default={def()} disabled={busy} onClick={() => p.onInstall(e)}>
            <Download aria-hidden />
            <span>{t("cw_lib_install")}</span>
          </button>
        ) : null}
        {onShare && e.kind !== "pkg" ? (
          <button type="button" className="cw-focusable cw-button is-primary" data-nav-default={def()} disabled={busy} onClick={() => onCopyPick(e)}>
            <Copy aria-hidden />
            <span>{t("cw_lib_copy_here")}</span>
          </button>
        ) : null}
        {registerPath ? (
          <button type="button" className="cw-focusable cw-button" data-nav-default={def()} disabled={busy} onClick={() => p.onRegister(registerPath, displayName(e))}>
            <HousePlus aria-hidden />
            <span>{t("cw_lib_register")}</span>
          </button>
        ) : null}
        {p.onShowInFiles ? (
          <button type="button" className="cw-focusable cw-button" data-nav-default={def()} onClick={() => p.onShowInFiles?.(parentOf(e.at))}>
            <FolderOpen aria-hidden />
            <span>{t("cw_cleanup_show_in_files")}</span>
          </button>
        ) : null}
      </span>
    </li>
  );
}

/** Which drive a copy goes to. */
export function DrivePicker({ e, p, onClose }: { e: Entry; p: LibraryViewProps; onClose: () => void }) {
  return (
    <Modal label={t("cw_lib_copy_where")} className="cw-sheet" onBack={onClose}>
      <h2 className="cw-sheet-title">{t("cw_lib_copy_where")}</h2>
      <p className="cw-dim">{displayName(e)}</p>
      <div className="cw-sheet-items">
        {p.drives?.length ? (
          p.drives.map((d, i) => (
            <button
              key={d.path}
              type="button"
              className="cw-focusable cw-sheet-item"
              data-nav-default={i === 0 ? "" : undefined}
              onClick={() => {
                onClose();
                p.onCopy(e, d.path);
              }}
            >
              <Copy aria-hidden />
              <span>{t("cw_lib_copy_to", { drive: `${volumeLabel(d)} (${formatBytes(d.free_bytes)})` })}</span>
            </button>
          ))
        ) : (
          <p className={p.drives ? "cw-field-error" : "cw-dim"}>{t(p.drives ? "cw_home_err_load_storage" : "cw_games_loading")}</p>
        )}
      </div>
      <div className="cw-dialog-buttons">
        <button type="button" className="cw-focusable cw-button" onClick={onClose}>
          <X aria-hidden />
          <span>{t("cw_dialog_cancel")}</span>
        </button>
      </div>
    </Modal>
  );
}

/** The game's full-screen detail: its cover, everything known about it, and each part with what can be done with it. */
function Detail({ g, p }: { g: GameGroup; p: LibraryViewProps }) {
  const [pick, setPick] = useState<Entry | null>(null);
  const close = useCallback(() => p.onOpen(null), [p]);
  const inst = g.titleId ? p.installed?.get(g.titleId) : undefined;
  const size = groupSize(g, p.sizes);
  // a PS4 title names a PS4 firmware: nothing to hold against this PS5's
  const ps5fw = g.platform !== "ps4" && p.firmware ? p.firmware : null;
  const newer = !!(g.fw && ps5fw && compareVersions(g.fw, ps5fw.replace(/[^0-9.]/g, "")) > 0);
  const rows: [string, string][] = [];
  if (g.titleId) rows.push([t("cw_games_info_title_id"), g.titleId]);
  rows.push([t("cw_games_info_size"), size.partial ? t("cw_lib_size_counting", { size: formatBytes(size.bytes) }) : formatBytes(size.bytes)]);
  if (g.fw) rows.push([t("cw_lib_info_fw"), ps5fw ? `${g.fw} · ${t("cw_lib_this_fw", { fw: ps5fw })}` : g.fw]);
  if (g.sdk) rows.push([t("cw_lib_info_sdk"), g.sdk]);
  rows.push([t("cw_lib_info_formats"), g.formats.map(formatText).join(", ")]);
  rows.push([t("cw_lib_info_source"), g.places.join(", ")]);
  let state: string;
  if (!p.installed || !g.titleId) state = "";
  else if (inst) state = inst.version ? t("cw_lib_installed_v", { version: inst.version }) : t("cw_lib_installed");
  else state = t("cw_lib_not_installed");
  return (
    <Modal label={g.title} className="cw-libview" onBack={close}>
      <button type="button" className="cw-focusable cw-button cw-libview-close" onClick={close}>
        <X aria-hidden />
        <span>{t("cw_files_close")}</span>
      </button>
      <div className="cw-libview-head">
        <BigCover icon={g.cover?.icon ?? ""} titleId={inst ? g.titleId : ""} />
        <div className="cw-libview-text">
          <h2 className="cw-libview-title">{g.title}</h2>
          <span className="cw-libview-tags">
            {platformText(g.platform) ? <span className="cw-tag">{platformText(g.platform)}</span> : null}
            {state ? <span className={`cw-tag${inst ? " is-on" : ""}`}>{inst ? <Check aria-hidden /> : null}{state}</span> : null}
            {g.updates ? <span className="cw-tag">{t("cw_lib_badge_update", { version: newestUpdate(g) })}</span> : null}
            {g.dlc ? <span className="cw-tag">{t("cw_lib_badge_dlc", { n: g.dlc })}</span> : null}
            {g.backports ? <span className="cw-tag">{t("cw_lib_role_backport")}</span> : null}
          </span>
          <dl className="cw-info cw-libview-info">
            {rows.map(([k, v]) => (
              <div key={k}>
                <dt className="cw-dim">{k}</dt>
                <dd>{v}</dd>
              </div>
            ))}
          </dl>
          {g.titleUnread ? (
            <p className="cw-msg cw-dim">{t(g.titleUnread === "img_compressed" ? "cw_lib_unread_compressed" : "cw_lib_unread_other")}</p>
          ) : null}
          {newer ? (
            <p className="cw-msg is-error">{t("cw_lib_fw_newer", { need: g.fw, have: p.firmware ?? "" })}</p>
          ) : null}
          {p.task ? (
            <p className="cw-msg" role="status">
              {p.task.text}
              {p.task.stoppable ? (
                <button type="button" className="cw-focusable cw-button cw-msg-action is-danger" onClick={p.onStop}>
                  <Square aria-hidden />
                  <span>{t("cw_lib_stop")}</span>
                </button>
              ) : null}
            </p>
          ) : null}
          {p.message ? (
            <div className={`cw-msg ${p.message.tone === "error" ? "is-error" : "is-info"}`} role={p.message.tone === "error" ? "alert" : "status"}>
              {p.message.text}
            </div>
          ) : null}
        </div>
      </div>
      <h3 className="cw-libview-h3">
        <Layers aria-hidden />
        <span>{t("cw_lib_parts", { n: g.parts.length })}</span>
      </h3>
      <ul className="cw-libparts">
        {g.parts.map((part, i) => (
          <PartRow key={part.entry.key} g={g} part={part} first={i === 0} p={p} onCopyPick={setPick} />
        ))}
      </ul>
      {pick ? <DrivePicker e={pick} p={p} onClose={() => setPick(null)} /> : null}
    </Modal>
  );
}

function GameCard({ g, p }: { g: GameGroup; p: LibraryViewProps }) {
  const inst = g.titleId ? p.installed?.has(g.titleId) : false;
  const size = groupSize(g, p.sizes);
  const meta = [platformText(g.platform), size.bytes > 0 ? formatBytes(size.bytes) : "", g.formats.map(formatText)[0] ?? ""].filter(Boolean).join(" · ");
  return (
    <li className="cw-gcard-item">
      <button type="button" className="cw-focusable cw-gcard cw-libcard" data-lib-key={g.key} onClick={() => p.onOpen(g)}>
        <CardCover icon={g.cover?.icon ?? ""} titleId={inst ? g.titleId : ""} />
        {inst ? (
          <span className="cw-libcard-installed" title={t("cw_lib_installed")}>
            <Check aria-hidden />
          </span>
        ) : null}
        <span className="cw-libcard-name">{g.title}</span>
        <span className="cw-libcard-meta cw-dim">{meta}</span>
        <span className="cw-gcard-tags">
          {g.titleUnread ? <span className="cw-tag">{t("cw_lib_title_unread")}</span> : null}
          {g.updates ? <span className="cw-tag">{kindText("patch")}</span> : null}
          {g.dlc ? <span className="cw-tag">{t("cw_lib_badge_dlc", { n: g.dlc })}</span> : null}
          {g.backports ? (
            <span className="cw-tag">
              <Wrench aria-hidden />
              {t("cw_lib_role_backport")}
            </span>
          ) : null}
          {g.parts.length > 1 && !g.updates && !g.dlc && !g.backports ? <span className="cw-tag">{t("cw_lib_badge_copies", { n: g.parts.length })}</span> : null}
        </span>
      </button>
    </li>
  );
}

/** The Library without state: everything comes in through props (tests render it directly). */
export function LibraryView(p: LibraryViewProps) {
  const all = useMemo(() => groupEntries(entriesOf(p.sources, p.scans)), [p.sources, p.scans]);
  const shown = useMemo(() => filterGroups(all, p.query), [all, p.query]);
  const open = p.open ? all.find((g) => g.key === p.open) ?? null : null;
  const notes: string[] = [];
  for (const s of p.sources) {
    const scan = p.scans.get(s.id);
    if (scan && !scan.complete) notes.push(t("cw_lib_partial", { name: s.name }));
  }
  const never = p.loaded && p.sources.every((s) => !p.scans.get(s.id));
  let body;
  if (!p.loaded) body = <p className="cw-dim">{t("cw_games_loading")}</p>;
  else if (never) body = <p className="cw-dim">{t("cw_lib_never")}</p>;
  else if (!shown.length)
    body = <p className="cw-dim">{p.query.trim() ? t("cw_games_no_match", { query: p.query.trim() }) : t("cw_lib_none")}</p>;
  else
    body = (
      <ul className="cw-games cw-libgrid" aria-label={t("cw_lib_title")}>
        {shown.map((g) => (
          <GameCard key={g.key} g={g} p={p} />
        ))}
      </ul>
    );
  return (
    <div className="cw-games-section">
      <h1 className="cw-h1">
        {t("cw_lib_title")}
        {p.loaded ? <span className="cw-tag">{all.length}</span> : null}
      </h1>
      {p.switcher}
      <div className="cw-toolbar">
        <label className="cw-games-search">
          <Search aria-hidden />
          <input
            className="cw-focusable cw-input"
            type="text"
            value={p.query}
            placeholder={t("cw_games_search")}
            aria-label={t("cw_games_search")}
            autoComplete="off"
            spellCheck={false}
            onChange={(ev) => p.onQuery(ev.target.value)}
          />
        </label>
        {p.task?.stoppable && !open ? (
          <button type="button" className="cw-focusable cw-button" data-action="lib-stop" onClick={p.onStop}>
            <Square aria-hidden />
            <span>{t("cw_lib_stop")}</span>
          </button>
        ) : (
          <button type="button" className="cw-focusable cw-button" data-action="lib-scan" disabled={!!p.task} onClick={p.onScan}>
            <RefreshCw aria-hidden />
            <span>{t("cw_lib_scan")}</span>
          </button>
        )}
      </div>
      <p className="cw-dim">{p.sources.length > 1 ? t("cw_lib_sources", { n: p.sources.length - 1 }) : t("cw_lib_no_shares")}</p>
      {p.task && !open ? <p className="cw-dim" role="status">{p.task.text}</p> : null}
      {notes.map((n) => (
        <p key={n} className="cw-dim">
          {n}
        </p>
      ))}
      {p.message && !open ? (
        <div className={`cw-msg ${p.message.tone === "error" ? "is-error" : "is-info"}`} role={p.message.tone === "error" ? "alert" : "status"}>
          {p.message.text}
        </div>
      ) : null}
      {body}
      {open ? <Detail g={open} p={p} /> : null}
    </div>
  );
}

/* ---- state ---- */

const pct = (done: number, total: number) => (total > 0 ? Math.min(100, Math.floor((done / total) * 100)) : 0);

const consoleSource = (): LibSource => ({ id: CONSOLE_SOURCE, name: t("cw_lib_this_console") });

/**
 * The section's state lives outside the component: a scan, copy or install
 * started here goes on in the helper when the section is left (like an install
 * once the console has it), and the section shows it again, with its outcome,
 * when it opens next. Stop asks the helper to cancel the running job.
 */
interface LibState {
  sources: LibSource[];
  scans: Map<string, LibScan | null>;
  loaded: boolean;
  task: Task | null;
  message: Message | null;
  copied: { key: string; path: string } | null;
  installed: Map<string, InstalledTitle> | null;
  sizes: Map<string, FolderSize>;
}

const lib: LibState = { sources: [], scans: new Map(), loaded: false, task: null, message: null, copied: null, installed: null, sizes: new Map() };
const listeners = new Set<() => void>();
let stopAsked = false;

function patch(p: Partial<LibState>): void {
  Object.assign(lib, p);
  for (const fn of Array.from(listeners)) fn();
}

const stopped = () => stopAsked;

/** This console's titles (no details: one detail is read when its game opens). Never throws. */
async function loadInstalledIds(): Promise<void> {
  try {
    const { sources } = await loadSources(call);
    const next = new Map<string, InstalledTitle>();
    for (const g of assemble(sources)) next.set(g.titleId, { version: lib.installed?.get(g.titleId)?.version ?? null });
    patch({ installed: next });
  } catch {
    // unknown: the cards and the detail say nothing about it
  }
}

/** The saved shares, then each source's cached scan; a cache that cannot be read is said so. */
async function load(): Promise<void> {
  let list: LibSource[] = [consoleSource()];
  try {
    list = list.concat((await listShares()).map((s) => ({ id: s.id, name: s.name })));
  } catch {
    // No shares readable: this console only.
  }
  const next = new Map<string, LibScan | null>();
  const failed: string[] = [];
  for (const s of list) {
    try {
      next.set(s.id, await readScan(s.id));
    } catch (e) {
      next.set(s.id, lib.scans.get(s.id) ?? null);
      failed.push(t("cw_lib_read_failed", { name: s.name, why: failText(e) || (e instanceof ApiError ? e.code : String(e)) }));
    }
  }
  patch({ sources: list, scans: next, loaded: true, ...(failed.length ? { message: { tone: "error", text: failed.join(" · ") } } : {}) });
  void loadInstalledIds();
}

async function scanAll(): Promise<void> {
  if (lib.task) return;
  stopAsked = false;
  // busy before the first await, so a second press cannot start a second run
  patch({ message: null, task: { key: "scan", text: t("cw_lib_scanning", { name: consoleSource().name, n: 0 }), stoppable: true } });
  const failed: string[] = [];
  let list = lib.sources;
  try {
    // the shares as they are now (one may have been added in Files or from the PC since)
    list = [consoleSource(), ...(await listShares()).map((s) => ({ id: s.id, name: s.name }))];
    patch({ sources: list });
  } catch {
    // keep the list we have
  }
  for (const s of list) {
    if (stopped()) break;
    patch({ task: { key: "scan", text: t("cw_lib_scanning", { name: s.name, n: 0 }), stoppable: true } });
    try {
      const job = await startScan(s.id);
      const end = await watchSmbJob(job, {
        stopped,
        onStatus: (st: SmbJob) => patch({ task: { key: "scan", text: t("cw_lib_scanning", { name: s.name, n: st.files_total }), stoppable: true } }),
      });
      if (end.end === "failed") failed.push(`${s.name}: ${failText(new ApiError(502, end.status.error ?? "io_error"))}`);
      else if (end.end === "gone") failed.push(`${s.name}: ${failText(new ApiError(502, "smb_job_lost"))}`);
      const scan = await readScan(s.id);
      // loaded: a scan started before the section's first load (Home's USB card) is that load
      patch({ scans: new Map(lib.scans).set(s.id, scan), loaded: true });
    } catch (e) {
      failed.push(`${s.name}: ${failText(e)}`);
    }
  }
  // a new scan: folder sizes are counted again when asked
  patch({ task: null, loaded: true, sizes: new Map(), ...(failed.length ? { message: { tone: "error", text: t("cw_lib_scan_failed", { list: failed.join(" · ") }) } } : {}) });
  void loadInstalledIds();
}

/** Copies `src` (on a share) to `dest`; true when it finished. */
async function copyIn(src: string, dest: string, name: string): Promise<boolean> {
  const { job } = await call<{ job: string }>("/api/smb/copy", { src, dest });
  const end = await watchSmbJob(job, {
    stopped,
    onStatus: (st) =>
      patch({
        task: { key: "copy", text: t("cw_lib_copying", { name, pct: pct(st.bytes_done, st.bytes_total), size: formatBytes(st.bytes_total) }), stoppable: true },
      }),
  });
  if (end.end === "done") return true;
  if (end.end === "cancelled") throw new ApiError(409, "cancelled");
  if (end.end === "gone") throw new ApiError(502, "smb_job_lost");
  throw new ApiError(502, end.status.error ?? "io_error");
}

async function install(e: Entry): Promise<void> {
  if (lib.task) return;
  stopAsked = false;
  const name = displayName(e);
  patch({ message: null, task: { key: "install", text: t("cw_pkg_phase_starting"), stoppable: true } });
  let path = e.at;
  let staged = false;
  try {
    if (e.source !== CONSOLE_SOURCE) {
      await call("/api/fs/mkdir", { path: STAGE_DIR });
      path = stagePath();
      staged = true;
      await copyIn(e.at, path, name);
    }
    patch({ task: { key: "install", text: t("cw_pkg_phase_starting"), stoppable: true } });
    let job: string;
    try {
      const r = await call<{ job: string }>("/api/pkg/install", staged ? { path, remove_after: true } : { path });
      job = r.job;
    } catch (err) {
      // refused before it started: the copy is ours to remove
      if (staged) await call("/api/fs/delete", { path }).catch(() => undefined);
      throw err;
    }
    // Stop counts only while the package is being copied: once the console's installer has it, it cannot be stopped.
    let phase: string | null = null;
    const end = await watchOp(job, {
      stopped: () => stopped() && installStoppable(phase),
      onStatus: (s) => {
        phase = s.message;
        patch({
          task: {
            key: "install",
            text: `${installPhaseText(s.message) ?? t("cw_pkg_phase_starting")} · ${pct(s.bytes_done, s.bytes_total)}%`,
            stoppable: installStoppable(s.message),
          },
        });
      },
    });
    const line = installMessage(end, name, errorText);
    patch({ task: null, message: line ?? lib.message });
    void loadInstalledIds();
  } catch (err) {
    if (err instanceof ApiError && err.code === "cancelled") patch({ task: null, message: { tone: "info", text: t("cw_pkg_stopped") } });
    else patch({ task: null, message: installMessage({ end: "error", error: err }, name, failText) ?? lib.message });
  }
}

async function copy(e: Entry, drive: string): Promise<void> {
  if (lib.task) return;
  stopAsked = false;
  const name = displayName(e);
  patch({ message: null, task: { key: "copy", text: t("cw_lib_copying", { name, pct: 0, size: formatBytes(e.size) }), stoppable: true } });
  const dest = copyDest(drive, e);
  try {
    await call("/api/fs/mkdir", { path: dest.slice(0, dest.lastIndexOf("/")) });
    await copyIn(e.at, dest, name);
    patch({
      task: null,
      ...(e.kind === "folder" ? { copied: { key: e.key, path: dest } } : {}),
      message: { tone: "info", text: t(e.kind === "folder" ? "cw_lib_copied_folder" : "cw_lib_copied", { path: dest }) },
    });
  } catch (err) {
    let message: Message;
    if (err instanceof ApiError && err.code === "cancelled") message = { tone: "info", text: t("cw_lib_copy_stopped") };
    else if (err instanceof ApiError && err.code === "exists") message = { tone: "error", text: t("cw_lib_err_exists", { path: dest }) };
    else message = { tone: "error", text: failText(err) };
    patch({ task: null, message });
  }
}

async function register(path: string, name: string): Promise<void> {
  if (lib.task) return;
  patch({ task: { key: "register", text: t("cw_lib_registering", { name }), stoppable: false } });
  try {
    await call("/api/apps/register", { path });
    patch({ task: null, message: { tone: "info", text: t("cw_lib_registered", { name }) } });
    void loadInstalledIds();
  } catch (err) {
    patch({ task: null, message: { tone: "error", text: failText(err) || (err instanceof ApiError ? err.code : String(err)) } });
  }
}

const listAny = (path: string, offset: number, limit: number) =>
  call<ListPage>(path.startsWith("smb://") ? "/api/smb/list" : "/api/fs/list", { path, offset, limit });

/**
 * Counts the size of each folder of `g` that the scan left at 0 (a folder's size is not read
 * during a scan: that would walk every game), one after the other with the page's one folder walk.
 * Returns the function that stops it (the detail closed).
 */
function countFolders(g: GameGroup): () => void {
  let cancel: (() => void) | null = null;
  let over = false;
  const todo = g.parts.filter((p) => p.entry.kind === "folder" && p.entry.size <= 0 && !lib.sizes.get(p.entry.key)?.done);
  void (async () => {
    for (const part of todo) {
      if (over) return;
      const key = part.entry.key;
      const w = startFolderSize(part.entry.at, (c) => patch({ sizes: new Map(lib.sizes).set(key, { bytes: c.bytes, done: false }) }), {
        list: listAny,
        now: () => Date.now(),
      });
      if (!w) return; // another walk (Files' Info, Disk usage) holds it: sizes stay unknown
      cancel = w.cancel;
      const r = await w.done;
      cancel = null;
      if (r.end === "done") patch({ sizes: new Map(lib.sizes).set(key, { bytes: r.bytes, done: true }) });
      else {
        const s = new Map(lib.sizes);
        s.delete(key);
        patch({ sizes: s });
      }
    }
  })();
  return () => {
    over = true;
    cancel?.();
  };
}

/** The installed version of `titleId`, read once when its game opens. */
async function loadInstalledVersion(titleId: string): Promise<void> {
  if (!lib.installed?.has(titleId) || lib.installed.get(titleId)?.version) return;
  const info = await loadInfo(call, [titleId]);
  const v = info.get(titleId)?.version ?? null;
  if (v && lib.installed?.has(titleId)) patch({ installed: new Map(lib.installed).set(titleId, { version: v }) });
}

/** The Library section: one card per game on the saved shares and this console's drives; its detail installs or copies each part. */
export function Library({
  scan = false,
  ctx,
  switcher,
}: { scan?: boolean; ctx?: SectionContext; switcher?: ReactNode } = {}) {
  const [, bump] = useReducer((n: number) => n + 1, 0);
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState<string | null>(null);
  const [drives, setDrives] = useState<Volume[] | null>(null);
  const live = useRef(true);
  const root = useRef<HTMLDivElement>(null);
  const stopCount = useRef<(() => void) | null>(null);

  // Opened to scan (a USB drive just connected): start one, unless one is running already.
  useEffect(() => {
    if (scan && !lib.task) void scanAll();
  }, [scan]);

  useEffect(() => {
    live.current = true;
    listeners.add(bump);
    // A scan running keeps the store current itself; otherwise read the cache again (a share may have been added).
    if (lib.task?.key !== "scan") void load();
    return () => {
      live.current = false;
      listeners.delete(bump);
      stopCount.current?.();
    };
  }, []);

  useEffect(
    () =>
      onNavAction(
        (a) => {
          if (a === "refresh" && lib.task?.key !== "scan") void load();
          return false;
        },
        () => root.current,
      ),
    [],
  );

  const openGame = useCallback((g: GameGroup | null) => {
    stopCount.current?.();
    stopCount.current = null;
    setOpen(g ? g.key : null);
    if (!g) return;
    stopCount.current = countFolders(g);
    if (g.titleId) void loadInstalledVersion(g.titleId);
    if (g.parts.some((p) => p.entry.source !== CONSOLE_SOURCE && p.entry.kind !== "pkg")) {
      setDrives(null);
      void call<{ volumes?: unknown }>("/api/volumes").then(
        (r) => live.current && setDrives((Array.isArray(r?.volumes) ? (r.volumes as Volume[]) : []).filter((v) => isDrive(v) && v.writable)),
        () => live.current && setDrives([]),
      );
    }
  }, []);

  const showInFiles = ctx?.canOpen("files") ? (path: string) => ctx.openSection("files", { path }) : null;

  return (
    <div ref={root}>
      <LibraryView
        switcher={switcher}
        sources={lib.sources}
        scans={lib.scans}
        loaded={lib.loaded}
        query={query}
        task={lib.task}
        message={lib.message}
        open={open}
        drives={drives}
        copied={lib.copied}
        installed={lib.installed}
        firmware={ctx?.status?.firmware ?? null}
        sizes={lib.sizes}
        onQuery={setQuery}
        onScan={() => void scanAll()}
        onStop={() => {
          stopAsked = true;
        }}
        onOpen={openGame}
        onInstall={(e) => void install(e)}
        onCopy={(e, d) => void copy(e, d)}
        onRegister={(path, name) => void register(path, name)}
        onShowInFiles={showInFiles}
      />
    </div>
  );
}
