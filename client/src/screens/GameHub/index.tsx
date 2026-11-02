/**
 * Game Hub (v5 §6.2).
 *
 * Everything about one game behind one URL: `/games/:title_id`.
 *
 * Header: game icon, name, title_id, size, play time.
 * Tabs: Overview · Cheats · Saves · Add-ons · Updates
 *
 * Launching and closing live on the console rows (ConsolesCard), so with
 * several consoles it is always clear which one a game starts or stops on.
 * Each tab fetches its own data when opened.
 */
import { formatDate } from "../../lib/formatDate";
import { useGameLaunch } from "../../lib/useGameLaunch";
import { useStreamLaunch } from "../Play/game/streamLaunch";
import { useCallback, useEffect, useMemo, useState } from "react";
import {
  useLocation,
  useParams,
  useNavigate,
  useSearchParams,
} from "react-router";
import {
  ArrowLeft,
  Gamepad2,
  Info,
  Save,
  Image as ImageIcon,
  Package,
  Download,
  Shield,
  CircleDot,
} from "lucide-react";

import {
  PageHeader,
  Button,
  Tabs,
  Badge,
  EmptyState,
  Card,
  Callout,
  Spinner,
  Toggle,
} from "../../components";
import { GameIcon } from "../../components/GameIcon";
import { useTr } from "../../state/lang";
import {
  useLibraryStore,
  libraryForHost,
  refreshLibrary,
} from "../../state/library";
import { useLibraryScan } from "../../lib/useLibraryScan";
import { shownGamePath } from "./gamePath";
import { useConnectionStore } from "../../state/connection";
import { usePlayTimeStore } from "../../state/playTime";
import { usePkgLibrary, type PkgEntry } from "../../state/pkgLibrary";
import { pushNotification } from "../../state/notifications";
import {
  appsInstalled,
  cheatsGet,
  cheatsToggle,
  profileInfo,
  savesList,
  type InstalledTitle,
  type CheatMod,
  type ProfileUser,
  type SaveEntry,
} from "../../api/ps5";
import { transferAddr, hostOf, mgmtAddr } from "../../lib/addr";
import { useTitleInfo } from "../../lib/useTitleInfo";
import {
  driveOffers,
  gamePath,
  rosterOnly,
  summaryLine,
} from "../../lib/gamePage";
import type { GameView } from "../../api/games";
import { installOffers } from "./collectionInstall";
import { useRosterStore } from "../../state/roster";
import { useGameView } from "./useGameView";
import { installStagedPkg, stagedPkgAction } from "./installStaged";
import { CollectionCover } from "../Collection/CollectionCover";
import { CoverFrame, PlatformTag } from "../../components/GameIconFrame";
import { ConsolesCard } from "./ConsolesCard";
import { DrivesCard } from "./DrivesCard";
import { OnThisPs5Card } from "./OnThisPs5Card";
import { Ps5DrivesCard } from "./Ps5DrivesCard";
import { installedKindOf } from "./installedKind";
import { useSmpState } from "./SmpNeeded";
import { ConvertBuildsCard, NextStep } from "./NextStep";
import { TraceCard } from "./TraceCard";
import {
  playFor,
  useConsolePlayTime,
  useTrackedPlay,
} from "../../lib/trackedPlay";
import { PlayedLine } from "./PlayedLine";
import { CheatAppliedLine } from "../Cheats/CheatAppliedLine";
import { anyWaiting } from "../Cheats/cheatApplied";
import { useWaitingPoll } from "../Cheats/waitingPoll";
import { runningOn, useRunningAppsStore } from "../../state/runningApps";
// Direct import to avoid the barrel's circular-dep warning at build.
import { useConfirm } from "../../components/ConfirmDialog";
import { SaveBackupsList } from "./SaveBackupsList";
import { formatBytes } from "../../lib/format";

const TAB_IDS = ["overview", "cheats", "saves", "addons", "updates"] as const;

type TabId = (typeof TAB_IDS)[number];

