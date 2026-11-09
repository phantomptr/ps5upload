import { useCallback, useEffect, useRef, useState } from "react";
import { ArchiveRestore, Eraser, Gamepad2, HardDrive, Sparkles, Usb, X } from "lucide-react";
import { formatBytes } from "../../lib/format";
import { ApiError, call, callBlob, isOffline } from "../api";
import { cleanupLastFound, onCleanupFound } from "../cleanup/Cleanup";
import { onNavAction } from "../nav";
import type { SectionContext } from "../sections";
import type { ConsoleStatus } from "../shell/TopBar";
import { t } from "../strings";
import { ConfirmDialog } from "../ui/ConfirmDialog";
import { pollWhileVisible } from "../ui/visiblePoll";
import { TaskRow } from "./TaskRow";
import { HomeDeviceAsks } from "../settings/Devices";
import { useJobs, visibleJobs, type Job } from "./useJobs";

/** Free space moves with every write: re-read this often while Home is open. */
const VOLUMES_POLL_MS = 15_000;
/** An action's result line clears itself after this long. */
const MESSAGE_MS = 8_000;
/** Home shows this many task rows, then "+N more": the running game and the drives stay in view. */
export const MAX_TASK_ROWS = 2;

/** One /api/volumes entry (payload/src/runtime.c handle_fs_list_volumes). */
export interface Volume {
  path: string;
  mount_from: string;
  fs_type: string;
  total_bytes: number;
  free_bytes: number;
  writable: boolean;
  source_image?: string;
}

/** Something Home asks about before doing it. */
export type Ask =
  | { kind: "cancel"; job: Job }
  | { kind: "close"; appId: number; titleId: string };

export interface Message {
  tone: "error" | "info";
  text: string;
}

/* ---- volumes ---- */

const isUsbStick = (v: Volume) => /^\/mnt\/usb\d+$/.test(v.path);
const isExt = (v: Volume) => /^\/mnt\/ext\d+$/.test(v.path);
/** A USB stick that was not there on the previous read (`seen`, null on the first: what is
 *  plugged in when the page opens is not news). */
export function newUsbStick(seen: ReadonlySet<string> | null, volumes: readonly Volume[]): Volume | null {
  if (!seen) return null;
  return volumes.find((v) => isUsbStick(v) && !seen.has(v.path)) ?? null;
}

/** A USB drive formatted as PS5 extended storage (M.2 is /dev/nvme*). */
const isUsbExt = (v: Volume) => isExt(v) && v.mount_from.startsWith("/dev/da");
/** A USB drive files can be written to: a stick, or one formatted as extended storage. */
export const isWritableUsb = (v: Volume) => v.writable && (isUsbStick(v) || isUsbExt(v));

/** The console's drives: internal, extended/M.2 and USB. Mounted disk images are left out. */
export function isDrive(v: Volume): boolean {
  return v.path === "/data" || isExt(v) || isUsbStick(v);
}

export function volumeLabel(v: Volume): string {
  if (v.path === "/data") return t("cw_home_vol_internal");
  if (isUsbStick(v)) return t("cw_home_vol_usb");
  if (isUsbExt(v)) return t("cw_home_vol_usb_ext");
  if (v.mount_from.startsWith("/dev/nvme")) return t("cw_home_vol_m2");
  return t("cw_home_vol_ext");
}

/* ---- actions ---- */

/** A failed call as one short translated line; null for a 401 (the reload banner says it). */
export function errorText(err: unknown): string | null {
  if (err instanceof ApiError) {
    if (err.status === 401) return null;
    if (isOffline(err)) return t("cw_home_err_offline");
    if (err.code === "busy") return t("cw_home_err_busy");
    if (err.code === "unknown_job") return t("cw_home_err_gone");
    return t("cw_home_err_failed", { code: err.code });
  }
  return t("cw_home_err_failed", {
    code: err instanceof Error ? err.message : String(err),
  });
}

