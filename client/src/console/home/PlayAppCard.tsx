import { useCallback, useEffect, useRef, useState } from "react";
import { Download, Play, RefreshCw, UserPlus } from "lucide-react";
import { ApiError, call, isOffline } from "../api";
import { t } from "../strings";
import { ConfirmDialog } from "../ui/ConfirmDialog";
import { pollWhileVisible } from "../ui/visiblePoll";

/** PS5Upload Play's title id (payload/include/config.h PLAY_APP_TITLE_ID). */
export const PLAY_APP_TITLE_ID = "FAKE99261";
/** The app's name: a product name, the same in every language. */
const PLAY_APP_NAME = "PS5Upload Play";
/** How often the card reads the state while nothing runs, and while an install runs. */
const IDLE_POLL_MS = 15_000;
const INSTALL_POLL_MS = 1_000;

/** /api/play-app (payload/src/webui_api.c r_play_app). */
export interface PlayAppState {
  installed: { version: string; path: string; registered: boolean } | null;
  staged: { version: string } | null;
  store_managed: boolean;
  store_busy: boolean;
  running: boolean;
  install: {
    state: "idle" | "closing" | "copying" | "registering" | "done" | "failed";
    done: number;
    total: number;
    error?: string;
    /** done: who registers the title ("shadowmount": the console's folder loader does, maybe later). */
    registered_by?: string;
    pending?: boolean;
    register_error?: string;
  };
}

/** "13.000.000" (param.json's fixed form) as people read it: "13.0.0". Anything else as it is. */
export function displayVersion(v: string): string {
  const parts = v.split(".");
  if (parts.length !== 3 || !parts.every((p) => /^\d+$/.test(p))) return v;
  return parts.map((p) => String(Number(p))).join(".");
}

export type PlayAction = "install" | "update" | null;

/** What the card offers for `s`. */
export function playAppView(s: PlayAppState): {
  action: PlayAction;
  canOpen: boolean;
  busy: boolean;
  /** Install/Update goes through a question first (another app installed it). */
  asks: boolean;
} {
  const busy = s.install.state === "closing" || s.install.state === "copying" || s.install.state === "registering";
  let action: PlayAction = null;
  if (s.staged && !s.installed) action = "install";
  else if (s.staged && s.installed && s.staged.version !== s.installed.version) action = "update";
  // The same version, but not on the home screen: installing again registers it.
  else if (s.staged && s.installed && !s.installed.registered) action = "install";
  return {
    action,
    canOpen: !!s.installed && s.installed.registered,
    busy,
    asks: action !== null && s.store_managed,
  };
}

/** A failed install or open as one line (null: nothing to say; the reload banner covers a 401). */
export function playAppErrorText(err: unknown): string | null {
  if (err instanceof ApiError) {
    if (err.status === 401) return null;
    if (isOffline(err)) return t("cw_home_err_offline");
    if (err.code === "store_busy") return t("cw_play_store_busy");
    if (err.code === "other_game_running") return t("cw_games_err_other_running");
    return t("cw_play_failed", { code: err.code });
  }
  return t("cw_play_failed", { code: err instanceof Error ? err.message : String(err) });
}

/** The "a friend has a PS5?" tip: shown until dismissed on this device (integration design: once). */
export const PLAY_TIP_KEY = "ps5upload.minion.playTipSeen";
function tipSeen(): boolean {
  try {
    return localStorage.getItem(PLAY_TIP_KEY) === "1";
  } catch {
    return false;
  }
}
function markTipSeen(): void {
  try {
    localStorage.setItem(PLAY_TIP_KEY, "1");
  } catch {
    /* private mode: shown again next time */
  }
}

function isPlayState(v: unknown): v is PlayAppState {
  return !!v && typeof v === "object" && "install" in v && "installed" in v;
}

/** PS5Upload Play on this console: its versions and Install / Update / Open. Hidden while there
 *  is neither an installed copy nor one PS5Upload left to install. */
export function PlayAppCard() {
  const [state, setState] = useState<PlayAppState | null>(null);
  const [asking, setAsking] = useState(false);
  const [message, setMessage] = useState<{ tone: "error" | "info"; text: string } | null>(null);
  const [pending, setPending] = useState(false);
  const busy = state ? playAppView(state).busy : false;
  // Whether the last answer showed an install running (or one was just started here).
  const watching = useRef(false);

  const load = useCallback(
    () =>
      call<unknown>("/api/play-app").then(
        (r) => {
          if (!isPlayState(r)) return;
          // An install that ended while the card watched says how it went.
          if (watching.current && r.install.state === "done")
            setMessage(
              r.install.register_error
                ? { tone: "error", text: t("cw_play_failed", { code: r.install.register_error }) }
                : { tone: "info", text: t("cw_play_done") },
            );
          if (watching.current && r.install.state === "failed")
            setMessage({ tone: "error", text: t("cw_play_failed", { code: r.install.error ?? "failed" }) });
          watching.current = playAppView(r).busy;
          setState(r);
        },
        () => {},
      ),
    [],
  );

  useEffect(() => pollWhileVisible(load, busy ? INSTALL_POLL_MS : IDLE_POLL_MS), [load, busy]);

  const install = useCallback(
    (confirmStore: boolean) => {
      if (pending) return;
      setPending(true);
      setMessage(null);
      call("/api/play-app/install", confirmStore ? { confirm_store: true } : {}).then(
        () => {
          setPending(false);
          setAsking(false);
          watching.current = true;
          void load();
        },
        (e: unknown) => {
          setPending(false);
          setAsking(false);
          const text = playAppErrorText(e);
          if (text) setMessage({ tone: "error", text });
        },
      );
    },
    [pending, load],
  );

  // Open, or open at its join screen ("Join a friend": the app reads the intent from the helper).
  const open = useCallback((intent?: "join") => {
    setMessage(null);
    call("/api/play-app/open", intent ? { intent } : {}).then(
      () => {},
      (e: unknown) => {
        const text = playAppErrorText(e);
        if (text) setMessage({ tone: "error", text });
      },
    );
  }, []);
  const [tip, setTip] = useState(() => !tipSeen());

  if (!state) return null;
  return (
    <PlayAppCardView
      state={state}
      message={message}
      asking={asking}
      pending={pending}
      onInstall={() => (playAppView(state).asks ? setAsking(true) : install(false))}
      onConfirmStore={() => install(true)}
      onCancelStore={() => setAsking(false)}
      onOpen={() => open()}
      onJoin={() => open("join")}
      tip={tip}
      onDismissTip={() => {
        markTipSeen();
        setTip(false);
      }}
    />
  );
}

