import { useEffect, useState, type ReactNode } from "react";
import {
  Camera,
  Gamepad2,
  Keyboard,
  Play,
  Search,
  Sparkles,
  Thermometer,
  X,
} from "lucide-react";

import { GameIcon, Spinner, Toggle } from "../../../components";
import { useTr } from "../../../state/lang";
import { useInstalledTitles } from "../../../state/installedTitles";
import { saveBackupApi } from "../../../api/saveBackup";
import { hostOf } from "../../../lib/addr";
import type { InstalledTitle } from "../../../api/ps5";
import { useStreamLaunch } from "./streamLaunch";
import { useStreamGame } from "./useStreamGame";
import { ConsoleTab } from "./ConsoleTab";
import { ControlsTab } from "./ControlsTab";
import { CheatAppliedLine } from "../../Cheats/CheatAppliedLine";

export type GameTab = "game" | "cheats" | "console" | "controls";

/** Presses inside the panel are the panel's: never the pad's, never the stage's. */
const keep = {
  onClick: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onDoubleClick: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onPointerDown: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onPointerUp: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onPointerMove: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onKeyDown: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onKeyUp: (e: { stopPropagation(): void }) => e.stopPropagation(),
};

/** The Game panel over the stream: the running game, its cheats, the console. It reads the
 *  console only while open. */
export function GamePanel({
  host,
  open,
  onClose,
  variant,
}: {
  /** The streamed console. */
  host: string | null;
  open: boolean;
  onClose: () => void;
  /** Over the picture at its right edge, a sheet over its bottom (a phone in full screen), or a
   *  card under the player (a phone's page, where the picture is too small to cover). */
  variant: "side" | "sheet" | "inline";
}) {
  const tr = useTr();
  const [tab, setTab] = useState<GameTab>("game");
  const { game, toggle, setEngine } = useStreamGame(host, open);
  const titles = useInstalledTitles(open && host ? host : undefined);
  const name = (id: string) =>
    titles?.find((t) => t.titleId === id)?.titleName || id;

  // Esc closes the panel (and is not the controller's ○ while it is open).
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, onClose]);

  if (!open) return null;

  const tabs: Array<{ id: GameTab; label: string; icon: ReactNode }> = [
    {
      id: "game",
      label: tr("rp_game_tab_game", undefined, "Game"),
      icon: <Gamepad2 size={14} aria-hidden />,
    },
    {
      id: "cheats",
      label: tr("rp_game_tab_cheats", undefined, "Cheats"),
      icon: <Sparkles size={14} aria-hidden />,
    },
    {
      id: "console",
      label: tr("rp_game_tab_console", undefined, "Console"),
      icon: <Thermometer size={14} aria-hidden />,
    },
    {
      id: "controls",
      label: tr("rp_game_tab_controls", undefined, "Controls"),
      icon: <Keyboard size={14} aria-hidden />,
    },
  ];

  return (
    <div
      role="dialog"
      aria-label={tr("rp_game_title", undefined, "Game")}
      data-testid="rp-game-panel"
      className={`pointer-events-auto flex flex-col overflow-hidden bg-black/85 text-white shadow-2xl ring-1 ring-white/10 ${
        variant === "inline"
          ? "max-h-[60vh] rounded-[var(--radius-card)] bg-neutral-900"
          : variant === "sheet"
            ? "absolute inset-x-0 bottom-0 z-40 max-h-[70%] rounded-t-2xl pb-[env(safe-area-inset-bottom)] backdrop-blur-md"
            : // Under the stream bar, which stays usable (the panel's own button closes it).
              "absolute bottom-0 right-0 top-[calc(max(0.75rem,env(safe-area-inset-top))+3rem)] z-40 w-[min(24rem,100%)] rounded-tl-2xl pr-[env(safe-area-inset-right)] backdrop-blur-md"
      }`}
      {...keep}
    >
      <div className="flex items-center gap-1 px-3 pb-2 pt-2">
        <div
          className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto"
          role="tablist"
          aria-label={tr("rp_game_title", undefined, "Game")}
        >
          {tabs.map((t) => (
            <button
              key={t.id}
              type="button"
              role="tab"
              aria-selected={tab === t.id}
              onClick={() => setTab(t.id)}
              className={`flex h-8 shrink-0 items-center gap-1.5 rounded-full px-3 text-xs font-medium transition-colors ${
                tab === t.id ? "bg-white/20" : "hover:bg-white/10"
              }`}
              data-testid={`rp-game-tab-${t.id}`}
            >
              {t.icon}
              {t.label}
            </button>
          ))}
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label={tr("rp_phone_close", undefined, "Close")}
          className="flex h-8 w-8 items-center justify-center rounded-full hover:bg-white/15"
          data-testid="rp-game-close"
        >
          <X size={16} />
        </button>
      </div>
      <div
        className="min-h-0 flex-1 overflow-y-auto px-4 pb-4 text-sm"
        role="tabpanel"
      >
        {game.error && !game.titleId ? (
          <p className="text-amber-300" data-testid="rp-game-error">
            {tr(
              "rp_game_unreachable",
              { error: game.error },
              `Can't reach the helper on this PS5: ${game.error}`,
            )}
          </p>
        ) : !game.ready ? (
          <div className="flex justify-center py-6">
            <Spinner size={18} />
          </div>
        ) : tab === "game" ? (
          <GameTabBody
            host={host}
            titleId={game.titleId}
            seen={game.seen}
            name={name}
            titles={titles}
          />
        ) : tab === "cheats" ? (
          <CheatsTabBody
            game={game}
            name={name}
            toggle={toggle}
            setEngine={setEngine}
          />
        ) : tab === "console" ? (
          <ConsoleTab host={host} />
        ) : (
          <ControlsTab name={name} />
        )}
      </div>
    </div>
  );
}