/** A failure the dialog keeps open for (try again in a moment): 409 (busy) or 503 (stopping). */
export function keepsDialogOpen(err: unknown): boolean {
  return err instanceof ApiError && (err.status === 409 || err.status === 503);
}

/** Do what was confirmed. Throws the ApiError. */
export async function runAsk(ask: Ask, deps: { cancel: (id: string) => Promise<void> }): Promise<void> {
  switch (ask.kind) {
    case "cancel":
      await deps.cancel(ask.job.job);
      return;
    case "close":
      await call("/api/app/close", { app_id: ask.appId });
      return;
  }
}

function askCopy(ask: Ask): {
  title: string;
  body: string;
  confirm: string;
  cancel?: string;
  danger: boolean;
} {
  switch (ask.kind) {
    case "cancel":
      return {
        title: t("cw_home_confirm_cancel_title"),
        body: t(ask.job.mine ? "cw_home_confirm_cancel_body" : "cw_home_confirm_cancel_body_pc"),
        confirm: t("cw_home_cancel_task_confirm"),
        cancel: t("cw_home_keep_task"),
        danger: true,
      };
    case "close":
      return {
        title: t("cw_home_confirm_close_title", { name: ask.titleId }),
        body: t("cw_home_confirm_close_body"),
        confirm: t("cw_home_close_game"),
        danger: true,
      };
  }
}

/* ---- view ---- */

export interface HomeViewProps {
  status: ConsoleStatus | null;
  /** null until the first answer. */
  volumes: Volume[] | null;
  volumesError: boolean;
  jobs: Job[];
  jobsLoaded: boolean;
  jobsError: boolean;
  ask: Ask | null;
  busy: boolean;
  /** The confirmed action failed in a way worth retrying (409 busy, 503): shown in the dialog. */
  askError?: string | null;
  message: Message | null;
  onAsk: (ask: Ask) => void;
  onConfirm: () => void;
  onDismiss: () => void;
  /** The running game's cover (an object URL), or null (the generic icon shows). */
  cover?: string | null;
  /** Finished tasks beyond the cap are shown. */
  /** The cover failed to load: drop it (the generic icon shows). */
  onCoverError?: () => void;
  tasksExpanded?: boolean;
  onToggleTasks?: () => void;
  /** Open the Cheats section, or null while there is none (the shortcut is hidden). */
  onOpenCheats?: (() => void) | null;
  /** Opens Saves at this game, to back up (or restore) its save; null when Saves is absent. */
  onOpenSaves?: ((titleId: string) => void) | null;
  /** ✕ on a storage card: Files at that drive's path, or null when Files cannot open a path. */
  onOpenDrive: ((path: string) => void) | null;
  /** Open Clean up, or null while there is none (the card is hidden). */
  onOpenCleanup?: (() => void) | null;
  /** What the last clean-up scan of this page found, or null before one ran. */
  cleanupFound?: { bytes: number; count: number } | null;
  /** A USB stick just plugged in, or null. */
  usbNew?: Volume | null;
  /** Look for games on it: Library, scanning; null while there is no Library. */
  onFindGames?: (() => void) | null;
  onDismissUsb?: () => void;
}

