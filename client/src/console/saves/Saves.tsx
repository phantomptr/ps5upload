import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import { ArchiveRestore, Gamepad2, HardDriveDownload, RefreshCw, Square, Trash2, X } from "lucide-react";
import { formatBytes } from "../../lib/format";
import { ApiError, call } from "../api";
import { errorText, isWritableUsb, volumeLabel, type Message, type Volume } from "../home/Home";
import { loadImage, useLazyImage } from "../games/covers";
import { parseDbPage } from "../games/model";
import { CARD_PX } from "../library/Library";
import { onNavAction } from "../nav";
import type { SectionContext } from "../sections";
import { getLanguage, t } from "../strings";
import { ConfirmDialog } from "../ui/ConfirmDialog";
import { Modal } from "../ui/Modal";
import {
  BACKUP_ROOT,
  backUp,
  backupsDir,
  deleteBackup,
  livePath,
  loadBackups,
  loadSaves,
  loadUsers,
  restore,
  SAVE_KINDS,
  saveIconPath,
  shallowSize,
  type Backup,
  type SaveKind,
  type SaveTitle,
  type SaveUser,
  usbBackupRoot,
} from "./model";

/** A place backups can go: the internal drive, or a USB drive (#528). */
export interface BackupPlace {
  root: string;
  label: string;
}

/** The internal drive, then each USB drive that can be written to. */
export function backupPlaces(volumes: readonly Volume[]): BackupPlace[] {
  const usb = volumes.filter(isWritableUsb).map((v) => ({ root: usbBackupRoot(v.path), label: `${volumeLabel(v)} (${v.path.slice(5)})` }));
  return [{ root: BACKUP_ROOT, label: t("cw_home_vol_internal") }, ...usb];
}

/** A backup, restore or delete running: one at a time, page-wide. */
interface Task {
  text: string;
}

export type SaveAsk = { kind: "restore"; backup: Backup } | { kind: "delete"; backup: Backup };

export interface SavesViewProps {
  users: SaveUser[] | null;
  user: string | null;
  saves: SaveTitle[] | null;
  names: ReadonlyMap<string, string>;
  /** A save's own icon by title, for a game no longer installed (no name from app.db). */
  icons: ReadonlyMap<string, string>;
  /** Backups by title (for the user shown); a title not read yet is missing. */
  backups: ReadonlyMap<string, Backup[]>;
  /** The open title's current save size, and each backup's (by name), once read. */
  sizes: ReadonlyMap<string, number>;
  open: string | null;
  task: Task | null;
  message: Message | null;
  loadError: string | null;
  /** The game running now (/api/status), or null. */
  running: { titleId: string; appId: number } | null;
  ask: SaveAsk | null;
  askError: string | null;
  /** Where backups go and are listed from; the chooser shows when there is more than one. */
  places: BackupPlace[];
  place: string;
  onPlace: (root: string) => void;
  onUser: (id: string) => void;
  onOpen: (titleId: string | null) => void;
  onRefresh: () => void;
  onBackUp: (s: SaveTitle) => void;
  onAsk: (a: SaveAsk | null) => void;
  onConfirm: () => void;
  onCloseGame: (appId: number) => void;
}

const when = (b: Backup) => new Date(b.when).toLocaleString(getLanguage());

const kindText = (k: SaveKind) =>
  k === "savedata_prospero" ? t("cw_saves_kind_prospero") : k === "savedata" ? t("cw_saves_kind_ps4") : t("cw_saves_kind_meta");

/** The game's cover; a game no longer installed shows its save's own icon (`fallback`). */
function Cover({ titleId, big, fallback }: { titleId: string; big?: boolean; fallback?: string | null }) {
  const ref = useRef<HTMLSpanElement>(null);
  const load = useCallback(
    // The fallback arrives after the first try, which can already have missed: a key of its
    // own, or the remembered miss would keep the placeholder for a game no longer installed.
    (id: string) =>
      loadImage(`save:${id}@${CARD_PX}${fallback ? "+icon" : ""}`, [`/user/appmeta/${id}/icon0.png`, `/user/app/${id}/sce_sys/icon0.png`, ...(fallback ? [fallback] : [])], CARD_PX),
    [fallback],
  );
  const url = useLazyImage(titleId, ref, load);
  return (
    <span ref={ref} className={`cw-gcover${big ? " is-big" : ""}`} aria-hidden>
      {url ? <img src={url} alt="" /> : <Gamepad2 />}
    </span>
  );
}