function NoGame({
  seen,
  name,
}: {
  seen: string | null;
  name: (id: string) => string;
}) {
  const tr = useTr();
  return (
    <p className="py-4 text-white/70" data-testid="rp-game-none">
      {seen
        ? tr("rp_game_closed", { name: name(seen) }, `${name(seen)} closed.`)
        : tr("cw_cheats_no_game", undefined, "No game is running.")}
    </p>
  );
}

function GameTabBody({
  host,
  titleId,
  seen,
  name,
  titles,
}: {
  host: string | null;
  titleId: string | null;
  seen: string | null;
  name: (id: string) => string;
  titles: InstalledTitle[] | null;
}) {
  const picker = host ? (
    <PlayOther host={host} titles={titles} running={titleId} />
  ) : null;
  if (!titleId)
    return (
      <div className="space-y-3">
        <NoGame seen={seen} name={name} />
        {picker}
      </div>
    );
  return (
    <div className="space-y-4 py-2">
      <div className="flex items-center gap-3" data-testid="rp-game-now">
        {host && (
          <GameIcon
            host={host}
            titleId={titleId}
            size={56}
            rounded="rounded-xl"
          />
        )}
        <div className="min-w-0">
          <div className="truncate font-semibold">{name(titleId)}</div>
          <div className="font-mono text-xs text-white/60">{titleId}</div>
        </div>
      </div>
      {host && <SnapshotSave host={host} titleId={titleId} />}
      {picker}
    </div>
  );
}

/** Play something else: the console's games, started on this stream (closing the running one
 *  first, with your say-so). */
function PlayOther({
  host,
  titles,
  running,
}: {
  host: string;
  titles: InstalledTitle[] | null;
  running: string | null;
}) {
  const tr = useTr();
  const request = useStreamLaunch((s) => s.request);
  const busy = useStreamLaunch(
    (s) => !!s.pending && s.pending.stage !== "failed",
  );
  const [q, setQ] = useState("");
  const games = (titles ?? [])
    .filter((t) => !t.system && t.titleId !== running)
    .filter((t) => {
      const k = q.trim().toLowerCase();
      return (
        !k ||
        t.titleName.toLowerCase().includes(k) ||
        t.titleId.toLowerCase().includes(k)
      );
    })
    .sort((a, b) => a.titleName.localeCompare(b.titleName));
  return (
    <section className="space-y-2" data-testid="rp-game-play-other">
      <h3 className="text-xs font-semibold uppercase tracking-wide text-white/60">
        {running
          ? tr("rp_game_play_other", undefined, "Play something else")
          : tr("rp_game_play_one", undefined, "Play a game")}
      </h3>
      <label className="flex h-8 items-center gap-2 rounded-full bg-white/10 px-3">
        <Search size={13} className="shrink-0 text-white/60" aria-hidden />
        <input
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder={tr(
            "rp_game_search",
            undefined,
            "Search this PS5's games",
          )}
          className="min-w-0 flex-1 bg-transparent text-xs text-white placeholder:text-white/50 focus:outline-none"
          data-testid="rp-game-search"
        />
      </label>
      {titles === null ? (
        <div className="flex justify-center py-3">
          <Spinner size={14} />
        </div>
      ) : (
        <ul className="max-h-64 space-y-0.5 overflow-y-auto">
          {games.map((t) => (
            <li key={t.titleId}>
              <button
                type="button"
                disabled={busy}
                onClick={() =>
                  request(host, {
                    titleId: t.titleId,
                    titleName: t.titleName,
                    source: t.source || undefined,
                  })
                }
                className="flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-start hover:bg-white/10 disabled:opacity-50"
                data-testid="rp-game-play-title"
              >
                <GameIcon
                  host={host}
                  titleId={t.titleId}
                  size={28}
                  rounded="rounded-md"
                />
                <span className="min-w-0 flex-1 truncate text-sm">
                  {t.titleName}
                </span>
                <Play
                  size={13}
                  className="shrink-0 text-white/60"
                  aria-hidden
                />
              </button>
            </li>
          ))}
          {games.length === 0 && (
            <li className="px-2 py-2 text-xs text-white/60">
              {tr("rp_game_search_none", undefined, "No game matches.")}
            </li>
          )}
        </ul>
      )}
    </section>
  );
}