export default function GameHubScreen() {
  const { title_id } = useParams<{ title_id: string }>();
  const tr = useTr();
  const navigate = useNavigate();
  const [searchParams, setSearchParams] = useSearchParams();

  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const entries = useLibraryStore((s) => libraryForHost(s, host).entries);
  const mountMap = useLibraryStore((s) => libraryForHost(s, host).mountMap);
  const playTimeState = usePlayTimeStore();
  const [installedTitles, setInstalledTitles] = useState<InstalledTitle[]>([]);
  const gv = useGameView(title_id ?? "");
  const [sendHost, setSendHost] = useState<string | null>(null);
  const profiles = useRosterStore((s) => s.profiles);
  const names = useMemo(
    () =>
      Object.fromEntries(
        profiles.map((p) => [hostOf(p.host), p.name || hostOf(p.host)]),
      ),
    [profiles],
  );
  const connectedHost = payloadStatus === "up" ? hostOf(host ?? "") : "";
  const connectedEntry = gv.view?.consoles.find(
    (c) => c.host === connectedHost,
  );
  // Back to wherever the page was opened from; Games when it was opened directly.
  const goBack = () => {
    const idx = (window.history.state as { idx?: number } | null)?.idx ?? 0;
    if (idx > 0) navigate(-1);
    else navigate("/games");
  };

  // The connected console's installed titles: this game's "On this PS5" card, the name of a
  // game the drives scan doesn't have, and what a backport's library scan may walk.
  const [installedTick, setInstalledTick] = useState(0);
  useEffect(() => {
    setInstalledTitles([]);
  }, [host]);
  useEffect(() => {
    if (!host?.trim() || payloadStatus !== "up") return;
    let cancelled = false;
    appsInstalled(transferAddr(host.trim()))
      .then((res) => {
        if (!cancelled) setInstalledTitles(res.titles);
      })
      .catch(() => {
        // Non-fatal — library entries are the primary source.
      });
    return () => {
      cancelled = true;
    };
  }, [host, payloadStatus, installedTick]);
  // The PS5's drives: this game's folders and images, read here when the grid hasn't lately.
  useLibraryScan();
  const installedHere = useMemo(
    () =>
      title_id
        ? installedTitles.find(
            (t) => t.titleId.toUpperCase() === title_id.toUpperCase(),
          )
        : undefined,
    [installedTitles, title_id],
  );
  // A game from a disk image starts only while ShadowMount+ runs: Play waits for it.
  const disc = !!installedHere && installedKindOf(installedHere) === "disc";
  const smp = useSmpState(disc ? (host ?? "") : "");
  // The Collection's "Backport…" opens the panel on arrival.
  const location = useLocation();
  const handedBackport =
    (location.state as { backport?: boolean } | null)?.backport === true;

  // Determine the active tab from URL ?tab=
  const tabParam = searchParams.get("tab");
  const activeTab: TabId = TAB_IDS.find((t) => t === tabParam) ?? "overview";

  // Find the game in the library or installed apps
  const game = useMemo(() => {
    if (!title_id) return null;
    const app = installedTitles.find((a) => a.titleId === title_id);
    // Check library entries first (games on disk)
    const libEntry = entries?.find((e) => e.titleId === title_id);
    if (libEntry) {
      return {
        titleId: title_id,
        name: libEntry.name,
        // Where the console runs it from, when it is registered (#452).
        path: shownGamePath(libEntry.path, app, entries, mountMap),
        size: libEntry.size,
        source: "library" as const,
      };
    }
    // Check installed apps (fetched inline from the engine)
    if (app) {
      return {
        titleId: title_id,
        name: app.titleName ?? title_id,
        path: shownGamePath(undefined, app, entries, mountMap),
        size: 0,
        source: "installed" as const,
      };
    }
    // On another console, or only on the drives: the engine knows it.
    if (gv.view) {
      return {
        titleId: gv.view.title_id,
        name: gv.view.title,
        path: "",
        size: 0,
        source: "collection" as const,
      };
    }
    return null;
  }, [title_id, entries, mountMap, installedTitles, gv.view]);

  // The helper's tracked play time; the app's own count only when the helper has
  // no tracker to ask. The console's own Play Time record backs it up.
  const tracked = useTrackedPlay(host);
  const { seconds: playSeconds, lastSeenMs } = playFor(
    tracked,
    playTimeState,
    host,
    title_id,
  );
  const consolePlay = useConsolePlayTime(host);

  const tabs = useMemo(
    () =>
      TAB_IDS.map((id) => ({
        id,
        label: tabLabel(id, tr),
        icon: tabIcon(id),
      })),
    [tr],
  );

  // ── Play and Close game ───────────────────────────────────────────
  // One implementation for every place that offers them (lib/useGameLaunch): it closes a running
  // game first with the user's say-so, waits for the title to come up, and diagnoses (and for a
  // folder, repairs) a console that refuses homebrew.
  const launcher = useGameLaunch();
  // Running state comes from the shared store the shell's watcher keeps current.
  const running = useRunningAppsStore(
    (s) => !!title_id && runningOn(s, host).has(title_id),
  );
  const launching = !!title_id && launcher.launchingId === title_id;
  const stopping = !!title_id && launcher.stoppingId === title_id;
  const handleLaunch = () => {
    if (!title_id) return Promise.resolve();
    // Where the console runs it from (a folder or image), for the homebrew repair.
    const source = gv.view?.consoles.find(
      (c) => c.host === connectedHost,
    )?.registered_from;
    return launcher.launch({
      titleId: title_id,
      titleName: game?.name ?? title_id,
      source,
    });
  };
  const handleStop = () =>
    title_id
      ? launcher.stop({ titleId: title_id, titleName: game?.name ?? title_id })
      : Promise.resolve();
  // After an uninstall: what the console has, its row, and its drives, read again.
  const { refresh: refreshConsole } = gv;
  const onConsoleChanged = useCallback(() => {
    setInstalledTick((n) => n + 1);
    if (connectedHost) {
      void refreshConsole(connectedHost).catch(() => {});
      void refreshLibrary(connectedHost);
    }
  }, [connectedHost, refreshConsole]);

  // Play it on Remote Play: the stream starts (waking the console), then the game, on the Play
  // screen. Only for the connected console, once it is paired for Remote Play.
  const streamProfile = profiles.find((p) => hostOf(p.host) === hostOf(connectedHost));
  const canStream = !!(streamProfile?.wake_regist_key && streamProfile?.wake_rp_key);
  const handlePlayStream = () => {
    if (!title_id || !connectedHost) return;
    const source = gv.view?.consoles.find((c) => c.host === connectedHost)?.registered_from;
    useStreamLaunch.getState().request(connectedHost, {
      titleId: title_id,
      titleName: game?.name ?? title_id,
      source,
    });
    navigate("/play");
  };

  if (!title_id) {
    return (
      <div className="p-6">
        <EmptyState
          icon={Gamepad2}
          title={tr("game_hub_not_found", undefined, "Game not found")}
          message={tr(
            "game_hub_not_found_desc",
            undefined,
            "This title ID does not match any game on this computer, a saved server or a PS5.",
          )}
        />
      </div>
    );
  }

  if (!game && gv.loading) {
    return (
      <div className="app-page flex justify-center py-12">
        <Spinner size={20} />
      </div>
    );
  }

  if (!game) {
    const connected = payloadStatus === "up";
    return (
      <div className="p-6">
        <PageHeader
          icon={Gamepad2}
          title={title_id}
          description={tr("game_hub_not_found", undefined, "Game not found")}
          right={
            <Button
              variant="ghost"
              size="sm"
              leftIcon={<ArrowLeft size={16} className="rtl:rotate-180" />}
              onClick={goBack}
            >
              {tr("game_hub_back_games", undefined, "Back")}
            </Button>
          }
        />
        <EmptyState
          icon={Gamepad2}
          title={tr("game_hub_not_found", undefined, "Game not found")}
          message={
            connected
              ? tr(
                  "game_hub_not_found_desc",
                  undefined,
                  "This title ID does not match any game on this computer, a saved server or a PS5.",
                )
              : tr("v5_home_disconnected", undefined, "Not connected")
          }
        />
      </div>
    );
  }

  return (
    <div className="app-page">
      {launcher.dialogs}
      {/* Hero: the framed cover beside a big title and the facts as chips, on
          one glass panel (the reference's greeting panel, laid sideways). */}
      <div className="mb-4">
        <button
          type="button"
          data-testid="game-back"
          onClick={goBack}
          className="inline-flex min-h-9 items-center gap-1.5 rounded-full px-3 text-sm font-medium text-[var(--color-muted)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
        >
          <ArrowLeft size={15} className="rtl:rotate-180" />
          {tr("game_hub_back_games", undefined, "Back")}
        </button>
      </div>
      <header className="glass relative mb-6 overflow-hidden rounded-[var(--radius-panel)] p-5 sm:p-7">
        <div
          aria-hidden
          className="dot-texture pointer-events-none absolute -right-10 -top-10 h-64 w-96"
        />
        <div className="relative flex flex-col gap-5 sm:flex-row sm:items-center sm:gap-7">
          <CoverFrame
            className="w-36 shrink-0 sm:w-44"
            overlay={
              gv.view?.platform ? (
                <span className="absolute left-2 top-2">
                  <PlatformTag platform={gv.view.platform} />
                </span>
              ) : null
            }
          >
            {game.source === "collection" && gv.view?.cover ? (
              // The Collection's cover, with the same fallbacks as its cards (the IPC when the
              // window's own load is refused, then the initials): never a broken image.
              <CollectionCover
                className="h-full"
                game={{
                  game_id: gv.view.title_id,
                  title: game.name,
                  local_cover: gv.view.cover.startsWith("/api/collection/")
                    ? "cover"
                    : undefined,
                  cover_stamp: /[?&]v=([^&]+)/.exec(gv.view.cover)?.[1],
                  cover_url: gv.view.cover.startsWith("http")
                    ? gv.view.cover
                    : undefined,
                  locations: gv.view.copies,
                }}
              />
            ) : (
              <GameIcon
                host={host ?? ""}
                titleId={game.titleId}
                gamePath={game.path}
                alt={game.name}
                size={120}
                rounded=""
                fill
              />
            )}
          </CoverFrame>

          <div className="min-w-0 flex-1">
            <h1 className="text-[1.875rem] leading-[1.08] font-bold tracking-[-0.035em] [overflow-wrap:anywhere] sm:text-[2.75rem]">
              {game.name}
            </h1>
            <div className="mt-4 flex flex-wrap items-center gap-2">
              <FactChip mono>{game.titleId}</FactChip>
              {game.size > 0 && <FactChip>{formatBytes(game.size)}</FactChip>}
              {running && (
                <FactChip
                  tone="good"
                  icon={
                    <CircleDot
                      size={12}
                      className="animate-pulse"
                      aria-hidden
                    />
                  }
                >
                  {tr("installed_badge_playing", undefined, "Playing")}
                </FactChip>
              )}
              {/* Captures are filed by date, not by game, so this opens all of them. */}
              <button
                type="button"
                onClick={() => navigate("/captures")}
                className="inline-flex min-h-8 items-center gap-1.5 rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-3 text-xs font-medium text-[var(--color-text)] shadow-[var(--edge-highlight)] transition-colors hover:bg-[var(--color-surface)]"
              >
                <ImageIcon size={12} aria-hidden />
                {tr("captures", undefined, "Screenshots & clips")}
              </button>
            </div>
            <PlayedLine
              seconds={playSeconds}
              lastSeenMs={lastSeenMs}
              consoleSeconds={consolePlay?.get(game.titleId)}
            />
            {gv.view &&
              (gv.view.consoles.length > 0 || gv.view.copies.length > 0) && (
                <p
                  className="mt-3 text-sm text-[var(--color-muted)]"
                  data-testid="game-summary"
                >
                  {summaryLine(
                    rosterOnly(gv.view, Object.keys(names)),
                    names,
                    tr,
                  )}
                </p>
              )}
            {game.path && (
              <p
                className="mt-2 truncate font-mono text-xs text-[var(--color-muted)]"
                title={game.path}
                data-testid="game-path"
              >
                {game.path}
              </p>
            )}
          </div>
        </div>
      </header>

      {/* Tabs */}
      <Tabs
        tabs={tabs}
        value={activeTab}
        onChange={(id) => setSearchParams({ tab: id })}
        variant="underline"
        ariaLabel={tr("game_hub_overview", undefined, "Game tabs")}
        className="mb-6"
      />

      {activeTab === "overview" && (
        // One column on a phone; on a wide window the console side (its rows, what the
        // connected PS5 has, its drives) beside this computer's side (copies, builds, traces).
        <div className="mb-4 grid gap-4 xl:grid-cols-2 xl:items-start">
          <div className="xl:col-span-2 empty:hidden">
            <NextStep
              view={gv.view}
              connectedHost={connectedHost}
              consoleName={names[connectedHost] ?? connectedHost}
              consoles={profiles.length}
              onSend={() => setSendHost(connectedHost || null)}
            />
          </div>
          <div className="grid min-w-0 gap-4">
            <ConsolesCard
              titleId={game.titleId}
              consoles={profiles.map((p) => ({
                host: hostOf(p.host),
                name: p.name || hostOf(p.host),
              }))}
              connected={connectedHost}
              view={gv.view}
              refresh={gv.refresh}
              onPlay={() => void handleLaunch()}
              onPlayStream={canStream ? handlePlayStream : undefined}
              launching={launching}
              running={running}
              onStop={() => void handleStop()}
              stopping={stopping}
              sendHost={sendHost}
              setSendHost={setSendHost}
              playBlocked={
                disc && smp.absent
                  ? tr(
                      "installed_play_needs_smp",
                      undefined,
                      "Needs ShadowMount+ running to mount and launch",
                    )
                  : undefined
              }
            />
            {connectedHost && installedHere && (
              <OnThisPs5Card
                host={connectedHost}
                consoleName={names[connectedHost] ?? connectedHost}
                title={installedHere}
                allTitles={installedTitles}
                running={running}
                openBackport={handedBackport}
                onLaunch={() => void handleLaunch()}
                onChanged={onConsoleChanged}
              />
            )}
            {connectedHost && (
              <Ps5DrivesCard host={connectedHost} titleId={game.titleId} />
            )}
          </div>
          <div className="grid min-w-0 gap-4">
            <DrivesCard
              view={gv.view}
              connectedHost={connectedHost}
              onSend={() => setSendHost(connectedHost || null)}
              onChanged={() => void gv.reload()}
            />
            <ConvertBuildsCard view={gv.view} connectedHost={connectedHost} />
            {/* Measuring needs the game as a folder on the connected console (an image is
                read-only, and the journal is written into the game's folder). */}
            {(() => {
              const folder =
                connectedEntry?.installed && connectedEntry.registered_from
                  ? connectedEntry.registered_from
                  : game.source === "installed" && game.path
                    ? game.path
                    : null;
              const isFolder =
                !!folder &&
                /^\//.test(folder) &&
                !/\.(exfat|ffpkg|ffpfs|ffpfsc)$/i.test(folder);
              return (
                <TraceCard
                  titleId={game.titleId}
                  host={connectedHost && isFolder ? connectedHost : null}
                  gamePath={isFolder ? folder : null}
                  consoleName={names[connectedHost] ?? connectedHost}
                  running={running}
                />
              );
            })()}
          </div>
        </div>
      )}

      {/* The other tabs are about the connected console: say which, and when it lacks the game. */}
      {activeTab !== "overview" &&
        activeTab !== "updates" &&
        activeTab !== "addons" &&
        connectedHost && (
          <div
            className="mb-3 text-xs text-[var(--color-muted)]"
            data-testid="game-tab-console"
          >
            {connectedEntry && !connectedEntry.installed
              ? tr(
                  "game_tab_not_on",
                  { name: names[connectedHost] ?? connectedHost },
                  "Not on {name}",
                )
              : tr(
                  "game_tab_on",
                  { name: names[connectedHost] ?? connectedHost },
                  "On {name}",
                )}
          </div>
        )}

      {/* Tab content */}
      <GameTabContent view={gv.view} game={game} host={host} tab={activeTab} />
    </div>
  );
}