/** One drive. Focusable; ✕ (a click) opens Files there when `onOpen` is given. */
export function VolumeCard({ v, onOpen }: { v: Volume; onOpen: ((path: string) => void) | null }) {
  const used = v.total_bytes > 0 ? Math.max(0, Math.min(100, 100 - (v.free_bytes / v.total_bytes) * 100)) : 0;
  const Icon = isUsbStick(v) || isUsbExt(v) ? Usb : HardDrive;
  return (
    <li className="cw-volume-item">
      <div
        className="cw-focusable cw-volume"
        tabIndex={0}
        role={onOpen ? "button" : undefined}
        data-volume={v.path}
        onClick={onOpen ? () => onOpen(v.path) : undefined}
        onKeyDown={
          onOpen
            ? (e) => {
                // A real button's keys (the nav layer leaves a handled key alone: no double open).
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  onOpen(v.path);
                }
              }
            : undefined
        }
      >
        <div className="cw-volume-head">
          <Icon aria-hidden />
          <span className="cw-volume-name">{volumeLabel(v)}</span>
          {!v.writable ? <span className="cw-tag">{t("cw_home_vol_readonly")}</span> : null}
        </div>
        <div className="cw-dim cw-mono cw-volume-path">{v.path}</div>
        <div className="cw-bar" aria-hidden>
          <div className={`cw-bar-fill${used >= 90 ? " is-warn" : ""}`} style={{ width: `${used}%` }} />
        </div>
        <div className="cw-volume-free">
          {t("cw_home_free_of", {
            free: formatBytes(v.free_bytes),
            total: formatBytes(v.total_bytes),
          })}
        </div>
      </div>
    </li>
  );
}

