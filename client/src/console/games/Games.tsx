import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  ArchiveRestore,
  ArrowDownAZ,
  Clock,
  Eye,
  EyeOff,
  Gamepad2,
  Play,
  RefreshCw,
  Search,
  Square,
  Trash2,
  X,
} from "lucide-react";
import { formatBytes } from "../../lib/format";
import { ApiError, call } from "../api";
import { errorText, keepsDialogOpen, type Message } from "../home/Home";
import { onNavAction } from "../nav";
import type { SectionContext } from "../sections";
import { getLanguage, t } from "../strings";
import { ConfirmDialog } from "../ui/ConfirmDialog";
import { Modal } from "../ui/Modal";
import { pollWhileVisible } from "../ui/visiblePoll";
import { useLazyCover } from "./covers";
import {
  codeOf,
  filterGames,
  isHomebrewRefusal,
  loadInstalled,
  parseProcesses,
  refusedCode,
  runningGames,
  sortGames,
  stillInstalled,
  type Game,
  type Installed,
  type Place,
  type RunningGame,
  type Show,
  type SortMode,
} from "./model";

/** The process list (which game runs) is read this often while Games is open. */
export const GAMES_POLL_MS = 5_000;
/** After Launch, the game is looked for this often, and for this long, before "give it a moment". */
export const LAUNCH_POLL_MS = 2_000;
export const LAUNCH_WAIT_MS = 90_000;
/** After an accepted uninstall the title is checked this often, for this long (a big game takes minutes). */
export const REMOVE_POLL_MS = 5_000;
export const REMOVE_WAIT_MS = 180_000;
/** After Close, the game is looked for this long before the next try at a harder stop. */
const CLOSE_WAIT_MS = 6_000;
/** An action's result line clears itself after this long. */
const MESSAGE_MS = 10_000;

export type GameAsk = { kind: "uninstall"; game: Game } | { kind: "close"; game: Game; run: RunningGame };

export function placeLabel(p: Place): string {
  switch (p) {
    case "internal":
      return t("cw_home_vol_internal");
    case "ext":
      return t("cw_home_vol_ext");
    case "usb":
      return t("cw_home_vol_usb");
    case "image":
      return t("cw_games_place_image");
    case "folder":
      return t("cw_games_place_folder");
    default:
      return t("cw_games_place_unknown");
  }
}

/** A failed launch as one line. */
export function launchErrorText(err: unknown): string | null {
  const code = codeOf(err);
  if (code === "other_game_running") return t("cw_games_err_other_running");
  if (isHomebrewRefusal(code)) return t("cw_games_err_launch_refused");
  return errorText(err);
}

/** A failed uninstall as one line. */
export function uninstallErrorText(err: unknown): string | null {
  const code = codeOf(err);
  if (code === "game_running") return t("cw_games_err_running");
  const sony = refusedCode(code);
  if (sony) return t("cw_games_err_uninstall_refused", { code: sony });
  if (code === "uninstall_unavailable") return t("cw_games_err_uninstall_unavailable");
  return errorText(err);
}

function lastPlayedText(ms: number | null): string {
  if (ms == null) return t("cw_games_never");
  try {
    return new Date(ms).toLocaleDateString(getLanguage());
  } catch {
    return t("cw_games_never");
  }
}

/* ---- view ---- */

export interface GamesViewProps {
  /** Games' Installed / On Drives & Shares switch, under the title. */
  switcher?: ReactNode;
  data: Installed | null;
  loadError: string | null;
  query: string;
  sort: SortMode;
  show: Show;
  running: ReadonlyMap<string, RunningGame>;
  /** The game the screen shows (PROC_FOCUS, /api/status), for the "On screen" tag. */
  foreground: string | null;
  launching: string | null;
  removing: ReadonlySet<string>;
  message: Message | null;
  /** The game whose sheet is open. */
  openId: string | null;
  ask: GameAsk | null;
  busy: boolean;
  askError?: string | null;
  onQuery: (q: string) => void;
  onSort: () => void;
  onShow: () => void;
  onRefresh: () => void;
  onOpen: (id: string) => void;
  onCloseSheet: () => void;
  onLaunch: (g: Game) => void;
  onToggleHidden: (g: Game) => void;
  onAsk: (a: GameAsk) => void;
  onConfirm: () => void;
  onDismiss: () => void;
  /** Open the game's save backups (the Saves section); absent: no button. */
  onSaves?: (g: Game) => void;
}