/** Snapshot save now: a backup of the running game's save, taken while it plays. */
function SnapshotSave({ host, titleId }: { host: string; titleId: string }) {
  const tr = useTr();
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(
    null,
  );
  useEffect(() => setResult(null), [titleId]);
  const take = async () => {
    setBusy(true);
    setResult(null);
    try {
      await saveBackupApi.snapshot({ addr: hostOf(host), title_id: titleId });
      setResult({
        ok: true,
        text: tr(
          "rp_game_snap_done",
          undefined,
          'Save backed up. It is with the game\'s save backups, marked "Taken while playing".',
        ),
      });
    } catch (e) {
      setResult({
        ok: false,
        text: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className="space-y-2">
      <button
        type="button"
        disabled={busy}
        onClick={() => void take()}
        className="flex h-9 w-full items-center justify-center gap-2 rounded-full bg-white/10 text-sm font-medium hover:bg-white/20 disabled:opacity-60"
        data-testid="rp-game-snapshot"
      >
        {busy ? <Spinner size={14} /> : <Camera size={14} aria-hidden />}
        {busy
          ? tr("rp_game_snap_busy", undefined, "Backing up the save…")
          : tr("rp_game_snap", undefined, "Snapshot save now")}
      </button>
      <p className="text-xs text-white/60">
        {tr(
          "rp_game_snap_hint",
          undefined,
          "Backs up this game's save while you play. It waits for a moment when the game is not saving, so it may take a few seconds.",
        )}
      </p>
      {result && (
        <p
          className={`text-xs ${result.ok ? "text-emerald-300" : "text-amber-300"}`}
          role={result.ok ? "status" : "alert"}
          data-testid="rp-game-snapshot-result"
        >
          {result.text}
        </p>
      )}
    </section>
  );
}

function CheatsTabBody({
  game,
  name,
  toggle,
  setEngine,
}: {
  game: ReturnType<typeof useStreamGame>["game"];
  name: (id: string) => string;
  toggle: ReturnType<typeof useStreamGame>["toggle"];
  setEngine: ReturnType<typeof useStreamGame>["setEngine"];
}) {
  const tr = useTr();
  if (!game.titleId) return <NoGame seen={game.seen} name={name} />;
  return (
    <div className="space-y-3 py-1" data-testid="rp-game-cheats">
      <div className="flex items-center gap-2">
        <span className="flex-1 text-xs text-white/70">
          {game.engine
            ? tr("cheats_engine_on", undefined, "Cheat engine is on")
            : tr("cheats_engine_off", undefined, "Cheat engine is off")}
        </span>
        <button
          type="button"
          disabled={game.engineBusy}
          onClick={() => void setEngine(!game.engine)}
          className="h-7 rounded-full bg-white/10 px-3 text-xs hover:bg-white/20 disabled:opacity-50"
          data-testid="rp-game-cheats-engine"
        >
          {game.engine
            ? tr("cheats_engine_turn_off", undefined, "Turn off")
            : tr("cheats_engine_turn_on", undefined, "Turn on")}
        </button>
      </div>
      {game.held && (
        <p className="text-xs text-amber-300" data-testid="rp-game-cheats-held">
          {tr(
            "cheats_held",
            undefined,
            "This game's cheats are paused: it closed right after they were applied. Switch one of its cheats to resume.",
          )}
        </p>
      )}
      {game.message && (
        <p className="text-xs text-amber-300" role="alert">
          {game.message}
        </p>
      )}
      {game.mods === null ? (
        game.modsError ? (
          <p className="text-xs text-amber-300">{game.modsError}</p>
        ) : (
          <div className="flex justify-center py-4">
            <Spinner size={16} />
          </div>
        )
      ) : game.mods.length === 0 ? (
        <p className="text-xs text-white/70" data-testid="rp-game-cheats-none">
          {tr(
            "rp_game_no_cheats",
            undefined,
            "No cheats for this game on the PS5. Get them on the Cheats screen; the stream keeps playing.",
          )}
        </p>
      ) : (
        <ul className="grid gap-1">
          {game.mods.map((m) => (
            <li
              key={m.index}
              className={`flex items-center gap-2 rounded-lg px-2 py-1.5 ${m.on ? "bg-white/10" : ""}`}
            >
              <div className="min-w-0 flex-1">
                <Toggle
                  checked={m.on}
                  disabled={game.busy !== null || (!!m.other_version && !m.on)}
                  onChange={() => void toggle(m)}
                  label={m.name || `#${m.index}`}
                  hint={m.desc || undefined}
                />
                {game.engine && !game.held && (
                  <CheatAppliedLine mod={m} className="mt-0.5" />
                )}
              </div>
              {m.other_version && (
                <span className="shrink-0 rounded-full bg-white/10 px-2 py-0.5 text-[11px] text-white/70">
                  {tr("cheats_other_version", undefined, "Other game version")}
                </span>
              )}
              {game.busy === m.index && <Spinner size={14} />}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