/** Home without state: everything comes in through props (tests render it directly). */
export function HomeView(p: HomeViewProps) {
  const running = p.status?.running ?? null;
  // Every running task keeps its row (Home is where it is cancelled); only finished ones are capped.
  const allTasks = visibleJobs(p.jobs);
  const runningTasks = allTasks.filter((j) => j.state === "running");
  const finishedTasks = allTasks.filter((j) => j.state !== "running");
  const finishedShown = p.tasksExpanded ? finishedTasks.length : Math.max(0, MAX_TASK_ROWS - runningTasks.length);
  const tasks = runningTasks.concat(finishedTasks.slice(0, finishedShown));
  const moreTasks = finishedTasks.length - Math.min(finishedShown, finishedTasks.length);
  const canCollapse = !!p.tasksExpanded && finishedTasks.length > Math.max(0, MAX_TASK_ROWS - runningTasks.length);
  const drives = (p.volumes ?? []).filter(isDrive);
  const copy = p.ask ? askCopy(p.ask) : null;

  // Every empty or failed state says what happens next (both lists are read again every few seconds).
  let tasksNote: string | null = null;
  if (p.jobsError && !tasks.length) tasksNote = `${t("cw_home_err_load_tasks")} ${t("cw_home_retrying")}`;
  else if (!p.jobsLoaded) tasksNote = t("cw_home_loading");
  else if (!tasks.length) tasksNote = `${t("cw_home_no_tasks")} ${t("cw_home_no_tasks_next")}`;

  let drivesNote: string | null = null;
  if (p.volumesError && !drives.length) drivesNote = `${t("cw_home_err_load_storage")} ${t("cw_home_retrying")}`;
  else if (p.volumes == null) drivesNote = t("cw_home_loading");

  return (
    <div className="cw-home">
      <h1 className="cw-h1">{t("cw_nav_home")}</h1>
      {/* A device asking to use this PS5 (Allow / Decline), where someone at the TV sees it. */}
      <HomeDeviceAsks />
      {p.message ? (
        <div className={`cw-msg is-${p.message.tone}`} role={p.message.tone === "error" ? "alert" : "status"}>
          {p.message.text}
        </div>
      ) : null}

      {p.usbNew ? (
        <section className="cw-card cw-usb-new" role="status" aria-label={t("cw_home_usb_new_title")} data-usb-new={p.usbNew.path}>
          <p className="cw-usb-new-title">{t("cw_home_usb_new_title")}</p>
          <p className="cw-dim">{t("cw_home_usb_new_body", { free: formatBytes(p.usbNew.free_bytes) })}</p>
          <div className="cw-dialog-buttons">
            {p.onFindGames ? (
              <button type="button" className="cw-focusable cw-button is-primary" data-action="usb-find-games" onClick={p.onFindGames}>
                <span>{t("cw_home_usb_find")}</span>
              </button>
            ) : null}
            {p.onOpenDrive ? (
              <button
                type="button"
                className="cw-focusable cw-button"
                data-action="usb-open"
                onClick={() => p.onOpenDrive?.(p.usbNew!.path)}
              >
                <span>{t("cw_home_usb_open")}</span>
              </button>
            ) : null}
            <button type="button" className="cw-focusable cw-button" data-action="usb-dismiss" onClick={p.onDismissUsb}>
              <span>{t("cw_files_close")}</span>
            </button>
          </div>
        </section>
      ) : null}

      <section className="cw-card cw-game" aria-label={t("cw_home_now_running")}>
        {running && p.cover ? (
          <img className="cw-game-cover" src={p.cover} alt="" onError={p.onCoverError} />
        ) : (
          <Gamepad2 className="cw-game-icon" aria-hidden />
        )}
        {running ? (
          <>
            <div className="cw-game-text">
              <div className="cw-dim">{t("cw_home_now_running")}</div>
              <div className="cw-game-title">{running.name ?? running.title_id}</div>
              {running.name ? <div className="cw-dim cw-mono">{running.title_id}</div> : null}
            </div>
            <button
              type="button"
              className="cw-focusable cw-button"
              data-action="close-game"
              onClick={() =>
                p.onAsk({
                  kind: "close",
                  appId: running.app_id,
                  titleId: running.name ?? running.title_id,
                })
              }
            >
              <X aria-hidden />
              <span>{t("cw_home_close_game")}</span>
            </button>
            {p.onOpenCheats ? (
              <button
                type="button"
                className="cw-focusable cw-button cw-game-cheats"
                data-action="open-cheats"
                onClick={p.onOpenCheats}
              >
                <Sparkles aria-hidden />
                <span>{t("cw_home_cheats")}</span>
              </button>
            ) : null}
            {p.onOpenSaves ? (
              <button
                type="button"
                className="cw-focusable cw-button"
                data-action="open-game-saves"
                onClick={() => p.onOpenSaves?.(running.title_id)}
              >
                <ArchiveRestore aria-hidden />
                <span>{t("cw_home_game_saves")}</span>
              </button>
            ) : null}
          </>
        ) : (
          <div className="cw-game-text cw-dim">{p.status ? t(p.onOpenCheats ? "cw_home_no_game_next" : "cw_home_no_game_plain") : t("cw_home_loading")}</div>
        )}
      </section>

      <section className="cw-home-section">
        <h2 className="cw-h2">{t("cw_home_tasks")}</h2>
        {p.jobsError && tasks.length ? <div className="cw-dim">{t("cw_home_err_load_tasks")}</div> : null}
        {tasksNote ? (
          <p className="cw-dim">{tasksNote}</p>
        ) : (
          <ul className="cw-tasks">
            {tasks.map((j) => (
              <TaskRow key={j.job} job={j} onCancel={(job) => p.onAsk({ kind: "cancel", job })} />
            ))}
          </ul>
        )}
        {(moreTasks > 0 || canCollapse) && p.onToggleTasks ? (
          <button
            type="button"
            className="cw-focusable cw-button cw-tasks-more"
            data-action="toggle-tasks"
            aria-expanded={!!p.tasksExpanded}
            onClick={p.onToggleTasks}
          >
            <span>{moreTasks > 0 ? t("cw_home_tasks_more", { n: moreTasks }) : t("cw_home_tasks_fewer")}</span>
          </button>
        ) : null}
      </section>

      <section className="cw-home-section">
        <h2 className="cw-h2">{t("cw_home_storage")}</h2>
        {drivesNote ? (
          <p className="cw-dim">{drivesNote}</p>
        ) : (
          <ul className="cw-volumes">
            {drives.map((v) => (
              <VolumeCard key={v.path} v={v} onOpen={p.onOpenDrive} />
            ))}
          </ul>
        )}
        {p.onOpenCleanup ? (
          <button
            type="button"
            className="cw-focusable cw-button cw-card cw-cleanup-card"
            data-action="open-cleanup"
            onClick={p.onOpenCleanup}
          >
            <Eraser aria-hidden />
            <span className="cw-cleanup-text">
              <span className="cw-cleanup-name">{t("cw_cleanup_home_card")}</span>
              <span className="cw-dim">
                {p.cleanupFound && p.cleanupFound.count > 0
                  ? t("cw_cleanup_home_found", { size: formatBytes(p.cleanupFound.bytes) })
                  : t("cw_cleanup_home_hint")}
              </span>
            </span>
          </button>
        ) : null}
      </section>

      {p.ask && copy ? (
        <ConfirmDialog
          title={copy.title}
          body={copy.body}
          confirmLabel={copy.confirm}
          cancelLabel={copy.cancel}
          danger={copy.danger}
          busy={p.busy}
          error={p.askError}
          onConfirm={p.onConfirm}
          onCancel={p.onDismiss}
        />
      ) : null}
    </div>
  );
}