function Cover({ titleId, big }: { titleId: string; big?: boolean }) {
  const ref = useRef<HTMLSpanElement>(null);
  const url = useLazyCover(titleId, ref);
  const [failed, setFailed] = useState<string | null>(null);
  return (
    <span ref={ref} className={`cw-gcover${big ? " is-big" : ""}`} aria-hidden>
      {url && failed !== url ? <img src={url} alt="" onError={() => setFailed(url)} /> : <Gamepad2 />}
    </span>
  );
}

function Tags({ g, p }: { g: Game; p: GamesViewProps }) {
  const tags: { key: string; text: string; cls?: string }[] = [];
  if (p.launching === g.titleId) tags.push({ key: "starting", text: t("cw_games_starting"), cls: " is-on" });
  else if (p.running.has(g.titleId))
    tags.push({
      key: "running",
      text: p.foreground === g.titleId ? t("cw_games_on_screen") : t("cw_games_running"),
      cls: " is-on",
    });
  if (p.removing.has(g.titleId)) tags.push({ key: "removing", text: t("cw_games_removing") });
  if (g.hidden) tags.push({ key: "hidden", text: t("cw_games_hidden") });
  if (!tags.length) return null;
  return (
    <span className="cw-gcard-tags">
      {tags.map((x) => (
        <span key={x.key} className={`cw-tag${x.cls ?? ""}`}>
          {x.text}
        </span>
      ))}
    </span>
  );
}

function GameSheet({ g, p, children }: { g: Game; p: GamesViewProps; children?: ReactNode }) {
  const run = p.running.get(g.titleId) ?? null;
  const otherRunning = !run && p.running.size > 0;
  const starting = p.launching !== null;
  const removing = p.removing.has(g.titleId);
  const rows: [string, string, boolean?][] = [
    [t("cw_games_info_title_id"), g.titleId, true],
    [t("cw_games_info_version"), g.version ?? "—"],
    [t("cw_games_info_size"), g.sizeBytes != null ? formatBytes(g.sizeBytes) : "—"],
    [t("cw_games_info_location"), placeLabel(g.place)],
  ];
  if (g.source) rows.push([t("cw_games_info_source"), g.source, true]);
  rows.push([t("cw_games_info_last_played"), lastPlayedText(g.lastPlayed)]);
  let note: string | null = null;
  if (otherRunning) note = t("cw_games_note_other_running");
  else if (run) note = t("cw_games_note_running");
  return (
    <Modal label={g.name} className="cw-sheet cw-game-sheet" onBack={p.onCloseSheet}>
      <div className="cw-game-sheet-head">
        <Cover titleId={g.titleId} big />
        <div className="cw-game-text">
          <h2 className="cw-sheet-title">{g.name}</h2>
          <Tags g={g} p={p} />
        </div>
      </div>
      <div className="cw-game-sheet-body">
        <div>
          <dl className="cw-info">
            {rows.map(([k, v, mono]) => (
              <div key={k}>
                <dt className="cw-dim">{k}</dt>
                <dd className={mono ? "cw-mono" : undefined}>{v}</dd>
              </div>
            ))}
          </dl>
          {note ? <p className="cw-dim">{note}</p> : null}
        </div>
        <div className="cw-sheet-items">
          <button
            type="button"
            className="cw-focusable cw-sheet-item"
            data-game-action="launch"
            data-nav-default=""
            disabled={otherRunning || starting || removing}
            onClick={() => p.onLaunch(g)}
          >
            <Play aria-hidden />
            <span>{run ? t("cw_games_resume") : t("cw_games_launch")}</span>
          </button>
          {run ? (
            <button
              type="button"
              className="cw-focusable cw-sheet-item is-danger"
              data-game-action="close"
              onClick={() => p.onAsk({ kind: "close", game: g, run })}
            >
              <Square aria-hidden />
              <span>{t("cw_home_close_game")}</span>
            </button>
          ) : null}
          {p.onSaves ? (
            <button type="button" className="cw-focusable cw-sheet-item" data-game-action="saves" onClick={() => p.onSaves?.(g)}>
              <ArchiveRestore aria-hidden />
              <span>{t("cw_games_saves")}</span>
            </button>
          ) : null}
          {g.hidden !== null ? (
            <button
              type="button"
              className="cw-focusable cw-sheet-item"
              data-game-action="hide"
              disabled={removing}
              onClick={() => p.onToggleHidden(g)}
            >
              {g.hidden ? <Eye aria-hidden /> : <EyeOff aria-hidden />}
              <span>{g.hidden ? t("cw_games_show_icon") : t("cw_games_hide_icon")}</span>
            </button>
          ) : null}
          <button
            type="button"
            className="cw-focusable cw-sheet-item is-danger"
            data-game-action="uninstall"
            disabled={!!run || removing || p.launching === g.titleId}
            onClick={() => p.onAsk({ kind: "uninstall", game: g })}
          >
            <Trash2 aria-hidden />
            <span>{t("cw_games_uninstall")}</span>
          </button>
        </div>
      </div>
      <div className="cw-dialog-buttons">
        <button type="button" className="cw-focusable cw-button" onClick={p.onCloseSheet}>
          <X aria-hidden />
          <span>{t("cw_files_close")}</span>
        </button>
      </div>
      {children}
    </Modal>
  );
}