/** A fact about the game as a quiet pill (title id, size, play time). */
function FactChip({
  children,
  icon,
  mono = false,
  tone,
}: {
  children: React.ReactNode;
  icon?: React.ReactNode;
  mono?: boolean;
  tone?: "good";
}) {
  return (
    <span
      className={`inline-flex min-h-8 items-center gap-1.5 rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-3 text-xs font-medium shadow-[var(--edge-highlight)] ${
        mono ? "font-mono" : ""
      } ${tone === "good" ? "text-[var(--color-good)]" : "text-[var(--color-text)]"}`}
    >
      {icon}
      {children}
    </span>
  );
}

/** Render the content for the active tab. */
function GameTabContent({
  tab,
  view,
  game,
  host,
}: {
  tab: TabId;
  view: GameView | null;
  game: GameInfo;
  host: string | null;
}) {
  switch (tab) {
    case "overview":
      // The consoles and drives cards above are the overview; the hero has the facts.
      return null;
    case "cheats":
      return <CheatsTab titleId={game.titleId} host={host} />;
    case "saves":
      return <SavesTab titleId={game.titleId} host={host} />;
    case "addons":
      return (
        <PackagesTab
          titleId={game.titleId}
          title={game.name}
          host={host}
          view={view}
          kind="addons"
        />
      );
    case "updates":
      return (
        <PackagesTab
          titleId={game.titleId}
          title={game.name}
          host={host}
          view={view}
          kind="updates"
        />
      );
    default:
      return null;
  }
}