/* ---- the section ---- */

function useVolumes(tick: number): { volumes: Volume[] | null; error: boolean } {
  const [volumes, setVolumes] = useState<Volume[] | null>(null);
  const [error, setError] = useState(false);
  useEffect(() => {
    let live = true;
    let inFlight = false;
    let stop = () => {};
    const load = () => {
      if (inFlight) return;
      inFlight = true;
      call<{ volumes?: unknown }>("/api/volumes").then(
        (r) => {
          inFlight = false;
          if (!live) return;
          setVolumes(Array.isArray(r?.volumes) ? (r.volumes as Volume[]) : []);
          setError(false);
        },
        (e: unknown) => {
          inFlight = false;
          if (!live) return;
          setError(true);
          // The token is gone: every read would 401 until the page reloads.
          if (e instanceof ApiError && e.status === 401) stop();
        },
      );
    };
    // Not while the page is hidden (the browser in the background): nothing shown could change.
    stop = pollWhileVisible(load, VOLUMES_POLL_MS);
    return () => {
      live = false;
      stop();
    };
  }, [tick]);
  return { volumes, error };
}

/** A title id the helper can look up (also keeps the path below free of anything else). */
const TITLE_ID = /^[A-Z]{4}\d{5}$/;

/** The running game's cover: its icon0.png read through the helper, as an object URL; null when unreadable. */
export function useAppCover(titleId: string | null): string | null {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    setUrl(null);
    if (!titleId || !TITLE_ID.test(titleId)) return;
    let live = true;
    let made: string | null = null;
    const ctl = new AbortController();
    callBlob("/api/fs/read", { path: `/user/appmeta/${titleId}/icon0.png` }, ctl.signal).then(
      (b) => {
        if (!live || !/^image\/(png|jpeg)$/.test(b.type)) return;
        made = URL.createObjectURL(b);
        setUrl(made);
      },
      () => {},
    );
    return () => {
      live = false;
      ctl.abort();
      if (made) URL.revokeObjectURL(made);
    };
  }, [titleId]);
  return url;
}