function askCopy(a: GameAsk): { title: string; body: string; confirm: string } {
  if (a.kind === "close")
    return {
      title: t("cw_home_confirm_close_title", { name: a.game.name }),
      body: t("cw_home_confirm_close_body"),
      confirm: t("cw_home_close_game"),
    };
  return {
    title: t("cw_games_confirm_uninstall_title", { name: a.game.name }),
    body: t(a.game.origin === "registered" ? "cw_games_confirm_uninstall_registered" : "cw_games_confirm_uninstall_pkg"),
    confirm: t("cw_games_uninstall"),
  };
}

/** Games without state: everything comes in through props (tests render it directly). */
export function GamesView(p: GamesViewProps) {
  const runningIds = useMemo(() => new Set(p.running.keys()), [p.running]);
  const shown = useMemo(
    () => (p.data ? sortGames(filterGames(p.data.games, p.query, p.show), runningIds, p.sort) : []),
    [p.data, p.query, p.show, p.sort, runningIds],
  );
  const open = p.openId ? (p.data?.games.find((g) => g.titleId === p.openId) ?? null) : null;
  const copy = p.ask ? askCopy(p.ask) : null;
  // Inside the sheet, which stays mounted: ○ returns to the button that asked, and closing the sheet
  // later to the game's card (useModalFocus remembers each opener; an opener that unmounts is lost).
  const confirm =
    p.ask && copy ? (
      <ConfirmDialog
        title={copy.title}
        body={copy.body}
        confirmLabel={copy.confirm}
        danger
        busy={p.busy}
        error={p.askError}
        onConfirm={p.onConfirm}
        onCancel={p.onDismiss}
      />
    ) : null;

  let body;
  if (!p.data) {
    body = p.loadError ? (
      <div className="cw-msg is-error" role="alert">
        {p.loadError} {t("cw_games_err_load_next")}
      </div>
    ) : (
      <p className="cw-dim">{t("cw_games_loading")}</p>
    );
  } else if (!shown.length) {
    let empty = t("cw_games_none");
    if (p.query.trim()) empty = t("cw_games_no_match", { query: p.query.trim() });
    else if (p.show === "hidden") empty = t("cw_games_none_hidden");
    body = <p className="cw-dim">{empty}</p>;
  } else {
    body = (
      <ul className="cw-games" aria-label={t("cw_games_title")}>
        {shown.map((g) => (
          <li key={g.titleId} className="cw-gcard-item">
            <button
              type="button"
              className={`cw-focusable cw-gcard${p.running.has(g.titleId) ? " is-running" : ""}`}
              data-title-id={g.titleId}
              onClick={() => p.onOpen(g.titleId)}
            >
              <Cover titleId={g.titleId} />
              <span className="cw-gcard-name">{g.name}</span>
              <Tags g={g} p={p} />
            </button>
          </li>
        ))}
      </ul>
    );
  }

  return (
    <div className="cw-games-section">
      <h1 className="cw-h1">
        {t("cw_games_title")}
        {p.data ? <span className="cw-tag">{p.data.games.length}</span> : null}
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
            autoCapitalize="off"
            spellCheck={false}
            data-action="games-search"
            onChange={(e) => p.onQuery(e.target.value)}
          />
        </label>
        <button type="button" className="cw-focusable cw-button" data-action="games-sort" onClick={p.onSort}>
          {p.sort === "recent" ? <Clock aria-hidden /> : <ArrowDownAZ aria-hidden />}
          <span>{p.sort === "recent" ? t("cw_games_sort_recent") : t("cw_games_sort_name")}</span>
        </button>
        {p.data?.hiddenKnown ? (
          <button
            type="button"
            className="cw-focusable cw-button"
            aria-pressed={p.show === "hidden"}
            data-action="games-show"
            onClick={p.onShow}
          >
            <EyeOff aria-hidden />
            <span>{t("cw_games_only_hidden")}</span>
          </button>
        ) : null}
        <button type="button" className="cw-focusable cw-button" data-action="games-refresh" onClick={p.onRefresh}>
          <RefreshCw aria-hidden />
          <span>{t("cw_cheats_refresh")}</span>
        </button>
      </div>
      {p.data?.incomplete ? <p className="cw-dim">{t("cw_games_incomplete")}</p> : null}
      {p.message ? (
        <div className={`cw-msg ${p.message.tone === "error" ? "is-error" : "is-info"}`} role={p.message.tone === "error" ? "alert" : "status"}>
          {p.message.text}
        </div>
      ) : null}
      {body}
      {open ? (
        <GameSheet g={open} p={p}>
          {confirm}
        </GameSheet>
      ) : (
        confirm
      )}
    </div>
  );
}