/**
 * Shared fetch-on-mount shell for the tabs that pull from the console.
 *
 * Every remote tab needs the same three-state render (loading / error /
 * data) plus the same "not connected" short-circuit, so it lives here
 * once instead of five times. `deps` re-runs the fetch the same way
 * `useEffect` deps do.
 */
function useTabFetch<T>(
  host: string | null,
  fetcher: (addr: string) => Promise<T>,
  deps: unknown[],
): {
  data: T | null;
  loading: boolean;
  error: string | null;
  setData: (data: T) => void;
} {
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const [data, setData] = useState<T | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!host?.trim() || payloadStatus !== "up") {
      setData(null);
      setLoading(false);
      setError(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);
    fetcher(transferAddr(host.trim()))
      .then((res) => {
        if (!cancelled) setData(res);
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
    // `fetcher` is intentionally excluded — callers pass an inline closure,
    // so including it would refetch on every render. `deps` is the contract.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host, payloadStatus, ...deps]);

  return { data, loading, error, setData };
}

/** Standard card wrapper for a fetched tab: title, spinner, error, body. */
function TabCard({
  icon: Icon,
  title,
  loading,
  error,
  children,
}: {
  icon: typeof Info;
  title: string;
  loading?: boolean;
  error?: string | null;
  children: React.ReactNode;
}) {
  const tr = useTr();
  return (
    <Card>
      <h2 className="mb-4 flex items-center gap-3 text-[1.0625rem] font-semibold tracking-[-0.01em]">
        <span className="grid h-8 w-8 shrink-0 place-items-center rounded-full bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)]">
          <Icon size={15} aria-hidden />
        </span>
        {title}
        {loading && <Spinner size={14} />}
      </h2>
      {error ? (
        <Callout
          tone="error"
          title={tr("game_hub_error_title", undefined, "Error")}
        >
          {error}
        </Callout>
      ) : loading ? (
        <p className="text-sm text-[var(--color-muted)]">
          {tr("loading", undefined, "Loading…")}
        </p>
      ) : (
        children
      )}
    </Card>
  );
}

/** Shown by remote tabs when there's no live console to query. */
function NotConnectedNote() {
  const tr = useTr();
  return (
    <p className="text-sm text-[var(--color-muted)]">
      {tr(
        "game_hub_needs_connection",
        undefined,
        "Connect to a PS5 to load this.",
      )}
    </p>
  );
}

/** Cheats tab — the cheat mods this title has, each individually toggleable. */
function CheatsTab({
  titleId,
  host,
}: {
  titleId: string;
  host: string | null;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const connected = !!host?.trim() && payloadStatus === "up";
  const { data, loading, error, setData } = useTabFetch(
    host,
    (addr) => cheatsGet(titleId, addr),
    [titleId],
  );

  // Local echo of each toggle so the switch responds immediately; the
  // console is the source of truth on the next load.
  const [overrides, setOverrides] = useState<Record<number, boolean>>({});
  useEffect(() => setOverrides({}), [titleId, data]);
  const [busyIndex, setBusyIndex] = useState<number | null>(null);
  const [toggleError, setToggleError] = useState<string | null>(null);

  const onToggle = async (mod: CheatMod, next: boolean) => {
    if (!host?.trim()) return;
    setBusyIndex(mod.index);
    setToggleError(null);
    setOverrides((o) => ({ ...o, [mod.index]: next }));
    try {
      const res = await cheatsToggle(
        titleId,
        mod.index,
        next,
        transferAddr(host.trim()),
      );
      if (!res.ok) throw new Error(res.err || "Toggle rejected by the console");
    } catch (e) {
      // Roll the switch back — the console didn't accept it — then read what the console
      // has: an off that went through but could not be saved is still off in the game.
      setOverrides((o) => ({ ...o, [mod.index]: !next }));
      setToggleError(e instanceof Error ? e.message : String(e));
      void cheatsGet(titleId, transferAddr(host.trim()))
        .then(setData)
        .catch(() => {});
    } finally {
      setBusyIndex(null);
    }
  };

  const mods = data?.mods ?? [];
  // A cheat waiting for the game to load (#627): read the list again quietly until it applies,
  // as the Cheats screen does. Statuses come only for the game running now.
  const pollHost = connected && host ? transferAddr(host.trim()) : "";
  useWaitingPoll(
    !!pollHost && anyWaiting(mods),
    `${titleId}@${pollHost}`,
    () => cheatsGet(titleId, pollHost),
    setData,
    () => busyIndex !== null,
  );
  return (
    <TabCard
      icon={Shield}
      title={tr("game_hub_cheats", undefined, "Cheats")}
      loading={loading}
      error={error ?? data?.error ?? null}
    >
      {!connected ? (
        <NotConnectedNote />
      ) : mods.length === 0 ? (
        // Nothing on the console yet: say so and take them to where cheats are downloaded,
        // with this game already selected there.
        <div className="flex flex-col items-start gap-3">
          <p className="text-sm text-[var(--color-muted)]">
            {tr(
              "game_hub_no_cheats_v2",
              undefined,
              "No cheats for this game on the PS5 yet. Download them in Cheats, then turn them on here.",
            )}
          </p>
          <Button
            size="sm"
            variant="primary"
            leftIcon={<Shield size={13} />}
            onClick={() => navigate("/cheats", { state: { titleId } })}
          >
            {tr("game_hub_get_cheats", undefined, "Find cheats for this game")}
          </Button>
        </div>
      ) : (
        <>
          {toggleError && (
            <Callout
              tone="error"
              title={tr(
                "game_hub_cheat_toggle_failed",
                undefined,
                "Couldn’t change that cheat",
              )}
              className="mb-3"
              onDismiss={() => setToggleError(null)}
            >
              {toggleError}
            </Callout>
          )}
          <ul className="divide-y divide-[var(--color-border)]">
            {mods.map((mod) => (
              <li key={mod.index} className="py-2">
                <Toggle
                  checked={overrides[mod.index] ?? mod.on}
                  disabled={busyIndex === mod.index}
                  onChange={(next) => onToggle(mod, next)}
                  label={mod.name}
                  hint={mod.desc || undefined}
                />
                {/* What happened in the running game (#627); a switch flipped here waits for the next read. */}
                {overrides[mod.index] === undefined && <CheatAppliedLine mod={mod} className="mt-0.5" />}
              </li>
            ))}
          </ul>
        </>
      )}
    </TabCard>
  );
}

/** Saves tab — this title's save folders across every user account. */
function SavesTab({ titleId, host }: { titleId: string; host: string | null }) {
  const tr = useTr();
  const navigate = useNavigate();
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const connected = !!host?.trim() && payloadStatus === "up";
  // user_id=0 lists every user's saves; we filter to this title client-side.
  const { data, loading, error } = useTabFetch(
    host,
    (addr) => savesList(addr, 0),
    [],
  );

  const saves = useMemo(
    () => (data?.saves ?? []).filter((s: SaveEntry) => s.title_id === titleId),
    [data, titleId],
  );
  // The console's users, for Restore to another user (#612). Names are a nicety.
  const [users, setUsers] = useState<ProfileUser[]>([]);
  useEffect(() => {
    if (!connected || !host) return;
    let live = true;
    profileInfo(mgmtAddr(host))
      .then((p) => live && setUsers(p.users ?? []))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [connected, host]);
  const gameName = useTitleInfo(titleId)?.title ?? null;

  return (
    <TabCard
      icon={Save}
      title={tr("game_hub_saves", undefined, "Saves")}
      loading={loading}
      error={error}
    >
      {!connected ? (
        <NotConnectedNote />
      ) : saves.length === 0 ? (
        <p className="text-sm text-[var(--color-muted)]">
          {tr(
            "game_hub_no_saves",
            undefined,
            "No save data found for this title.",
          )}
        </p>
      ) : (
        <>
          <ul className="divide-y divide-[var(--color-border)]">
            {saves.map((s) => (
              <li
                key={s.path}
                className="flex items-center justify-between gap-4 py-2"
              >
                <div className="min-w-0">
                  <div className="truncate font-mono text-xs" title={s.path}>
                    {s.path}
                  </div>
                  <div className="mt-0.5 flex items-center gap-2 text-xs text-[var(--color-muted)]">
                    <Badge tone="neutral" variant="soft">
                      {s.kind === "ps4" ? "PS4" : "PS5"}
                    </Badge>
                    <span>
                      {tr("game_hub_save_user", undefined, "User")} {s.user_id}
                    </span>
                    {s.size > 0 && <span>· {formatBytes(s.size)}</span>}
                    {s.mtime > 0 && (
                      <span>· {formatDate(new Date(s.mtime * 1000))}</span>
                    )}
                  </div>
                  {host && (
                    <SaveBackupsList
                      host={host}
                      save={s}
                      users={users}
                      saves={data?.saves ?? []}
                      game={gameName ?? undefined}
                    />
                  )}
                </div>
              </li>
            ))}
          </ul>
          <div className="mt-4">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => navigate("/saves", { state: { titleId } })}
            >
              {tr(
                "game_hub_manage_saves",
                undefined,
                "Back up / restore in Saves",
              )}
            </Button>
          </div>
        </>
      )}
    </TabCard>
  );
}

/**
 * Add-ons / Updates tab — staged packages for this title, split by PARAM.SFO
 * CATEGORY. `ac` is DLC, `gp` is an update/patch. Both come from the same
 * per-host package library store, so one component serves both tabs.
 */
function PackagesTab({
  titleId,
  title,
  host,
  view,
  kind,
}: {
  titleId: string;
  title: string;
  host: string | null;
  view: GameView | null;
  kind: "addons" | "updates";
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const connected = !!host?.trim() && payloadStatus === "up";
  const wantCategory = kind === "addons" ? "ac" : "gp";
  const entries = usePkgLibrary(host ?? "", (s) => s.entries);

  const matching = useMemo(
    () =>
      (entries ?? []).filter(
        (e: PkgEntry) => e.titleId === titleId && e.category === wantCategory,
      ),
    [entries, titleId, wantCategory],
  );

  const isAddons = kind === "addons";
  const { confirm, dialog } = useConfirm();
  // Unknown (no read of this console yet) counts as installed: the question
  // is a warning, not a gate, and asking it on a guess would be noise.
  const baseInstalled =
    view?.consoles.find((c) => c.host === hostOf(host ?? ""))?.installed ??
    true;
  const onInstall = (e: PkgEntry) => {
    if (!host?.trim()) return;
    const what = isAddons
      ? tr("pkglib.addon.dlc", undefined, "DLC")
      : tr("pkglib.addon.update", undefined, "update");
    void installStagedPkg({
      host,
      entry: e,
      baseInstalled,
      confirmWithoutBase: () =>
        confirm({
          title: tr(
            "pkglib.baseMissing.title",
            undefined,
            "Base game isn't installed",
          ),
          message:
            e.platform === "ps5"
              ? tr(
                  "pkglib.baseMissing.bodyPs5",
                  { kind: what, id: titleId },
                  `This ${what} is for ${titleId}, but its base game isn't installed on the PS5. Some PS5 updates carry the whole app and install on their own; if this one can't, the PS5 will say so.`,
                )
              : tr(
                  "pkglib.baseMissing.body",
                  { kind: what, id: titleId },
                  `This ${what} is for ${titleId}, but its base game isn't installed on the PS5. Sony's installer will accept it, but nothing installs until the base game is on the console — install the base first.`,
                ),
          confirmLabel: tr(
            "pkglib.baseMissing.installAnyway",
            undefined,
            "Install anyway",
          ),
        }),
    });
  };
  return (
    <TabCard
      icon={isAddons ? Package : Download}
      title={
        isAddons
          ? tr("game_hub_addons", undefined, "Add-ons")
          : tr("game_hub_updates", undefined, "Updates")
      }
    >
      {dialog}
      {!connected ? (
        <NotConnectedNote />
      ) : matching.length === 0 ? (
        <p className="text-sm text-[var(--color-muted)]">
          {isAddons
            ? tr(
                "game_hub_no_addons_v2",
                undefined,
                "No DLC packages staged for this title. Send one from Upload & Install.",
              )
            : tr(
                "game_hub_no_updates_v2",
                undefined,
                "No update packages staged for this title. Send one from Upload & Install.",
              )}
        </p>
      ) : (
        <ul className="divide-y divide-[var(--color-border)]">
          {matching.map((e) => (
            <li
              key={e.path}
              className="flex items-center justify-between gap-4 py-2"
            >
              <div className="min-w-0">
                <div className="truncate text-sm font-medium">
                  {e.originalName || e.title || e.name}
                </div>
                <div className="mt-0.5 flex items-center gap-2 text-xs text-[var(--color-muted)]">
                  {e.appVer && <span className="font-mono">v{e.appVer}</span>}
                  {e.size > 0 && <span>· {formatBytes(e.size)}</span>}
                </div>
              </div>
              <div className="flex shrink-0 items-center gap-2">
                {e.installedHere && (
                  <Badge tone="good" variant="soft">
                    {tr("game_hub_installed", undefined, "Installed")}
                  </Badge>
                )}
                {stagedPkgAction(e) === "busy" ? (
                  <Badge tone="neutral" variant="soft">
                    {e.status === "installing"
                      ? tr("pkglib.installing", undefined, "Installing…")
                      : tr("game_pkg_queued", undefined, "Queued")}
                  </Badge>
                ) : (
                  <Button
                    size="sm"
                    variant={e.installedHere ? "ghost" : "secondary"}
                    leftIcon={<Download size={13} />}
                    onClick={() => onInstall(e)}
                  >
                    {stagedPkgAction(e) === "reinstall"
                      ? tr("pkglib.reinstall", undefined, "Reinstall")
                      : tr("pkglib.install", undefined, "Install")}
                  </Button>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}
      <div className="mt-4">
        <Button variant="ghost" size="sm" onClick={() => navigate("/send")}>
          {tr("game_hub_open_install_v2", undefined, "Open Upload & Install")}
        </Button>
      </div>
      {connected && driveOffers(view, host ?? "", kind).length > 0 && (
        <div
          className="mt-4 border-t border-[var(--color-border)] pt-3"
          data-testid="game-drive-offers"
        >
          <h3 className="mb-2 text-xs font-semibold text-[var(--color-muted)]">
            {tr("game_drive_offers", undefined, "On your drives")}
          </h3>
          <ul className="divide-y divide-[var(--color-border)]">
            {driveOffers(view, host ?? "", kind).map((o) => (
              <li
                key={o.path}
                className="flex items-center justify-between gap-4 py-2"
              >
                <div className="min-w-0">
                  <div className="truncate text-sm font-medium">
                    {o.title || o.name}
                  </div>
                  <div className="mt-0.5 flex items-center gap-2 text-xs text-[var(--color-muted)]">
                    {o.version && (
                      <span className="font-mono">v{o.version}</span>
                    )}
                    {o.size_bytes > 0 && (
                      <span>· {formatBytes(o.size_bytes)}</span>
                    )}
                  </div>
                </div>
                <Button
                  size="sm"
                  variant="secondary"
                  leftIcon={<Download size={13} />}
                  onClick={() => {
                    installOffers(hostOf(host ?? ""), [
                      { game: { title }, offer: o },
                    ]);
                    pushNotification(
                      "info",
                      tr(
                        "collection.queued",
                        { n: 1 },
                        "{n} packages queued for install",
                      ),
                      {
                        link: gamePath(titleId),
                      },
                    );
                  }}
                >
                  {tr("game_install", undefined, "Install")}
                </Button>
              </li>
            ))}
          </ul>
        </div>
      )}
    </TabCard>
  );
}

interface GameInfo {
  titleId: string;
  name: string;
  path: string;
  size: number;
  source: "library" | "installed" | "collection";
}

/** Overview tab — game info, description, play time, last played. */
/** Tab label lookup. */
function tabLabel(
  id: TabId,
  tr: (
    key: string,
    vars?: Record<string, string | number>,
    fallback?: string,
  ) => string,
): string {
  switch (id) {
    case "overview":
      return tr("game_hub_overview", undefined, "Overview");
    case "cheats":
      return tr("game_hub_cheats", undefined, "Cheats");
    case "saves":
      return tr("game_hub_saves", undefined, "Saves");
    case "addons":
      return tr("game_hub_addons", undefined, "Add-ons");
    case "updates":
      return tr("game_hub_updates", undefined, "Updates");
    default:
      return id;
  }
}

/** Tab icon lookup. */
function tabIcon(id: TabId) {
  switch (id) {
    case "overview":
      return Info;
    case "cheats":
      return Shield;
    case "saves":
      return Save;
    case "addons":
      return Package;
    case "updates":
      return Download;
    default:
      return Info;
  }
}