/** The Home section: the running game, tasks and storage. */
export function Home({ ctx }: { ctx: SectionContext }) {
  const { jobs, loaded, error: jobsError, cancel, refresh: refreshJobs } = useJobs();
  const [volumesTick, setVolumesTick] = useState(0);
  const { volumes, error: volumesError } = useVolumes(volumesTick);
  const [ask, setAsk] = useState<Ask | null>(null);
  const [busy, setBusy] = useState(false);
  const [askError, setAskError] = useState<string | null>(null);
  const [message, setMessage] = useState<Message | null>(null);
  const { refreshStatus, openSection, canOpen, hasSection } = ctx;

  // Create (refresh): read the tasks and drives again; App's fallback re-reads the status.
  useEffect(
    () =>
      onNavAction((a) => {
        if (a !== "refresh") return false;
        refreshJobs();
        setVolumesTick((n) => n + 1);
        return false;
      }),
    [refreshJobs],
  );

  useEffect(() => {
    if (!message) return;
    const timer = setTimeout(() => setMessage(null), MESSAGE_MS);
    return () => clearTimeout(timer);
  }, [message]);

  const onConfirm = useCallback(() => {
    if (!ask || busy) return;
    setBusy(true);
    setAskError(null);
    runAsk(ask, { cancel }).then(
      () => {
        setMessage(null);
        if (ask.kind === "close") refreshStatus();
        setBusy(false);
        setAsk(null);
      },
      (e: unknown) => {
        const text = errorText(e);
        setBusy(false);
        if (text && keepsDialogOpen(e)) {
          // Busy or stopping: say so where the user is looking; ✕ tries again, ○ gives up.
          setAskError(text);
          return;
        }
        setMessage(text ? { tone: "error", text } : null);
        setAsk(null);
      },
    );
  }, [ask, busy, cancel, refreshStatus]);

  const onDismiss = useCallback(() => {
    if (busy) return;
    setAsk(null);
    setAskError(null);
  }, [busy]);

  const [coverFailed, setCoverFailed] = useState<string | null>(null);
  const [tasksExpanded, setTasksExpanded] = useState(false);
  const titleId = ctx.status?.running?.title_id ?? null;
  const loadedCover = useAppCover(titleId);
  const cover = loadedCover && coverFailed !== loadedCover ? loadedCover : null;
  const onOpenCheats = hasSection("cheats") ? () => openSection("cheats") : null;
  const onOpenSaves = canOpen("saves") ? (title: string) => openSection("saves", { title }) : null;
  const onOpenDrive = canOpen("files") ? (path: string) => openSection("files", { path }) : null;
  const onOpenCleanup = hasSection("system") ? () => openSection("system", { view: "cleanup" }) : null;
  const [cleanupFound, setCleanupFound] = useState(cleanupLastFound);
  useEffect(() => onCleanupFound(() => setCleanupFound(cleanupLastFound())), []);
  // A USB stick plugged in while the page is open: offer to find games on it. Cleared when it
  // goes away.
  const seenDrives = useRef<Set<string> | null>(null);
  const [usbNew, setUsbNew] = useState<Volume | null>(null);
  useEffect(() => {
    if (!volumes) return;
    const fresh = newUsbStick(seenDrives.current, volumes);
    seenDrives.current = new Set(volumes.map((v) => v.path));
    if (fresh) setUsbNew(fresh);
    else setUsbNew((cur) => (cur && seenDrives.current?.has(cur.path) ? cur : null));
  }, [volumes]);
  const onFindGames = hasSection("games") ? () => openSection("games", { view: "library", scan: "1" }) : null;

  return (
    <HomeView
      status={ctx.status}
      volumes={volumes}
      volumesError={volumesError}
      jobs={jobs}
      jobsLoaded={loaded}
      jobsError={jobsError != null}
      ask={ask}
      busy={busy}
      askError={askError}
      message={message}
      onAsk={(a) => {
        setMessage(null);
        setAskError(null);
        setAsk(a);
      }}
      onConfirm={onConfirm}
      onDismiss={onDismiss}
      cover={cover}
      onCoverError={() => setCoverFailed(loadedCover)}
      tasksExpanded={tasksExpanded}
      onToggleTasks={() => setTasksExpanded((v) => !v)}
      onOpenCheats={onOpenCheats}
      onOpenSaves={onOpenSaves}
      onOpenDrive={onOpenDrive}
      onOpenCleanup={onOpenCleanup}
      cleanupFound={cleanupFound}
      usbNew={usbNew}
      onFindGames={onFindGames}
      onDismissUsb={() => setUsbNew(null)}
    />
  );
}
