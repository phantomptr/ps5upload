import { useEffect, useRef, useState, type ReactNode } from "react";
import { Gamepad2, Power, RefreshCw, Search } from "lucide-react";
import { useAppCover } from "../home/Home";
import { onNavAction } from "../nav";
import { t } from "../strings";
import { pollWhileVisible } from "../ui/visiblePoll";
import { createFinder, FINDER_CLOSED, savedText, type Finder, type FinderState } from "./finder";
import { FindCheats } from "./FindCheats";
import {
  appliedText,
  cheatsPhase,
  createCheatsController,
  EMPTY_STATE,
  type CheatMod,
  type CheatsController,
  type CheatsState,
} from "./model";

/** The running game is looked for this often while Cheats is open and the page is visible. */
export const CHEATS_POLL_MS = 5_000;

export interface CheatsViewProps {
  state: CheatsState;
  /** The running game's cover (an object URL), or null (the generic icon shows). */
  cover?: string | null;
  onCoverError?: () => void;
  onToggle: (mod: CheatMod) => void;
  onRefresh: () => void;
  /** Turn the helper's cheat engine on or off (absent: no engine button). */
  onEngine?: (on: boolean) => void;
  /** Find cheats online for the running game (absent: no Find button). */
  onFind?: () => void;
  /** A finished download, one line (shown until the section changes). */
  notice?: string | null;
}

/**
 * One cheat: a switch (✕ clicks it, □ switches the focused one), or Apply for a one-shot cheat.
 * While any switch runs the rows are aria-disabled, not disabled: a disabled button drops the
 * focus ring to nowhere, and the controller (model.toggle) ignores presses meanwhile anyway.
 */
function CheatRow({
  mod,
  busy,
  disabled,
  applied,
  onToggle,
}: {
  mod: CheatMod;
  busy: boolean;
  disabled: boolean;
  /** Switched-on cheats are applied now: the engine is on and the game is not paused. */
  applied: boolean;
  onToggle: (m: CheatMod) => void;
}) {
  let tag: string;
  if (busy) tag = t("cw_cheats_switching");
  else if (mod.button) tag = t("cw_cheats_apply");
  // On but not applied (#550): saved for when the engine is on again, never "On".
  else tag = t(mod.on ? (applied ? "cw_cheats_on" : "cw_cheats_saved") : "cw_cheats_off");
  // What actually happened in the game (#627), only while switched-on cheats are being applied.
  const status = applied ? appliedText(mod) : null;
  return (
    <li className="cw-cheat-item">
      <button
        type="button"
        role={mod.button ? undefined : "switch"}
        aria-checked={mod.button ? undefined : mod.on}
        aria-busy={busy || undefined}
        aria-disabled={disabled || undefined}
        className={`cw-focusable cw-button cw-cheat${mod.on && !mod.button ? " is-on" : ""}`}
        data-cheat-index={mod.index}
        onClick={() => onToggle(mod)}
      >
        <span className="cw-cheat-text">
          <span className="cw-cheat-name">{mod.name}</span>
          {mod.desc ? <span className="cw-dim cw-cheat-desc">{mod.desc}</span> : null}
          {mod.otherVersion ? <span className="cw-dim cw-cheat-desc">{t("cw_cheats_other_version")}</span> : null}
          {status ? (
            <span className={`cw-cheat-desc cw-cheat-applied is-${mod.applied}`} data-applied={mod.applied}>
              {status}
            </span>
          ) : null}
        </span>
        <span className={`cw-tag${mod.on && !mod.button ? " is-on" : ""}`}>{tag}</span>
      </button>
    </li>
  );
}

function RefreshButton({ onRefresh }: { onRefresh: () => void }) {
  return (
    <button type="button" className="cw-focusable cw-button" data-action="cheats-refresh" onClick={onRefresh}>
      <RefreshCw aria-hidden />
      <span>{t("cw_cheats_refresh")}</span>
    </button>
  );
}

function FindButton({ onFind, more }: { onFind: () => void; more?: boolean }) {
  return (
    <button type="button" className={`cw-focusable cw-button${more ? "" : " is-primary"}`} data-action="cheats-find" onClick={onFind}>
      <Search aria-hidden />
      <span>{t(more ? "cw_cheats_find_more" : "cw_cheats_find")}</span>
    </button>
  );
}

/** The helper's cheat engine switch: it is remembered across restarts, so it works before a game runs. */
function EngineButton({ on, busy, onEngine }: { on: boolean; busy: boolean; onEngine: (on: boolean) => void }) {
  return (
    <button
      type="button"
      className="cw-focusable cw-button"
      data-action="cheats-engine"
      aria-busy={busy || undefined}
      onClick={() => {
        if (!busy) onEngine(!on);
      }}
    >
      <Power aria-hidden />
      <span>{t(on ? "cw_cheats_engine_turn_off" : "cw_cheats_engine_turn_on")}</span>
    </button>
  );
}