function backupsText(list: Backup[] | undefined): string {
  if (!list) return "";
  if (!list.length) return t("cw_saves_backups_none");
  return list.length === 1 ? t("cw_saves_backups_one") : t("cw_saves_backups_n", { n: list.length });
}

const sizeKey = (titleId: string, backup?: string) => (backup ? `${titleId}/${backup}` : titleId);

function Sheet({ s, p }: { s: SaveTitle; p: SavesViewProps }) {
  const name = p.names.get(s.titleId) || s.titleId;
  const user = p.users?.find((u) => u.id === p.user);
  const list = p.backups.get(s.titleId);
  const busy = p.task !== null;
  const running = p.running?.titleId === s.titleId ? p.running : null;
  const current = p.sizes.get(sizeKey(s.titleId));
  const close = useCallback(() => p.onOpen(null), [p]);
  return (
    <Modal label={name} className="cw-sheet cw-game-sheet cw-saves-sheet" onBack={close}>
      <div className="cw-game-sheet-head">
        <Cover titleId={s.titleId} big fallback={p.icons.get(s.titleId)} />
        <div className="cw-game-text">
          <h2 className="cw-sheet-title">{name}</h2>
          <span className="cw-gcard-tags">
            {user ? <span className="cw-tag">{user.name}</span> : null}
            {running ? <span className="cw-tag is-on">{t("cw_games_running")}</span> : null}
          </span>
        </div>
      </div>
      <dl className="cw-info">
        <div>
          <dt className="cw-dim">{t("cw_games_info_title_id")}</dt>
          <dd className="cw-mono">{s.titleId}</dd>
        </div>
        <div>
          <dt className="cw-dim">{t("cw_saves_info_current")}</dt>
          <dd>{current != null ? formatBytes(current) : "…"}</dd>
        </div>
        <div>
          <dt className="cw-dim">{t("cw_saves_info_kinds")}</dt>
          <dd>{s.kinds.map(kindText).join(", ")}</dd>
        </div>
      </dl>
      {running ? (
        <div className="cw-msg is-error" role="status">
          {t("cw_saves_running", { name })}
          <button type="button" className="cw-focusable cw-button cw-msg-action is-danger" data-action="save-close-game" onClick={() => p.onCloseGame(running.appId)}>
            <Square aria-hidden />
            <span>{t("cw_home_close_game")}</span>
          </button>
        </div>
      ) : null}
      {p.task ? (
        <p className="cw-msg" role="status">
          {p.task.text}
        </p>
      ) : p.message ? (
        <div className={`cw-msg ${p.message.tone === "error" ? "is-error" : "is-info"}`} role={p.message.tone === "error" ? "alert" : "status"}>
          {p.message.text}
        </div>
      ) : null}
      <div className="cw-saves-actions">
        <button type="button" className="cw-focusable cw-button is-primary" data-action="save-backup" data-nav-default="" disabled={busy} onClick={() => p.onBackUp(s)}>
          <HardDriveDownload aria-hidden />
          <span>{t("cw_saves_back_up")}</span>
        </button>
      </div>
      <h3 className="cw-libview-h3">
        <ArchiveRestore aria-hidden />
        <span>{list ? backupsText(list) : t("cw_games_loading")}</span>
      </h3>
      {list?.length ? (
        <ul className="cw-libparts cw-saves-list">
          {list.map((b) => {
            const size = p.sizes.get(sizeKey(s.titleId, b.name));
            return (
              <li key={b.name} className="cw-libpart" data-backup={b.name}>
                <div className="cw-libpart-text">
                  <span className="cw-libpart-name">{when(b)}</span>
                  {b.auto ? <span className="cw-tag">{t("cw_saves_auto")}</span> : null}
                  <span className="cw-libpart-meta cw-dim">{size != null ? formatBytes(size) : "…"}</span>
                </div>
                <span className="cw-libpart-actions">
                  <button
                    type="button"
                    className="cw-focusable cw-button"
                    data-action="save-restore"
                    disabled={busy || !!running}
                    onClick={() => p.onAsk({ kind: "restore", backup: b })}
                  >
                    <ArchiveRestore aria-hidden />
                    <span>{t("cw_saves_restore")}</span>
                  </button>
                  <button type="button" className="cw-focusable cw-button is-danger" data-action="save-delete" disabled={busy} onClick={() => p.onAsk({ kind: "delete", backup: b })}>
                    <Trash2 aria-hidden />
                    <span>{t("cw_saves_delete")}</span>
                  </button>
                </span>
              </li>
            );
          })}
        </ul>
      ) : null}
      <p className="cw-dim cw-saves-where">
        {t(p.place === BACKUP_ROOT ? "cw_saves_where" : "cw_saves_where_usb", { path: backupsDir(p.user ?? "", s.titleId, p.place) })}
      </p>
      <div className="cw-dialog-buttons">
        <button type="button" className="cw-focusable cw-button" onClick={close}>
          <X aria-hidden />
          <span>{t("cw_files_close")}</span>
        </button>
      </div>
      {p.ask ? (
        <ConfirmDialog
          title={p.ask.kind === "restore" ? t("cw_saves_restore_title") : t("cw_saves_delete_title")}
          body={
            p.ask.kind === "restore"
              ? t("cw_saves_restore_body", { game: name, user: user?.name ?? p.user ?? "", when: when(p.ask.backup) })
              : t("cw_saves_delete_body", { when: when(p.ask.backup) })
          }
          confirmLabel={p.ask.kind === "restore" ? t("cw_saves_restore") : t("cw_saves_delete")}
          danger
          busy={busy}
          error={p.askError}
          onConfirm={p.onConfirm}
          onCancel={() => p.onAsk(null)}
        />
      ) : null}
    </Modal>
  );
}