/* ---- state ---- */

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** The process list's running games; null when it could not be read. */
async function readRunning(): Promise<Map<string, RunningGame> | null> {
  try {
    return runningGames(parseProcesses(await call<unknown>("/api/proc/list")).procs);
  } catch {
    return null;
  }
}

/**
 * Close a game the way the PC app does (InstalledApps handleStop): Sony's app kill by app id, and
 * when that is refused or the game is still there a few seconds later, SIGKILL of its process.
 * Throws the last error when neither worked.
 */
export async function closeGame(
  titleId: string,
  run: RunningGame,
  deps: { call: typeof call; running: () => Promise<Map<string, RunningGame> | null>; wait: (ms: number) => Promise<void> } = {
    call,
    running: readRunning,
    wait: sleep,
  },
): Promise<void> {
  let lastErr: unknown = null;
  if (run.appId) {
    try {
      await deps.call("/api/app/close", { app_id: run.appId });
      for (let waited = 0; waited < CLOSE_WAIT_MS; waited += 1000) {
        await deps.wait(1000);
        const now = await deps.running();
        if (now && !now.has(titleId)) return;
      }
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) throw e;
      lastErr = e;
    }
  }
  const now = await deps.running();
  const cur = now ? now.get(titleId) : run;
  if (now && !cur) return;
  try {
    await deps.call("/api/proc/kill", { pid: (cur ?? run).pid });
  } catch (e) {
    // Gone between the read and the kill: closed after all.
    if (e instanceof ApiError && e.code === "not_running") return;
    throw e ?? lastErr;
  }
}