/** An empty state: what is true, what to do next, and Refresh (Find first when given). */
function Empty({
  title,
  next,
  note,
  engine,
  onRefresh,
  onFind,
  tone,
}: {
  title: string;
  next?: string;
  /** One more line (the engine's state). */
  note?: string;
  engine?: ReactNode;
  onRefresh: () => void;
  onFind?: () => void;
  tone?: "error";
}) {
  return (
    <div className={`cw-card cw-cheats-empty${tone === "error" ? " is-error" : ""}`} role={tone === "error" ? "alert" : "status"}>
      <p className="cw-cheats-empty-title">{title}</p>
      {next ? <p className="cw-dim">{next}</p> : null}
      {note ? <p className="cw-dim">{note}</p> : null}
      <div className="cw-cheats-empty-buttons">
        {onFind ? <FindButton onFind={onFind} /> : null}
        {engine}
        <RefreshButton onRefresh={onRefresh} />
      </div>
    </div>
  );
}

/** Cheats without state: everything comes in through props (tests render it directly). */
export function CheatsView(p: CheatsViewProps) {
  const phase = cheatsPhase(p.state);
  const s = p.state;
  const title = phase.kind === "list" || phase.kind === "none" ? phase.title : null;
  const engineOn = s.status?.engine === true;
  const held = s.status?.game?.held === true;
  const engine =
    s.status && p.onEngine ? <EngineButton on={engineOn} busy={s.engineBusy} onEngine={p.onEngine} /> : null;
  const engineNote = s.status ? t(engineOn ? "cw_cheats_engine_on_state" : "cw_cheats_engine_off_state") : undefined;

  let body;
  switch (phase.kind) {
    case "loading":
      body = <p className="cw-dim">{t("cw_cheats_loading")}</p>;
      break;
    case "error":
      body = <Empty title={phase.text} onRefresh={p.onRefresh} tone="error" />;
      break;
    case "no_game":
      body = (
        <Empty
          title={t("cw_cheats_no_game")}
          next={t("cw_cheats_no_game_next")}
          note={engineNote}
          engine={engine}
          onRefresh={p.onRefresh}
        />
      );
      break;
    case "exited":
      body = (
        <Empty
          title={t("cw_cheats_exited")}
          next={t("cw_cheats_exited_next")}
          note={engineNote}
          engine={engine}
          onRefresh={p.onRefresh}
        />
      );
      break;
    case "none":
      body = <Empty title={t("cw_cheats_none")} next={t("cw_cheats_none_next")} onRefresh={p.onRefresh} onFind={p.onFind} />;
      break;
    case "list":
      body = (
        <>
          <ul className="cw-cheats">
            {phase.list.mods.map((m) => (
              <CheatRow
                key={m.index}
                mod={m}
                busy={s.busy === m.index}
                disabled={s.busy !== null}
                applied={engineOn && !held}
                onToggle={p.onToggle}
              />
            ))}
          </ul>
          {phase.list.truncated ? <p className="cw-dim cw-cheats-note">{t("cw_cheats_truncated")}</p> : null}
        </>
      );
      break;
  }

  return (
    <div className="cw-cheats-section">
      <h1 className="cw-h1">{t("cw_cheats_title")}</h1>
      {title ? (
        <section className="cw-card cw-game" aria-label={t("cw_home_now_running")}>
          {p.cover ? (
            <img className="cw-game-cover" src={p.cover} alt="" onError={p.onCoverError} />
          ) : (
            <Gamepad2 className="cw-game-icon" aria-hidden />
          )}
          <div className="cw-game-text">
            <div className="cw-dim">{t("cw_home_now_running")}</div>
            <div className="cw-game-title">{title}</div>
            <div className="cw-dim cw-cheats-how">
              {held
                ? t("cw_cheats_held")
                : s.status && !engineOn
                  ? t("cw_cheats_engine_off_state")
                  : t("cw_cheats_how")}
            </div>
          </div>
          <div className="cw-cheats-game-buttons">
            {engine}
            {phase.kind === "list" && p.onFind ? <FindButton onFind={p.onFind} more /> : null}
            <RefreshButton onRefresh={p.onRefresh} />
          </div>
        </section>
      ) : null}
      {p.notice ? (
        <div className="cw-msg is-info" role="status">
          {p.notice}
        </div>
      ) : null}
      {s.message ? (
        <div className="cw-msg is-error" role="alert">
          {s.message}
        </div>
      ) : null}
      {body}
    </div>
  );
}

