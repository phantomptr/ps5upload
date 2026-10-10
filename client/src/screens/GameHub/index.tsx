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
import { useMakeWay } from "../../lib/useMakeWay";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useParams, useNavigate, useSearchParams } from "react-router";
import {
  ArrowLeft,
  Gamepad2,
  Info,
  Save,
  Image as ImageIcon,
  Package,
  Download,
  Shield,
  Clock,
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
import { useLibraryStore, libraryForHost } from "../../state/library";
import { useConnectionStore } from "../../state/connection";
import { usePlayTimeStore } from "../../state/playTime";
import { usePkgLibrary, type PkgEntry } from "../../state/pkgLibrary";
import { pushNotification } from "../../state/notifications";
import {
  appsInstalled,
  appLaunch,
  cheatsGet,
  cheatsToggle,
  savesList,
  type InstalledTitle,
  type CheatMod,
  type SaveEntry,
} from "../../api/ps5";
import { transferAddr, mgmtAddr, hostOf } from "../../lib/addr";
import { driveOffers, gamePath, rosterOnly, summaryLine } from "../../lib/gamePage";
import type { GameView } from "../../api/games";
import { installOffers } from "./collectionInstall";
import { useRosterStore } from "../../state/roster";
import { useGameView } from "./useGameView";
import { installStagedPkg, stagedPkgAction } from "./installStaged";
import { CollectionCover } from "../Collection/CollectionCover";
import { CoverFrame, PlatformTag } from "../../components/GameIconFrame";
import { ConsolesCard } from "./ConsolesCard";
import { DrivesCard } from "./DrivesCard";
import { fetchRunningGames } from "../../lib/runningGames";
import { killGame } from "../../lib/killGame";
import { playFor, useTrackedPlay } from "../../lib/trackedPlay";
import { runningOn, useRunningAppsStore } from "../../state/runningApps";
// Direct import to avoid the barrel's circular-dep warning at build.
import { useConfirm } from "../../components/ConfirmDialog";
import { useStaleHostGuard } from "../../lib/staleHostGuard";
import { formatBytes, formatDuration } from "../../lib/format";

/** How long to keep Play disabled while waiting for the title to appear in
 *  the process list, and how often to re-check. Mirrors the Installed Apps
 *  screen — a cold first launch (just-installed, disc image) legitimately
 *  takes this long, and re-firing a launch at a half-started title is
 *  exactly how it gets killed. */
const LAUNCH_CONFIRM_TIMEOUT_MS = 90_000;
const LAUNCH_CONFIRM_POLL_MS = 2_000;

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
  const playTimeState = usePlayTimeStore();
  const [installedTitles, setInstalledTitles] = useState<InstalledTitle[]>([]);
  const gv = useGameView(title_id ?? "");
  const [sendHost, setSendHost] = useState<string | null>(null);
  const profiles = useRosterStore((s) => s.profiles);
  const names = useMemo(
    () => Object.fromEntries(profiles.map((p) => [hostOf(p.host), p.name || hostOf(p.host)])),
    [profiles],
  );
  const connectedHost = payloadStatus === "up" ? hostOf(host ?? "") : "";
  const connectedEntry = gv.view?.consoles.find((c) => c.host === connectedHost);
  // Back to wherever the page was opened from; Games when it was opened directly.
  const goBack = () => {
    const idx = (window.history.state as { idx?: number } | null)?.idx ?? 0;
    if (idx > 0) navigate(-1);
    else navigate("/games");
  };

  // Fetch installed apps when connected, so we can resolve title_id → name
  // for games not in the library scan (e.g. system apps).
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
  }, [host, payloadStatus]);

  // Determine the active tab from URL ?tab=
  const tabParam = searchParams.get("tab");
  const activeTab: TabId =
    TAB_IDS.find((t) => t === tabParam) ?? "overview";

  // Find the game in the library or installed apps
  const game = useMemo(() => {
    if (!title_id) return null;
    // Check library entries first (games on disk)
    const libEntry = entries?.find((e) => e.titleId === title_id);
    if (libEntry) {
      return {
        titleId: title_id,
        name: libEntry.name,
        path: libEntry.path,
        size: libEntry.size,
        source: "library" as const,
      };
    }
    // Check installed apps (fetched inline from the engine)
    const app = installedTitles.find((a) => a.titleId === title_id);
    if (app) {
      return {
        titleId: title_id,
        name: app.titleName ?? title_id,
        path: app.source || "",
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
  }, [title_id, entries, installedTitles, gv.view]);

  // Same numbers as Game Activity's Tracked tab; the app's own count only when
  // the helper has no tracker to ask.
  const tracked = useTrackedPlay(host);
  const { seconds: playSeconds, lastSeenMs } = playFor(tracked, playTimeState, host, title_id);

  const tabs = useMemo(
    () =>
      TAB_IDS.map((id) => ({
        id,
        label: tabLabel(id, tr),
        icon: tabIcon(id),
      })),
    [tr],
  );

  // ── Launch ────────────────────────────────────────────────────────
  const guard = useStaleHostGuard();
  const [launching, setLaunching] = useState(false);
  const { makeWay, dialog: makeWayDialog } = useMakeWay();

  /**
   * Start this title on the console, then wait for it to actually come up.
   *
   * `appLaunch` returning only means Sony accepted the request — not that
   * the game is running. We hold the disabled/"Starting…" state for the
   * whole come-up window and watch (read-only) for the title to appear in
   * the process list, so the user can't fire a second launch into a title
   * that's still starting. We never act on a starting game.
   */
  const handleLaunch = useCallback(async () => {
    if (!title_id || launching) return;
    const probe = guard.capture();
    if (!probe.host?.trim()) return;

    setLaunching(true);
    try {
      // One game at a time: a running one is closed first, with the user's say-so.
      if (!(await makeWay(probe.host, title_id, title_id))) return;
      if (probe.isStale()) return;
      await appLaunch(transferAddr(probe.host), title_id);
      if (probe.isStale()) return;

      const addr = mgmtAddr(probe.host);
      const deadline = Date.now() + LAUNCH_CONFIRM_TIMEOUT_MS;
      while (Date.now() < deadline) {
        await new Promise((r) => setTimeout(r, LAUNCH_CONFIRM_POLL_MS));
        if (probe.isStale()) return;
        try {
          const running = await fetchRunningGames(addr);
          if (probe.isStale()) return;
          // Publish it so the console row offers Close game straight away.
          useRunningAppsStore.getState().setRunning(Array.from(running.keys()), probe.host);
          if (running.has(title_id)) return; // up — done waiting
        } catch {
          // Transient RPC failure while the title comes up: keep waiting.
          // The deadline is what ends this loop, not a single bad poll.
        }
      }
      // Timed out waiting. The launch itself may still have worked — say so
      // rather than claiming a failure we can't prove.
      pushNotification(
        "info",
        tr("game_hub_launch_unconfirmed", undefined, "Launch not confirmed"),
        {
          body: tr(
            "game_hub_launch_unconfirmed_body",
            undefined,
            "The PS5 accepted the launch but the title hasn't appeared yet. Check the console — it may still be starting.",
          ),
        },
      );
    } catch (e) {
      if (probe.isStale()) return;
      pushNotification(
        "error",
        tr("game_hub_launch_failed", undefined, "Launch failed"),
        { body: e instanceof Error ? e.message : String(e) },
      );
    } finally {
      setLaunching(false);
    }
  }, [title_id, launching, guard, tr, makeWay]);

  // ── Close ─────────────────────────────────────────────────────────
  // Running state comes from the shared store the shell's watcher (and the
  // Games screen, while open) keeps current for the connected console.
  const running = useRunningAppsStore((s) => !!title_id && runningOn(s, host).has(title_id));
  const [stopping, setStopping] = useState(false);
  const { confirm: confirmDialog, dialog: confirmDialogNode } = useConfirm();

  const handleStop = useCallback(async () => {
    if (!title_id || stopping) return;
    const probe = guard.capture();
    if (!probe.host?.trim()) return;
    const label = game?.name ?? title_id;
    const ok = await confirmDialog({
      title: tr("installed_stop_confirm_title", { name: label }, `Close ${label}?`),
      message: tr(
        "installed_stop_confirm_body",
        undefined,
        "This closes the running game on the PS5. Any unsaved progress will be lost — the same as quitting from the console.",
      ),
      confirmLabel: tr("installed_stop", undefined, "Close game"),
      destructive: true,
    });
    if (!ok || probe.isStale()) return;
    setStopping(true);
    try {
      const addr = mgmtAddr(probe.host);
      // The store holds title ids only; the app id and pid come from a fresh read.
      const target = (await fetchRunningGames(addr)).get(title_id);
      const closed = target ? await killGame(addr, target) : true;
      if (probe.isStale()) return;
      if (closed) {
        const store = useRunningAppsStore.getState();
        store.setRunning(
          Array.from(store.titleIds).filter((t) => t !== title_id),
          probe.host,
        );
        pushNotification("info", label, {
          body: tr("installed_stopped", undefined, "Game closed"),
        });
      } else {
        pushNotification("error", label, {
          body: tr(
            "installed_stop_failed",
            undefined,
            "Couldn't close the game — it may have already exited.",
          ),
        });
      }
    } catch (e) {
      if (probe.isStale()) return;
      pushNotification("error", label, {
        body: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setStopping(false);
    }
  }, [title_id, stopping, guard, game?.name, confirmDialog, tr]);

  if (!title_id) {
    return (
      <div className="p-6">
        <EmptyState
          icon={Gamepad2}
          title={tr("game_hub_not_found", undefined, "Game not found")}
          message={tr(
            "game_hub_not_found_desc",
            undefined,
            "This title ID does not match any installed or library game.",
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
              leftIcon={<ArrowLeft size={16} />}
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
                  "This title ID does not match any installed or library game.",
                )
              : tr("v5_home_disconnected", undefined, "Not connected")
          }
        />
      </div>
    );
  }

  return (
    <div className="app-page">
      {makeWayDialog}
      {confirmDialogNode}
      {/* Hero: the framed cover beside a big title and the facts as chips, on
          one glass panel (the reference's greeting panel, laid sideways). */}
      <div className="mb-4">
        <button
          type="button"
          data-testid="game-back"
          onClick={goBack}
          className="inline-flex min-h-9 items-center gap-1.5 rounded-full px-3 text-sm font-medium text-[var(--color-muted)] transition-colors hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
        >
          <ArrowLeft size={15} />
          {tr("game_hub_back_games", undefined, "Back")}
        </button>
      </div>
      <header className="glass relative mb-6 overflow-hidden rounded-[var(--radius-panel)] p-5 sm:p-7">
        <div aria-hidden className="dot-texture pointer-events-none absolute -right-10 -top-10 h-64 w-96" />
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
                  local_cover: gv.view.cover.startsWith("/api/collection/") ? "cover" : undefined,
                  cover_url: gv.view.cover.startsWith("http") ? gv.view.cover : undefined,
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
              {playSeconds !== undefined && playSeconds > 0 && (
                <FactChip icon={<Clock size={12} aria-hidden />}>{formatDuration(playSeconds)}</FactChip>
              )}
              {running && (
                <FactChip tone="good" icon={<CircleDot size={12} className="animate-pulse" aria-hidden />}>
                  {tr("installed_badge_playing", undefined, "Playing")}
                </FactChip>
              )}
            </div>
            {gv.view && (gv.view.consoles.length > 0 || gv.view.copies.length > 0) && (
              <p className="mt-3 text-sm text-[var(--color-muted)]" data-testid="game-summary">
                {summaryLine(rosterOnly(gv.view, Object.keys(names)), names, tr)}
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
        <div className="mb-4 grid gap-4">
          <ConsolesCard
            titleId={game.titleId}
            consoles={profiles.map((p) => ({ host: hostOf(p.host), name: p.name || hostOf(p.host) }))}
            connected={connectedHost}
            view={gv.view}
            refresh={gv.refresh}
            onPlay={() => void handleLaunch()}
            launching={launching}
            running={running}
            onStop={() => void handleStop()}
            stopping={stopping}
            sendHost={sendHost}
            setSendHost={setSendHost}
          />
          <DrivesCard
            view={gv.view}
            connectedHost={connectedHost}
            onSend={() => setSendHost(connectedHost || null)}
            onChanged={() => void gv.reload()}
          />
        </div>
      )}

      {/* The other tabs are about the connected console: say which, and when it lacks the game. */}
      {activeTab !== "overview" && activeTab !== "updates" && activeTab !== "addons" && connectedHost && (
        <div className="mb-3 text-xs text-[var(--color-muted)]" data-testid="game-tab-console">
          {connectedEntry && !connectedEntry.installed
            ? tr("game_tab_not_on", { name: names[connectedHost] ?? connectedHost }, "Not on {name}")
            : tr("game_tab_on", { name: names[connectedHost] ?? connectedHost }, "On {name}")}
        </div>
      )}

      {/* Tab content */}
      <GameTabContent
        view={gv.view}
        game={game}
        host={host}
        playSeconds={playSeconds}
        lastSeenMs={lastSeenMs}
        tab={activeTab}
      />
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
  playSeconds,
  lastSeenMs,
}: {
  tab: TabId;
  view: GameView | null;
  game: GameInfo;
  host: string | null;
  playSeconds: number | undefined;
  lastSeenMs: number | undefined;
}) {
  switch (tab) {
    case "overview":
      return <OverviewTab game={game} playSeconds={playSeconds} lastSeenMs={lastSeenMs} />;
    case "cheats":
      return <CheatsTab titleId={game.titleId} host={host} />;
    case "saves":
      return <SavesTab titleId={game.titleId} host={host} />;
    case "addons":
      return <PackagesTab titleId={game.titleId} title={game.name} host={host} view={view} kind="addons" />;
    case "updates":
      return <PackagesTab titleId={game.titleId} title={game.name} host={host} view={view} kind="updates" />;
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
): { data: T | null; loading: boolean; error: string | null } {
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

  return { data, loading, error };
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
        <span className="grid h-8 w-8 shrink-0 place-items-center rounded-full bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)]"><Icon size={15} aria-hidden /></span>
        {title}
        {loading && <Spinner size={14} />}
      </h2>
      {error ? (
        <Callout tone="error" title={tr("game_hub_error_title", undefined, "Error")}>
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
function CheatsTab({ titleId, host }: { titleId: string; host: string | null }) {
  const tr = useTr();
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const connected = !!host?.trim() && payloadStatus === "up";
  const { data, loading, error } = useTabFetch(
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
      // Roll the switch back — the console didn't accept it.
      setOverrides((o) => ({ ...o, [mod.index]: !next }));
      setToggleError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusyIndex(null);
    }
  };

  const mods = data?.mods ?? [];
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
        <p className="text-sm text-[var(--color-muted)]">
          {tr(
            "game_hub_no_cheats",
            undefined,
            "No cheats installed for this title. Add some from the Cheats screen.",
          )}
        </p>
      ) : (
        <>
          {toggleError && (
            <Callout
              tone="error"
              title={tr("game_hub_cheat_toggle_failed", undefined, "Couldn’t change that cheat")}
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
  const { data, loading, error } = useTabFetch(host, (addr) => savesList(addr, 0), []);

  const saves = useMemo(
    () => (data?.saves ?? []).filter((s: SaveEntry) => s.title_id === titleId),
    [data, titleId],
  );

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
          {tr("game_hub_no_saves", undefined, "No save data found for this title.")}
        </p>
      ) : (
        <>
          <ul className="divide-y divide-[var(--color-border)]">
            {saves.map((s) => (
              <li key={s.path} className="flex items-center justify-between gap-4 py-2">
                <div className="min-w-0">
                  <div
                    className="truncate font-mono text-xs"
                    title={s.path}
                  >
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
                      <span>· {new Date(s.mtime * 1000).toLocaleString()}</span>
                    )}
                  </div>
                </div>
              </li>
            ))}
          </ul>
          <div className="mt-4">
            <Button variant="ghost" size="sm" onClick={() => navigate("/saves")}>
              {tr("game_hub_manage_saves", undefined, "Back up / restore in Saves")}
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
    view?.consoles.find((c) => c.host === hostOf(host ?? ""))?.installed ?? true;
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
          title: tr("pkglib.baseMissing.title", undefined, "Base game isn't installed"),
          message: tr(
            "pkglib.baseMissing.body",
            { kind: what, id: titleId },
            `This ${what} is for ${titleId}, but its base game isn't installed on the PS5. Sony's installer will accept it, but nothing installs until the base game is on the console — install the base first.`,
          ),
          confirmLabel: tr("pkglib.baseMissing.installAnyway", undefined, "Install anyway"),
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
                "game_hub_no_addons",
                undefined,
                "No DLC packages staged for this title. Upload one from Install Package.",
              )
            : tr(
                "game_hub_no_updates",
                undefined,
                "No update packages staged for this title. Upload one from Install Package.",
              )}
        </p>
      ) : (
        <ul className="divide-y divide-[var(--color-border)]">
          {matching.map((e) => (
            <li key={e.path} className="flex items-center justify-between gap-4 py-2">
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
        <Button variant="ghost" size="sm" onClick={() => navigate("/install-package")}>
          {tr("game_hub_open_install", undefined, "Open Install Package")}
        </Button>
      </div>
      {connected && driveOffers(view, host ?? "", kind).length > 0 && (
        <div className="mt-4 border-t border-[var(--color-border)] pt-3" data-testid="game-drive-offers">
          <h3 className="mb-2 text-xs font-semibold text-[var(--color-muted)]">
            {tr("game_drive_offers", undefined, "On your drives")}
          </h3>
          <ul className="divide-y divide-[var(--color-border)]">
            {driveOffers(view, host ?? "", kind).map((o) => (
              <li key={o.path} className="flex items-center justify-between gap-4 py-2">
                <div className="min-w-0">
                  <div className="truncate text-sm font-medium">{o.title || o.name}</div>
                  <div className="mt-0.5 flex items-center gap-2 text-xs text-[var(--color-muted)]">
                    {o.version && <span className="font-mono">v{o.version}</span>}
                    {o.size_bytes > 0 && <span>· {formatBytes(o.size_bytes)}</span>}
                  </div>
                </div>
                <Button
                  size="sm"
                  variant="secondary"
                  leftIcon={<Download size={13} />}
                  onClick={() => {
                    installOffers(hostOf(host ?? ""), [{ game: { title }, offer: o }]);
                    pushNotification("info", tr("collection.queued", { n: 1 }, "{n} packages queued for install"), {
                      link: gamePath(titleId),
                    });
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
function OverviewTab({
  game,
  playSeconds,
  lastSeenMs,
}: {
  game: GameInfo;
  playSeconds: number | undefined;
  lastSeenMs: number | undefined;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  return (
    <div className="grid gap-4 md:grid-cols-2">
      <Card>
        <h2 className="mb-4 flex items-center gap-3 text-[1.0625rem] font-semibold tracking-[-0.01em]">
          <span className="grid h-8 w-8 shrink-0 place-items-center rounded-full bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)]">
            <Info size={15} aria-hidden />
          </span>
          {tr("game_hub_overview", undefined, "Overview")}
        </h2>
        <dl className="text-sm [&>div]:min-h-10 [&>div]:items-center [&>div]:gap-4 [&>div]:border-b [&>div]:border-[var(--color-border)] [&>div:last-child]:border-b-0">
          <div className="flex justify-between">
            <dt className="text-[var(--color-muted)]">{tr("game_hub_title_id", undefined, "Title ID")}</dt>
            <dd className="font-mono">{game.titleId}</dd>
          </div>
          <div className="flex justify-between">
            <dt className="text-[var(--color-muted)]">{tr("game_hub_name", undefined, "Name")}</dt>
            <dd className="truncate">{game.name}</dd>
          </div>
          {game.size > 0 && (
            <div className="flex justify-between">
              <dt className="text-[var(--color-muted)]">{tr("game_hub_size", undefined, "Size")}</dt>
              <dd>{formatBytes(game.size)}</dd>
            </div>
          )}
          <div className="flex justify-between">
            <dt className="text-[var(--color-muted)]">{tr("game_hub_path", undefined, "Path")}</dt>
            <dd className="max-w-[300px] truncate font-mono text-xs" title={game.path}>
              {game.path || "—"}
            </dd>
          </div>
          {playSeconds !== undefined && playSeconds > 0 && (
            <div className="flex justify-between">
              <dt className="text-[var(--color-muted)]">{tr("game_hub_total_play_time", undefined, "Total play time")}</dt>
              <dd>{formatDuration(playSeconds)}</dd>
            </div>
          )}
          {lastSeenMs && (
            <div className="flex justify-between">
              <dt className="text-[var(--color-muted)]">{tr("game_hub_last_played", undefined, "Last played")}</dt>
              <dd>{new Date(lastSeenMs).toLocaleDateString()}</dd>
            </div>
          )}
        </dl>
        {/* Captures are filed by date, not by game, so this opens all of them. */}
        <div className="mt-4">
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<ImageIcon size={14} />}
            onClick={() => navigate("/captures")}
          >
            {tr("captures", undefined, "Screenshots & clips")}
          </Button>
        </div>
      </Card>
    </div>
  );
}

/** Tab label lookup. */
function tabLabel(
  id: TabId,
  tr: (key: string, vars?: Record<string, string | number>, fallback?: string) => string,
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