/** The Games section: the console's installed games, launched, closed, hidden or uninstalled from the controller. */
export function Games({ ctx, switcher }: { ctx: SectionContext; switcher?: ReactNode }) {
  const [data, setData] = useState<Installed | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<SortMode>("recent");
  const [show, setShow] = useState<Show>("all");
  const [running, setRunning] = useState<Map<string, RunningGame>>(() => new Map());
  const [launching, setLaunching] = useState<string | null>(null);
  const [removing, setRemoving] = useState<Set<string>>(() => new Set());
  const [message, setMessage] = useState<Message | null>(null);
  const [openId, setOpenId] = useState<string | null>(null);
  const [ask, setAsk] = useState<GameAsk | null>(null);
  const [busy, setBusy] = useState(false);
  const [askError, setAskError] = useState<string | null>(null);
  const live = useRef(true);
  const root = useRef<HTMLDivElement>(null);
  const { refreshStatus } = ctx;

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  useEffect(() => {
    if (!message) return;
    const id = setTimeout(() => setMessage(null), MESSAGE_MS);
    return () => clearTimeout(id);
  }, [message]);

  const load = useCallback(async () => {
    setLoadError(null);
    try {
      const d = await loadInstalled();
      if (live.current) setData(d);
    } catch (e) {
      if (live.current) setLoadError(errorText(e));
    }
  }, []);

  const pollRunning = useCallback(async () => {
    const r = await readRunning();
    if (r && live.current) setRunning(r);
    return r;
  }, []);

  useEffect(() => {
    void load();
  }, [load]);
  useEffect(() => pollWhileVisible(pollRunning, GAMES_POLL_MS), [pollRunning]);

  // Create (refresh): read the list again; App's fallback re-reads the status.
  useEffect(
    () =>
      onNavAction(
        (a) => {
          if (a !== "refresh") return false;
          void load();
          void pollRunning();
          return false;
        },
        () => root.current,
      ),
    [load, pollRunning],
  );

  const onLaunch = useCallback(
    async (g: Game) => {
      if (launching) return;
      setOpenId(null);
      setLaunching(g.titleId);
      setMessage(null);
      try {
        await call("/api/apps/launch", { title_id: g.titleId });
        // The ack means Sony took the request; the game is up once its process shows. Never a
        // second launch meanwhile: a launch into a starting game pushes it to the background.
        const until = Date.now() + LAUNCH_WAIT_MS;
        let up = false;
        while (live.current && Date.now() < until) {
          const r = await pollRunning();
          if (r?.has(g.titleId)) {
            up = true;
            break;
          }
          await sleep(LAUNCH_POLL_MS);
        }
        // Not seen in time is not a failure: the console may still be starting it.
        if (live.current && !up) setMessage({ tone: "info", text: t("cw_games_launch_slow", { name: g.name }) });
        refreshStatus();
      } catch (e) {
        const text = launchErrorText(e);
        if (text && live.current) setMessage({ tone: "error", text });
      } finally {
        if (live.current) setLaunching(null);
      }
    },
    [launching, pollRunning, refreshStatus],
  );

  const onToggleHidden = useCallback(async (g: Game) => {
    if (g.hidden === null) return;
    const hide = !g.hidden;
    try {
      await call("/api/apps/icon", { title_id: g.titleId, hidden: hide });
      if (!live.current) return;
      setData((d) =>
        d ? { ...d, games: d.games.map((x) => (x.titleId === g.titleId ? { ...x, hidden: hide } : x)) } : d,
      );
      // The sheet closes so the line about the home screen shows.
      setOpenId(null);
      setMessage({ tone: "info", text: t(hide ? "cw_games_hidden_done" : "cw_games_shown_done", { name: g.name }) });
    } catch (e) {
      const text = errorText(e);
      if (text && live.current) setMessage({ tone: "error", text });
    }
  }, []);

  /** After an accepted uninstall whose title was still there: check until it goes (never call it failed early). */
  const watchRemoval = useCallback(
    async (g: Game) => {
      setRemoving((s) => new Set(s).add(g.titleId));
      const until = Date.now() + REMOVE_WAIT_MS;
      let gone = false;
      while (live.current && Date.now() < until) {
        await sleep(REMOVE_POLL_MS);
        try {
          if (!(await stillInstalled(g.titleId))) {
            gone = true;
            break;
          }
        } catch {
          // A failed read proves nothing either way: look again.
        }
      }
      if (!live.current) return;
      setRemoving((s) => {
        const n = new Set(s);
        n.delete(g.titleId);
        return n;
      });
      if (gone) {
        setMessage({ tone: "info", text: t("cw_games_uninstalled", { name: g.name }) });
        void load();
      } else {
        setMessage({ tone: "error", text: t("cw_games_still_present", { name: g.name }) });
      }
    },
    [load],
  );

  const onConfirm = useCallback(async () => {
    if (!ask || busy) return;
    setBusy(true);
    setAskError(null);
    try {
      if (ask.kind === "close") {
        await closeGame(ask.game.titleId, ask.run);
        await pollRunning();
        refreshStatus();
        setMessage({ tone: "info", text: t("cw_games_closed", { name: ask.game.name }) });
      } else {
        const r = await call<{ still_present?: boolean } | null>("/api/apps/uninstall", { title_id: ask.game.titleId });
        if (r?.still_present === true) {
          setMessage({ tone: "info", text: t("cw_games_removing_note", { name: ask.game.name }) });
          void watchRemoval(ask.game);
        } else {
          setMessage({ tone: "info", text: t("cw_games_uninstalled", { name: ask.game.name }) });
          setData((d) => (d ? { ...d, games: d.games.filter((x) => x.titleId !== ask.game.titleId) } : d));
          void load();
        }
      }
      setAsk(null);
      setOpenId(null);
    } catch (e) {
      const text = ask.kind === "close" ? errorText(e) : uninstallErrorText(e);
      if (keepsDialogOpen(e)) setAskError(text);
      else {
        setAsk(null);
        if (text) setMessage({ tone: "error", text });
      }
    } finally {
      setBusy(false);
    }
  }, [ask, busy, load, pollRunning, refreshStatus, watchRemoval]);

  return (
    <div ref={root}>
      <GamesView
        data={data}
        loadError={loadError}
        query={query}
        sort={sort}
        show={show}
        running={running}
        foreground={ctx.status?.running?.title_id ?? null}
        launching={launching}
        removing={removing}
        message={message}
        openId={openId}
        ask={ask}
        busy={busy}
        askError={askError}
        switcher={switcher}
        onQuery={setQuery}
        onSort={() => setSort((s) => (s === "recent" ? "name" : "recent"))}
        onShow={() => setShow((s) => (s === "all" ? "hidden" : "all"))}
        onRefresh={() => {
          void load();
          void pollRunning();
        }}
        onOpen={setOpenId}
        onCloseSheet={() => setOpenId(null)}
        onLaunch={(g) => void onLaunch(g)}
        onToggleHidden={(g) => void onToggleHidden(g)}
        onAsk={(a) => {
          setAskError(null);
          setAsk(a);
        }}
        onConfirm={() => void onConfirm()}
        onSaves={ctx.canOpen("saves") ? (g) => ctx.openSection("saves", { title: g.titleId }) : undefined}
        onDismiss={() => {
          if (!busy) {
            setAsk(null);
            setAskError(null);
          }
        }}
      />
    </div>
  );
}