/** Does a finder open for `finderTitle` close, with `running` the game the last status read reported
 * (null: none) and `noGameReads` the status reads in a row that reported none? */
export function finderClosesFor(finderTitle: string, running: string | null, noGameReads: number): boolean {
  if (running) return running !== finderTitle;
  return noGameReads >= 2;
}

/** □ on a focused cheat inside `root`: that cheat's index, or null. */
export function focusedCheatIndex(root: Element | null, active: Element | null): number | null {
  if (!root || !active || !root.contains(active)) return null;
  const el = active.closest("[data-cheat-index]");
  if (!el) return null;
  const n = Number(el.getAttribute("data-cheat-index"));
  return Number.isInteger(n) ? n : null;
}

/** The Cheats section: the running game's cheats, each switched on or off from the controller. */
export function Cheats() {
  const [state, setState] = useState<CheatsState>(EMPTY_STATE);
  const ctl = useRef<CheatsController | null>(null);
  const root = useRef<HTMLDivElement>(null);
  const [finderState, setFinderState] = useState<FinderState>(FINDER_CLOSED);
  const finder = useRef<Finder | null>(null);
  const [notice, setNotice] = useState<{ title: string; text: string } | null>(null);

  // The finder: a stored file is read at once (the game's cheats again) and closes the finder that
  // asked for it, never one opened since (another game, or opened again).
  useEffect(() => {
    finder.current = createFinder(setFinderState, ({ entry, title, replaced, current }) => {
      const running = ctl.current?.get().status?.game?.title_id ?? null;
      if (running !== title) return;
      setNotice({ title, text: savedText(entry.filename, replaced) });
      if (current) finder.current?.close();
      void ctl.current?.loadList();
    });
    return () => {
      finder.current?.close();
      finder.current = null;
    };
  }, []);

  // Opening the section reads at once; then the status every few seconds while the page shows.
  // Coming back to the page (it was hidden) reads the cheats again too: the PC may have changed them.
  useEffect(() => {
    const c = createCheatsController(setState);
    ctl.current = c;
    const stop = pollWhileVisible(() => void c.loadStatus(), CHEATS_POLL_MS);
    const onVisible = () => {
      if (!document.hidden) void c.loadList();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      stop();
      document.removeEventListener("visibilitychange", onVisible);
      c.dispose();
      ctl.current = null;
    };
  }, []);

  // Create re-reads (App's fallback still re-reads the console status); □ switches the focused cheat.
  useEffect(
    () =>
      onNavAction(
        (a) => {
          const c = ctl.current;
          if (!c) return false;
          if (a === "refresh") {
            void c.refresh();
            return false;
          }
          if (a === "select") {
            const index = focusedCheatIndex(root.current, document.activeElement);
            if (index == null) return false;
            const mod = c.get().list?.mods.find((m) => m.index === index);
            if (mod) void c.toggle(mod);
            return true;
          }
          return false;
        },
        () => root.current,
      ),
    [],
  );

  const titleId = state.status?.game?.title_id ?? null;
  // The finder goes when its game is confirmed gone: another game reported running, or no game on two
  // status reads in a row. A status not read yet (or a failed read, which keeps the last one) or a
  // single empty read keeps it open.
  const noGameReads = useRef(0);
  const status = state.status;
  useEffect(() => {
    if (!status) return;
    noGameReads.current = status.game ? 0 : noGameReads.current + 1;
  }, [status]);
  useEffect(() => {
    if (finderState.kind === "closed" || !status) return;
    const now = status.game?.title_id ?? null;
    if (finderClosesFor(finderState.title, now, noGameReads.current)) finder.current?.close();
  }, [status, finderState]);
  const loadedCover = useAppCover(titleId);
  const [coverFailed, setCoverFailed] = useState<string | null>(null);
  const cover = loadedCover && coverFailed !== loadedCover ? loadedCover : null;

  return (
    <div ref={root}>
      <CheatsView
        state={state}
        cover={cover}
        onCoverError={() => setCoverFailed(loadedCover)}
        onToggle={(m) => void ctl.current?.toggle(m)}
        onRefresh={() => void ctl.current?.refresh()}
        onEngine={(on) => void ctl.current?.setEngine(on)}
        onFind={
          titleId
            ? () => {
                setNotice(null);
                void finder.current?.open(titleId);
              }
            : undefined
        }
        notice={notice && notice.title === titleId ? notice.text : null}
      />
      <FindCheats
        state={finderState}
        onDownload={(e) => void finder.current?.download(e)}
        onRetry={() => {
          if (finderState.kind !== "closed") void finder.current?.open(finderState.title);
        }}
        onClose={() => finder.current?.close()}
      />
    </div>
  );
}
