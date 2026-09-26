/**
 * Game Hub (v5 §6.2).
 *
 * Everything about one game behind one URL: `/games/:title_id`.
 *
 * Header: game icon, name, title_id, firmware, size, launch actions.
 * Tabs: Overview · Cheats · Saves · Media · Add-ons · Updates · Storage · Play Time
 *
 * The tab content is rendered by sub-components that lazily fetch their
 * own data. Most tabs start as placeholder shells that the user can
 * navigate to; each gets fleshed out in subsequent phases.
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { useParams, useNavigate, Link, useSearchParams } from "react-router";
import {
  ArrowLeft,
  Gamepad2,
  Info,
  Save,
  Image as ImageIcon,
  Package,
  Download,
  HardDrive,
  Clock,
  Shield,
  Play,
  Film,
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
import {
  usePlayTimeStore,
  playSecondsFor,
  lastSeenPlayingFor,
} from "../../state/playTime";
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
import { transferAddr, mgmtAddr } from "../../lib/addr";
import { fetchRunningGames } from "../../lib/runningGames";
import { useStaleHostGuard } from "../../lib/staleHostGuard";
import { formatBytes, formatDuration } from "../../lib/format";

/** How long to keep Play disabled while waiting for the title to appear in
 *  the process list, and how often to re-check. Mirrors the Installed Apps
 *  screen — a cold first launch (just-installed, disc image) legitimately
 *  takes this long, and re-firing a launch at a half-started title is
 *  exactly how it gets killed. */
const LAUNCH_CONFIRM_TIMEOUT_MS = 90_000;
const LAUNCH_CONFIRM_POLL_MS = 2_000;

const TAB_IDS = [
  "overview",
  "cheats",
  "saves",
  "media",
  "addons",
  "updates",
  "storage",
  "playtime",
] as const;

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
    return null;
  }, [title_id, entries, installedTitles]);

  const playSeconds = playSecondsFor(playTimeState, host, title_id ?? null);
  const lastSeenMs = lastSeenPlayingFor(playTimeState, host, title_id ?? null);

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
  }, [title_id, launching, guard, tr]);

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
              onClick={() => navigate("/games")}
            >
              {tr("game_hub_back", undefined, "Back to library")}
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
    <div className="mx-auto w-full max-w-6xl p-4 md:p-6">
      {/* Header */}
      <header className="mb-6">
        <div className="mb-3 flex items-center gap-2">
          <Link
            to="/games"
            className="flex items-center gap-1 text-sm text-[var(--color-muted)] hover:text-[var(--color-text)]"
          >
            <ArrowLeft size={14} />
            {tr("game_hub_back", undefined, "Back to library")}
          </Link>
        </div>

        <div className="flex items-start gap-4">
          {/* Game icon */}
          <GameIcon
            host={host ?? ""}
            titleId={game.titleId}
            gamePath={game.path}
            alt={game.name}
            size={80}
            rounded="rounded-xl"
            className="shrink-0"
          />

          {/* Title + meta */}
          <div className="min-w-0 flex-1">
            <h1 className="truncate text-2xl font-bold tracking-tight">
              {game.name}
            </h1>
            <div className="mt-1 flex flex-wrap items-center gap-3 text-sm text-[var(--color-muted)]">
              <span className="font-mono">{game.titleId}</span>
              {game.size > 0 && <span>· {formatBytes(game.size)}</span>}
              {playSeconds !== undefined && playSeconds > 0 && (
                <span>· {formatDuration(playSeconds)}</span>
              )}
              <Badge tone="neutral" variant="soft">
                {game.source === "library" ? "Library" : "Installed"}
              </Badge>
            </div>
          </div>

          {/* Launch actions */}
          <div className="flex shrink-0 items-center gap-2">
            <Button
              variant="primary"
              size="sm"
              leftIcon={launching ? <Spinner size={14} /> : <Play size={14} />}
              disabled={launching || payloadStatus !== "up"}
              onClick={handleLaunch}
            >
              {launching
                ? tr("game_hub_launching", undefined, "Starting…")
                : tr("game_hub_launch", undefined, "Launch")}
            </Button>
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

      {/* Tab content */}
      <GameTabContent
        game={game}
        host={host}
        playSeconds={playSeconds}
        lastSeenMs={lastSeenMs}
        tab={activeTab}
      />
    </div>
  );
}