/** The Saves section without state (tests render it directly). */
export function SavesView(p: SavesViewProps) {
  const open = p.open ? p.saves?.find((s) => s.titleId === p.open) ?? null : null;
  const user = p.users?.find((u) => u.id === p.user);
  let body;
  if (p.loadError) body = <div className="cw-msg is-error" role="alert">{p.loadError}</div>;
  else if (!p.users || !p.saves) body = <p className="cw-dim">{t("cw_games_loading")}</p>;
  else if (!p.saves.length) body = <p className="cw-dim">{t("cw_saves_none", { name: user?.name ?? "" })}</p>;
  else
    body = (
      <ul className="cw-games cw-libgrid" aria-label={t("cw_saves_title")}>
        {p.saves.map((s) => (
          <li key={s.titleId} className="cw-gcard-item">
            <button type="button" className="cw-focusable cw-gcard cw-libcard" data-save-key={`${p.user}/${s.titleId}`} onClick={() => p.onOpen(s.titleId)}>
              <Cover titleId={s.titleId} fallback={p.icons.get(s.titleId)} />
              <span className="cw-libcard-name">{p.names.get(s.titleId) || s.titleId}</span>
              <span className="cw-libcard-meta cw-dim">{backupsText(p.backups.get(s.titleId)) || s.titleId}</span>
            </button>
          </li>
        ))}
      </ul>
    );
  return (
    <div className="cw-games-section">
      <h1 className="cw-h1">{t("cw_saves_title")}</h1>
      <p className="cw-dim">{t("cw_saves_hint")}</p>
      <div className="cw-toolbar">
        <span className="cw-saves-users">
          {(p.users ?? []).map((u) => (
            <button key={u.id} type="button" className="cw-focusable cw-button" aria-pressed={u.id === p.user} data-user={u.id} onClick={() => p.onUser(u.id)}>
              <span>{u.name}</span>
            </button>
          ))}
        </span>
        <button type="button" className="cw-focusable cw-button" data-action="saves-refresh" disabled={!!p.task} onClick={p.onRefresh}>
          <RefreshCw aria-hidden />
          <span>{t("cw_saves_refresh")}</span>
        </button>
      </div>
      {p.places.length > 1 ? (
        <div className="cw-toolbar" role="group" aria-label={t("cw_saves_place")}>
          <span className="cw-dim">{t("cw_saves_place")}</span>
          <span className="cw-saves-users">
            {p.places.map((pl) => (
              <button
                key={pl.root}
                type="button"
                className="cw-focusable cw-button"
                aria-pressed={pl.root === p.place}
                data-place={pl.root}
                disabled={!!p.task}
                onClick={() => p.onPlace(pl.root)}
              >
                <span>{pl.label}</span>
              </button>
            ))}
          </span>
        </div>
      ) : null}
      {p.message && !open ? (
        <div className={`cw-msg ${p.message.tone === "error" ? "is-error" : "is-info"}`} role={p.message.tone === "error" ? "alert" : "status"}>
          {p.message.text}
        </div>
      ) : null}
      {body}
      {open ? <Sheet s={open} p={p} /> : null}
    </div>
  );
}