export interface PlayAppCardViewProps {
  state: PlayAppState;
  message: { tone: "error" | "info"; text: string } | null;
  /** The "installed from another app" question is open. */
  asking: boolean;
  /** An install request is on its way. */
  pending: boolean;
  onInstall: () => void;
  onConfirmStore: () => void;
  onCancelStore: () => void;
  onOpen: () => void;
  /** Open at the join screen. */
  onJoin?: () => void;
  /** Show the one-time tip (a friend's PS5 can join). */
  tip?: boolean;
  onDismissTip?: () => void;
}

/** The card without state (tests render it directly). */
export function PlayAppCardView({
  state,
  message,
  asking,
  pending,
  onInstall,
  onConfirmStore,
  onCancelStore,
  onOpen,
  onJoin,
  tip,
  onDismissTip,
}: PlayAppCardViewProps) {
  if (!state.installed && !state.staged) return null;
  const view = playAppView(state);
  const pct = state.install.total > 0 ? Math.floor((state.install.done / state.install.total) * 100) : 0;
  let line: string;
  if (view.busy) line = t("cw_play_installing", { pct });
  else if (state.installed && view.action === "update")
    line = t("cw_play_update_ready", { version: displayVersion(state.staged!.version), installed: displayVersion(state.installed.version) });
  else if (state.installed)
    line = state.installed.registered
      ? t("cw_play_installed", { version: displayVersion(state.installed.version) })
      : t("cw_play_not_registered");
  else line = t("cw_play_ready", { version: displayVersion(state.staged!.version) });
  const blocked = state.store_busy || pending;

  return (
    <section className="cw-card cw-game cw-play-app" aria-label={PLAY_APP_NAME} data-play-app>
      <Play className="cw-game-icon" aria-hidden />
      <div className="cw-game-text">
        <div className="cw-game-title">{PLAY_APP_NAME}</div>
        <div className="cw-dim">{line}</div>
        {state.store_busy && view.action ? <div className="cw-dim">{t("cw_play_store_busy")}</div> : null}
        {state.running && view.action && !view.busy ? <div className="cw-dim">{t("cw_play_closes_first")}</div> : null}
        {/* What it is, while it is not here yet: one press installs, opening it pairs by itself. */}
        {!state.installed && !view.busy ? <div className="cw-dim" data-play-guide>{t("cw_play_guide")}</div> : null}
        {tip && view.canOpen && !view.busy ? (
          <div className="cw-dim cw-play-tip" data-play-tip>
            {t("cw_play_tip")}{" "}
            <button type="button" className="cw-focusable cw-button cw-play-tip-ok" data-action="play-tip-ok" onClick={onDismissTip}>
              <span>{t("cw_play_tip_ok")}</span>
            </button>
          </div>
        ) : null}
        {message ? (
          <div className={`cw-msg is-${message.tone}`} role={message.tone === "error" ? "alert" : "status"}>
            {message.text}
          </div>
        ) : null}
      </div>
      {view.action && !view.busy ? (
        <button
          type="button"
          className="cw-focusable cw-button is-primary"
          data-action={`play-app-${view.action}`}
          disabled={blocked}
          onClick={onInstall}
        >
          {view.action === "update" ? <RefreshCw aria-hidden /> : <Download aria-hidden />}
          <span>{t(view.action === "update" ? "cw_play_update" : "cw_play_install")}</span>
        </button>
      ) : null}
      {view.canOpen && !view.busy ? (
        <button type="button" className="cw-focusable cw-button" data-action="play-app-open" onClick={onOpen}>
          <Play aria-hidden />
          <span>{t("cw_play_open")}</span>
        </button>
      ) : null}
      {view.canOpen && !view.busy && onJoin ? (
        <button type="button" className="cw-focusable cw-button" data-action="play-app-join" onClick={onJoin}>
          <UserPlus aria-hidden />
          <span>{t("cw_play_join_friend")}</span>
        </button>
      ) : null}
      {asking ? (
        <ConfirmDialog
          title={t("cw_play_store_title")}
          body={t("cw_play_store_body")}
          confirmLabel={t(view.action === "update" ? "cw_play_update" : "cw_play_install")}
          busy={pending}
          onConfirm={onConfirmStore}
          onCancel={onCancelStore}
        />
      ) : null}
    </section>
  );
}