/** Render the content for the active tab. */
function GameTabContent({
  tab,
  game,
  host,
  playSeconds,
  lastSeenMs,
}: {
  tab: TabId;
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
    case "media":
      return <MediaTab />;
    case "addons":
      return <PackagesTab titleId={game.titleId} host={host} kind="addons" />;
    case "updates":
      return <PackagesTab titleId={game.titleId} host={host} kind="updates" />;
    case "storage":
      return <StorageTab game={game} />;
    case "playtime":
      return <PlayTimeTab playSeconds={playSeconds} lastSeenMs={lastSeenMs} />;
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
      <h2 className="mb-3 flex items-center gap-2 text-sm font-semibold">
        <Icon size={16} className="text-[var(--color-muted)]" />
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
 * Media tab.
 *
 * The PS5 stores screenshots and clips under
 * `/user/av_contents/{photo,video}/<userId>/<userId>/<batch>/<file>` — the
 * path carries a capture batch, NOT a title id, and the payload's listing
 * has nothing else to key on. So there is no honest way to show "this
 * game's media" here. Rather than filter on a heuristic that silently
 * misattributes captures, we say so and link to the full browsers.
 */
function MediaTab() {
  const tr = useTr();
  const navigate = useNavigate();
  return (
    <TabCard icon={ImageIcon} title={tr("game_hub_media", undefined, "Media")}>
      <p className="text-sm text-[var(--color-muted)]">
        {tr(
          "game_hub_media_not_per_game",
          undefined,
          "The PS5 doesn't tag screenshots or clips with the game they came from — captures are filed by date, not by title. Browse everything on the console instead:",
        )}
      </p>
      <div className="mt-4 flex flex-wrap gap-2">
        <Button
          variant="secondary"
          size="sm"
          leftIcon={<ImageIcon size={14} />}
          onClick={() => navigate("/screenshots")}
        >
          {tr("game_hub_open_screenshots", undefined, "Screenshots")}
        </Button>
        <Button
          variant="secondary"
          size="sm"
          leftIcon={<Film size={14} />}
          onClick={() => navigate("/videos")}
        >
          {tr("game_hub_open_videos", undefined, "Video clips")}
        </Button>
      </div>
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
  host,
  kind,
}: {
  titleId: string;
  host: string | null;
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
  return (
    <TabCard
      icon={isAddons ? Package : Download}
      title={
        isAddons
          ? tr("game_hub_addons", undefined, "Add-ons")
          : tr("game_hub_updates", undefined, "Updates")
      }
    >
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
              {e.installedHere && (
                <Badge tone="good" variant="soft">
                  {tr("game_hub_installed", undefined, "Installed")}
                </Badge>
              )}
            </li>
          ))}
        </ul>
      )}
      <div className="mt-4">
        <Button variant="ghost" size="sm" onClick={() => navigate("/install-package")}>
          {tr("game_hub_open_install", undefined, "Open Install Package")}
        </Button>
      </div>
    </TabCard>
  );
}

interface GameInfo {
  titleId: string;
  name: string;
  path: string;
  size: number;
  source: "library" | "installed";
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
  return (
    <div className="grid gap-4 md:grid-cols-2">
      <Card>
        <h2 className="mb-3 text-sm font-semibold">
          {tr("game_hub_overview", undefined, "Overview")}
        </h2>
        <dl className="space-y-2 text-sm">
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
      </Card>
    </div>
  );
}

/** Storage tab — disk usage breakdown. */
function StorageTab({ game }: { game: GameInfo }) {
  const tr = useTr();
  return (
    <Card>
      <h2 className="mb-3 text-sm font-semibold">
        {tr("game_hub_storage", undefined, "Storage")}
      </h2>
      {game.size > 0 ? (
        <dl className="space-y-2 text-sm">
          <div className="flex justify-between">
            <dt className="text-[var(--color-muted)]">{tr("game_hub_game_data", undefined, "Game data")}</dt>
            <dd>{formatBytes(game.size)}</dd>
          </div>
          <div className="flex justify-between">
            <dt className="text-[var(--color-muted)]">{tr("game_hub_path", undefined, "Path")}</dt>
            <dd className="max-w-[300px] truncate font-mono text-xs" title={game.path}>
              {game.path || "—"}
            </dd>
          </div>
        </dl>
      ) : (
        <p className="text-sm text-[var(--color-muted)]">
          {tr("game_hub_size_unavailable", undefined, "Size information unavailable.")}
        </p>
      )}
    </Card>
  );
}

/** Play time tab — aggregate stats. */
function PlayTimeTab({
  playSeconds,
  lastSeenMs,
}: {
  playSeconds: number | undefined;
  lastSeenMs: number | undefined;
}) {
  const tr = useTr();
  return (
    <Card>
      <h2 className="mb-3 text-sm font-semibold">
        {tr("game_hub_playtime", undefined, "Play Time")}
      </h2>
      {playSeconds !== undefined && playSeconds > 0 ? (
        <dl className="space-y-2 text-sm">
          <div className="flex justify-between">
            <dt className="text-[var(--color-muted)]">{tr("game_hub_total", undefined, "Total")}</dt>
            <dd>{formatDuration(playSeconds)}</dd>
          </div>
          {lastSeenMs && (
            <div className="flex justify-between">
              <dt className="text-[var(--color-muted)]">{tr("game_hub_last_session", undefined, "Last session")}</dt>
              <dd>{new Date(lastSeenMs).toLocaleString()}</dd>
            </div>
          )}
        </dl>
      ) : (
        <p className="text-sm text-[var(--color-muted)]">
          {tr("game_hub_no_playtime", undefined, "No play time data recorded for this game.")}
        </p>
      )}
    </Card>
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
    case "media":
      return tr("game_hub_media", undefined, "Media");
    case "addons":
      return tr("game_hub_addons", undefined, "Add-ons");
    case "updates":
      return tr("game_hub_updates", undefined, "Updates");
    case "storage":
      return tr("game_hub_storage", undefined, "Storage");
    case "playtime":
      return tr("game_hub_playtime", undefined, "Play Time");
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
    case "media":
      return ImageIcon;
    case "addons":
      return Package;
    case "updates":
      return Download;
    case "storage":
      return HardDrive;
    case "playtime":
      return Clock;
    default:
      return Info;
  }
}