/* ---- state ---- */

interface SavesState {
  task: Task | null;
  message: Message | null;
}

/** Outlives the section: a backup started here finishes, and says so, when the section is opened again. */
const st: SavesState = { task: null, message: null };
/** The backup place last picked (kept while the page is open). */
let lastPlace = BACKUP_ROOT;
const listeners = new Set<() => void>();
function patch(p: Partial<SavesState>): void {
  Object.assign(st, p);
  for (const fn of Array.from(listeners)) fn();
}

/** Why a call failed, as one line. */
export function saveErrorText(e: unknown): string {
  if (e instanceof ApiError) {
    if (e.code === "save_not_found") return t("cw_saves_err_gone");
    if (/^fs_mkdir/.test(e.code) || e.code === "permission_denied" || e.code === "not_allowed") return t("cw_saves_err_no_folder");
  }
  return errorText(e) || (e instanceof ApiError ? e.code : String(e));
}

/** Names by title id from app.db (installed games); a save of a removed game shows its id. */
async function loadNames(): Promise<Map<string, string>> {
  const out = new Map<string, string>();
  let offset = 0;
  for (let page = 0; page < 20; page++) {
    const p = parseDbPage(await call<unknown>("/api/apps/db", { offset }));
    for (const [id, name] of p.rows) if (name && name !== id && !out.has(id)) out.set(id, name);
    if (!p.more || p.next <= offset) break;
    offset = p.next;
  }
  return out;
}

/** The Saves section: per user, each game's save; back it up on the console, restore or delete a backup. */
export function Saves({ ctx }: { ctx: SectionContext }) {
  const [, bump] = useReducer((n: number) => n + 1, 0);
  const [users, setUsers] = useState<SaveUser[] | null>(null);
  const [user, setUser] = useState<string | null>(null);
  const [saves, setSaves] = useState<SaveTitle[] | null>(null);
  const [names, setNames] = useState<Map<string, string>>(new Map());
  const [icons, setIcons] = useState<Map<string, string>>(new Map());
  const [namesRead, setNamesRead] = useState(false);
  const [backups, setBackups] = useState<Map<string, Backup[]>>(new Map());
  const [sizes, setSizes] = useState<Map<string, number>>(new Map());
  const [open, setOpen] = useState<string | null>(ctx.params?.title ?? null);
  const [ask, setAsk] = useState<SaveAsk | null>(null);
  const [askError, setAskError] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [places, setPlaces] = useState<BackupPlace[]>(() => backupPlaces([]));
  const [place, setPlaceState] = useState(lastPlace);
  const setPlace = (r: string) => {
    lastPlace = r;
    setPlaceState(r);
  };
  const root = useRef<HTMLDivElement>(null);
  const live = useRef(true);

  useEffect(() => {
    live.current = true;
    listeners.add(bump);
    return () => {
      live.current = false;
      listeners.delete(bump);
    };
  }, []);

  const loadUserList = useCallback(async () => {
    try {
      const u = await loadUsers();
      if (!live.current) return;
      setUsers(u);
      setLoadError(null);
      setUser((cur) => (cur && u.some((x) => x.id === cur) ? cur : (u.find((x) => x.foreground) ?? u[0])?.id ?? null));
      if (!u.length) setSaves([]);
    } catch (e) {
      if (live.current) setLoadError(saveErrorText(e));
    }
  }, []);

  const loadPlaces = useCallback(async () => {
    const r = await call<{ volumes?: unknown }>("/api/volumes").catch(() => null);
    if (!live.current) return;
    const pl = backupPlaces(Array.isArray(r?.volumes) ? (r.volumes as Volume[]) : []);
    setPlaces(pl);
    // the drive was unplugged: back to the internal drive
    if (!pl.some((x) => x.root === lastPlace)) {
      lastPlace = BACKUP_ROOT;
      setPlaceState(BACKUP_ROOT);
    }
  }, []);

  const loadBackupsOf = useCallback(async (u: string, ids: string[], where: string) => {
    for (const id of ids) {
      try {
        const b = await loadBackups(u, id, call, where);
        if (!live.current) return;
        setBackups((m) => new Map(m).set(id, b));
      } catch {
        // unknown: the card says nothing
      }
    }
  }, []);

  const loadUserSaves = useCallback(
    async (u: string, where: string) => {
      setSaves(null);
      setBackups(new Map());
      try {
        const s = await loadSaves(u);
        if (!live.current) return;
        setSaves(s);
        void loadBackupsOf(
          u,
          s.map((x) => x.titleId),
          where,
        );
      } catch (e) {
        if (live.current) setLoadError(saveErrorText(e));
      }
    },
    [loadBackupsOf],
  );

  useEffect(() => {
    void loadUserList();
    void loadPlaces();
    loadNames().then(
      (n) => {
        if (!live.current) return;
        setNames(n);
        setNamesRead(true);
      },
      () => live.current && setNamesRead(true),
    );
  }, [loadUserList, loadPlaces]);

  useEffect(() => {
    if (user) void loadUserSaves(user, place);
  }, [user, place, loadUserSaves]);

  // Games no longer installed (no name in app.db): their save's own icon as the cover.
  useEffect(() => {
    if (!user || !saves || !namesRead) return;
    let on = true;
    void (async () => {
      for (const s of saves) {
        if (names.has(s.titleId)) continue;
        const icon = await saveIconPath(user, s).catch(() => null);
        if (!on) return;
        if (icon) setIcons((m) => new Map(m).set(s.titleId, icon));
      }
    })();
    return () => {
      on = false;
    };
  }, [user, saves, names, namesRead]);

  // The open title's sizes: its current save and each backup.
  const openSave = open && saves ? saves.find((s) => s.titleId === open) ?? null : null;
  const openBackups = open ? backups.get(open) : undefined;
  useEffect(() => {
    if (!openSave || !user) return;
    let on = true;
    void (async () => {
      let total = 0;
      for (const k of openSave.kinds) total += await shallowSize(livePath(user, k, openSave.titleId)).catch(() => 0);
      if (on) setSizes((m) => new Map(m).set(sizeKey(openSave.titleId), total));
      for (const b of openBackups ?? []) {
        const key = sizeKey(openSave.titleId, b.name);
        if (sizes.has(key)) continue;
        let n = 0;
        for (const k of SAVE_KINDS)
          n += await shallowSize(`${backupsDir(user, openSave.titleId, place)}/${b.name}/${k}`).catch(() => 0);
        if (on) setSizes((m) => new Map(m).set(key, n));
      }
    })();
    return () => {
      on = false;
    };
    // sizes is read only to skip what is known
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [openSave, openBackups, user]);

  useEffect(
    () =>
      onNavAction(
        (a) => {
          if (a === "refresh" && !st.task) {
            void loadUserList();
            void loadPlaces();
            if (user) void loadUserSaves(user, place);
            return true;
          }
          return false;
        },
        () => root.current,
      ),
    [loadUserList, loadUserSaves, loadPlaces, user, place],
  );

  const running = ctx.status?.running ? { titleId: ctx.status.running.title_id, appId: ctx.status.running.app_id } : null;
  const name = (id: string) => names.get(id) || id;

  const reloadBackups = async (u: string, id: string, where = place) => {
    const b = await loadBackups(u, id, call, where).catch(() => null);
    if (b && live.current) setBackups((m) => new Map(m).set(id, b));
  };

  const doBackUp = async (s: SaveTitle) => {
    if (st.task || !user) return;
    const u = user;
    const where = place;
    patch({ message: null, task: { text: t("cw_saves_backing_up", { name: name(s.titleId) }) } });
    try {
      const made = await backUp(u, s.titleId, s.kinds, { root: where });
      const b = (await loadBackups(u, s.titleId, call, where).catch(() => [])).find((x) => x.name === made);
      patch({ task: null, message: { tone: "info", text: t("cw_saves_backed_up", { when: b ? when(b) : made }) } });
    } catch (e) {
      patch({ task: null, message: { tone: "error", text: t("cw_saves_failed", { why: saveErrorText(e) }) } });
    }
    await reloadBackups(u, s.titleId, where);
  };

  const doConfirm = async () => {
    if (!ask || st.task || !user || !openSave) return;
    const u = user;
    const s = openSave;
    const a = ask;
    const where = place;
    setAskError(null);
    if (a.kind === "delete") {
      patch({ message: null, task: { text: t("cw_saves_deleting") } });
      try {
        await deleteBackup(u, s.titleId, a.backup.name, undefined, where);
        patch({ task: null, message: { tone: "info", text: t("cw_saves_deleted") } });
        setAsk(null);
      } catch (e) {
        patch({ task: null });
        setAskError(saveErrorText(e));
      }
      await reloadBackups(u, s.titleId, where);
      return;
    }
    // Restore: the game must be closed, as the console reports it now (not as last polled).
    try {
      const now = await call<{ running?: { title_id?: string } | null }>("/api/status");
      if (now?.running?.title_id === s.titleId) {
        setAskError(t("cw_saves_running", { name: name(s.titleId) }));
        ctx.refreshStatus();
        return;
      }
    } catch (e) {
      setAskError(saveErrorText(e));
      return;
    }
    setAsk(null);
    patch({ message: null, task: { text: t("cw_saves_restoring", { name: name(s.titleId) }) } });
    try {
      await restore(u, s.titleId, a.backup.name, s.kinds, undefined, where);
      patch({ task: null, message: { tone: "info", text: t("cw_saves_restored", { when: when(a.backup) }) } });
    } catch (e) {
      // the current save was backed up first (unless that is what failed): say where it is
      const kept = (await loadBackups(u, s.titleId).catch(() => [])).some((b) => b.auto && Date.now() - b.when < 10 * 60 * 1000);
      patch({ task: null, message: { tone: "error", text: t(kept ? "cw_saves_restore_failed" : "cw_saves_failed", { why: saveErrorText(e) }) } });
    }
    await reloadBackups(u, s.titleId, where);
    setSizes((m) => {
      const n = new Map(m);
      n.delete(sizeKey(s.titleId));
      return n;
    });
  };

  const closeGame = async (appId: number) => {
    try {
      await call("/api/app/close", { app_id: appId });
    } catch (e) {
      patch({ message: { tone: "error", text: saveErrorText(e) } });
    }
    ctx.refreshStatus();
  };

  return (
    <div ref={root}>
      <SavesView
        users={users}
        user={user}
        saves={saves}
        names={names}
        icons={icons}
        backups={backups}
        sizes={sizes}
        open={openSave ? open : null}
        task={st.task}
        message={st.message}
        loadError={loadError}
        running={running}
        ask={ask}
        askError={askError}
        places={places}
        place={place}
        onPlace={(r) => {
          if (st.task || r === place) return;
          setSizes(new Map());
          setPlace(r);
        }}
        onUser={(id) => {
          setOpen(null);
          setUser(id);
        }}
        onOpen={(id) => {
          setOpen(id);
          setAsk(null);
          setAskError(null);
          if (!st.task) patch({ message: null });
        }}
        onRefresh={() => {
          void loadUserList();
          void loadPlaces();
          if (user) void loadUserSaves(user, place);
        }}
        onBackUp={(s) => void doBackUp(s)}
        onAsk={(a) => {
          if (st.task) return;
          setAskError(null);
          setAsk(a);
        }}
        onConfirm={() => void doConfirm()}
        onCloseGame={(id) => void closeGame(id)}
      />
    </div>
  );
}
