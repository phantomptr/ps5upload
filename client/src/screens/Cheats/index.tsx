import { useCallback, useEffect, useMemo, useState } from "react";
import { Gamepad2, Globe, Power, RefreshCw, RotateCcw, WandSparkles, Zap } from "lucide-react";
import {
  PageHeader,
  Button,
  ErrorCard,
  ConnectionGate,
  EmptyState,
  GameIcon,
  Spinner,
} from "../../components";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { hostOf, transferAddr } from "../../lib/addr";
import { humanizePs5Error } from "../../lib/humanizeError";
import {
  appInfoDetails,
  appsInstalled,
  cheatsEngineSet,
  cheatsList,
  cheatsReload,
  cheatsReposSearch,
  cheatsStatus,
  type AppInfoDetails,
  type CheatRepoEntry,
  type CheatTitle,
  type CheatsStatusResponse,
  type InstalledTitle,
} from "../../api/ps5";
import { RepoBrowser } from "./RepoBrowser";
import { CheatGameList } from "./CheatGameList";
import { CheatGameDetail } from "./CheatGameDetail";
import { namesFromRepoEntries } from "../../lib/cheatBrowse";
import {
  applyCheatFilters,
  buildCheatGames,
  cheatSections,
  EMPTY_CHEAT_FILTERS,
  type CheatListFilters,
} from "../../lib/cheatGames";
import { useToast } from "../../state/toasts";
import { reapplyToast } from "./reapplyToast";

/** The cheat collection's index, for the session.
 *
 *  ~11k lines across the repos, served from the engine's cache in well under
 *  a second — and it changes about as often as somebody publishes a cheat,
 *  so fetching it once is enough. Having all of it up front is what lets the
 *  list say which games have cheats before anyone searches. */
let indexCache: CheatRepoEntry[] | null = null;
/** Game details by host + title id, for the session: a version doesn't
 *  change while you browse, and each is a round trip to the console. */
const detailsCache = new Map<string, AppInfoDetails | null>();

export default function CheatsScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const addr = host ? transferAddr(host) : "";
  const up = !!addr && payloadStatus === "up";

  const [installed, setInstalled] = useState<InstalledTitle[]>([]);
  const [downloaded, setDownloaded] = useState<CheatTitle[]>([]);
  const [status, setStatus] = useState<CheatsStatusResponse | null>(null);
  const [index, setIndex] = useState<CheatRepoEntry[]>(indexCache ?? []);
  const [indexError, setIndexError] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [filters, setFilters] = useState<CheatListFilters>(EMPTY_CHEAT_FILTERS);
  const { toast } = useToast();
  const [showBrowser, setShowBrowser] = useState(false);
  const [details, setDetails] = useState<Map<string, AppInfoDetails | null>>(new Map());

  const refresh = useCallback(async () => {
    if (!up) return;
    setLoading(true);
    setError(null);
    try {
      const [list, st, apps] = await Promise.all([
        cheatsList(addr),
        cheatsStatus(addr).catch(() => null),
        appsInstalled(addr).catch(() => null),
      ]);
      setDownloaded(list.titles ?? []);
      setStatus(st);
      if (apps) setInstalled(apps.titles);
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    } finally {
      setLoading(false);
    }
  }, [addr, up]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (indexCache) return;
    void (async () => {
      try {
        const r = await cheatsReposSearch("");
        indexCache = r.entries ?? [];
        setIndex(indexCache);
      } catch {
        // Offline or a repo is down: the list still shows your games and
        // the cheats already on the console, just not what else exists.
        setIndexError(true);
      }
    })();
  }, []);

  const repoNames = useMemo(() => namesFromRepoEntries(index), [index]);
  const games = useMemo(
    () =>
      buildCheatGames({
        installed,
        downloaded,
        index,
        runningTitleId: status?.game_running ? status.game_title_id : null,
        repoNames,
      }),
    [installed, downloaded, index, status, repoNames],
  );
  const sections = useMemo(() => cheatSections(applyCheatFilters(games, filters)), [games, filters]);
  const selected = games.find((g) => g.titleId === selectedId) ?? null;

  // Versions for the games that have cheats, fetched one at a time in the
  // background: the list can then say "for your version", which is the
  // difference between a cheat that works and one that does nothing.
  useEffect(() => {
    if (!up) return;
    const h = hostOf(host);
    const wanted = games.filter(
      (g) => g.installed && (g.available.length > 0 || g.downloaded || g.titleId === selectedId),
    );
    let cancelled = false;
    void (async () => {
      // The selected game first: its detail pane is waiting on it.
      const ordered = [...wanted].sort(
        (a, b) => Number(b.titleId === selectedId) - Number(a.titleId === selectedId),
      );
      for (const g of ordered) {
        if (cancelled) return;
        const key = `${h}/${g.titleId}`;
        if (!detailsCache.has(key)) {
          detailsCache.set(key, await appInfoDetails(addr, g.titleId));
        }
        if (cancelled) return;
        setDetails((prev) => {
          if (prev.get(g.titleId) === detailsCache.get(key)) return prev;
          const next = new Map(prev);
          next.set(g.titleId, detailsCache.get(key) ?? null);
          return next;
        });
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [games, up, addr, host, selectedId]);

  // Open the game being played, or failing that the first game with cheats
  // ready — on a wide screen an empty right-hand pane is wasted.
  useEffect(() => {
    if (selectedId || typeof window === "undefined") return;
    if (!window.matchMedia?.("(min-width: 768px)").matches) return;
    const first = sections.find((s) => s.key !== "none")?.games[0];
    if (first) setSelectedId(first.titleId);
  }, [sections, selectedId]);

  async function setEngine(enabled: boolean) {
    try {
      const r = await cheatsEngineSet(enabled, addr);
      if (r.ok) setStatus((prev) => (prev ? { ...prev, enabled: r.enabled } : prev));
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    }
  }

  // Re-reads the cheat files on the console and applies them again to the
  // running game (the console says so on the TV), so it is its own button, not
  // what Refresh does. The toast follows what the console reports afterwards.
  async function reapply() {
    try {
      await cheatsReload(addr);
      const st = await cheatsStatus(addr).catch(() => null);
      toast(reapplyToast(st, tr));
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    }
    await refresh();
  }

  const nothingDownloaded = downloaded.length === 0;

  return (
    <div className="app-page space-y-4">
      <PageHeader
        icon={WandSparkles}
        title={tr("cheats_title", undefined, "Cheats")}
        description={tr(
          "cheats_description_v2",
          undefined,
          "Pick a game, download cheats made for its version, then switch them on while you play.",
        )}
        right={
          <div className="flex items-center gap-2">
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<Globe size={14} />}
              onClick={() => setShowBrowser(true)}
              disabled={!up}
              title={tr(
                "cheats_browse_all_hint",
                undefined,
                "Search the whole collection, including games that aren't on this PS5",
              )}
            >
              {tr("cheats_browse_all", undefined, "Browse all cheats")}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => void refresh()}
              disabled={loading || !up}
              aria-label={tr("cheats_refresh", undefined, "Refresh")}
              title={tr("cheats_refresh", undefined, "Refresh")}
            >
              {loading ? <Spinner size={14} tone="inherit" /> : <RefreshCw size={14} />}
            </Button>
          </div>
        }
      />

      <ConnectionGate>
        {error && <ErrorCard title={error} onRetry={() => void refresh()} />}

        {status && (
          <EngineBar
            host={host}
            status={status}
            runningName={games.find((g) => g.running)?.name ?? null}
            onToggle={() => void setEngine(!status.enabled)}
            onReapply={() => void reapply()}
            onOpenRunning={() => status.game_title_id && setSelectedId(status.game_title_id.toUpperCase())}
          />
        )}

        {nothingDownloaded && (
          <ol className="grid gap-2 text-xs sm:grid-cols-3">
            {[
              tr("cheats_step1", undefined, "Pick one of your games below."),
              tr("cheats_step2", undefined, "Download a cheat made for your game's version."),
              tr("cheats_step3_v2", undefined, "Start the game on the PS5, then switch cheats on here."),
            ].map((text, i) => (
              <li
                key={i}
                className="flex items-start gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] px-3 py-2"
              >
                <span className="flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-[var(--color-accent)] text-[11px] font-semibold text-[var(--color-accent-contrast)]">
                  {i + 1}
                </span>
                <span className="text-[var(--color-muted)]">{text}</span>
              </li>
            ))}
          </ol>
        )}

        {indexError && (
          <p className="text-xs text-[var(--color-warn)]">
            {tr(
              "cheats_index_offline",
              undefined,
              "Couldn't reach the cheat collection, so only cheats already on the PS5 are shown.",
            )}
          </p>
        )}

        {games.length === 0 && !loading ? (
          <EmptyState
            icon={Gamepad2}
            title={tr("cheats_no_games", undefined, "No games found on this PS5.")}
            message={tr(
              "cheats_no_games_hint",
              undefined,
              "Install a game first, or use Browse all cheats to get cheats for any game.",
            )}
          />
        ) : (
          <div className="grid gap-5 md:grid-cols-[minmax(260px,340px)_1fr]">
            <div className={selected ? "max-md:hidden" : ""}>
              <CheatGameList
                host={host}
                sections={sections}
                filters={filters}
                onFilters={setFilters}
                selectedId={selectedId}
                onSelect={setSelectedId}
                details={details}
              />
            </div>
            <div className={selected ? "" : "max-md:hidden"}>
              {selected ? (
                <CheatGameDetail
                  key={selected.titleId}
                  host={host}
                  addr={addr}
                  game={selected}
                  details={details.get(selected.titleId)}
                  onChanged={refresh}
                  onBack={() => setSelectedId(null)}
                />
              ) : (
                <EmptyState
                  icon={Gamepad2}
                  title={tr("cheats_select_title", undefined, "Select a title")}
                  message={tr(
                    "cheats_select_title_hint",
                    undefined,
                    "Choose a game from the list to view available cheats",
                  )}
                />
              )}
            </div>
          </div>
        )}
      </ConnectionGate>

      {showBrowser && addr && (
        <RepoBrowser
          addr={addr}
          onDownloaded={() => void refresh()}
          onClose={() => setShowBrowser(false)}
        />
      )}
    </div>
  );
}

/** The engine switch and what it is doing right now, in one line. */
function EngineBar({
  host,
  status,
  runningName,
  onToggle,
  onReapply,
  onOpenRunning,
}: {
  host: string;
  status: CheatsStatusResponse;
  runningName: string | null;
  onToggle: () => void;
  onReapply: () => void;
  onOpenRunning: () => void;
}) {
  const tr = useTr();
  return (
    <div
      className={`flex flex-wrap items-center gap-3 rounded-lg border px-4 py-3 ${
        status.enabled
          ? "border-[var(--color-good)]/40 bg-[var(--color-good)]/5"
          : "border-[var(--color-border)] bg-[var(--color-surface-2)]"
      }`}
    >
      <Power
        size={18}
        className={status.enabled ? "text-[var(--color-good)]" : "text-[var(--color-muted)]"}
      />
      <div className="min-w-0 flex-1">
        <div className="text-sm font-medium">
          {status.enabled
            ? tr("cheats_engine_on", undefined, "Cheat engine is on")
            : tr("cheats_engine_off", undefined, "Cheat engine is off")}
        </div>
        <div className="text-xs text-[var(--color-muted)]">
          {status.enabled
            ? tr("cheats_engine_on_hint", undefined, "Cheats you switch on apply to the running game.")
            : tr("cheats_engine_off_hint_v2", undefined, "It turns on by itself when you switch a cheat on.")}
        </div>
      </div>
      {status.game_running && (
        <button
          type="button"
          onClick={onOpenRunning}
          className="flex min-w-0 items-center gap-2 rounded-md px-2 py-1 text-left hover:bg-[var(--color-surface-3)]"
        >
          <GameIcon host={hostOf(host)} titleId={status.game_title_id} size={32} />
          <span className="min-w-0">
            <span className="block text-[11px] text-[var(--color-muted)]">
              {tr("cheats_now_playing", undefined, "Now playing")}
            </span>
            <span className="block max-w-[14rem] truncate text-sm font-medium">
              {runningName || status.game_title_id}
            </span>
          </span>
        </button>
      )}
      {status.enabled && status.patches_total > 0 && (
        <span className="flex items-center gap-1 text-xs text-[var(--color-muted)]">
          <Zap size={12} />
          {tr("cheats_patches_count", { n: status.patches_total }, `${status.patches_total} patches applied`)}
        </span>
      )}
      {status.enabled && status.game_running && (
        <Button
          variant="ghost"
          size="sm"
          leftIcon={<RotateCcw size={13} />}
          onClick={onReapply}
          title={tr(
            "cheats_reapply_hint",
            undefined,
            "Read the cheat files on the PS5 again and re-apply the ones switched on. The console shows a notice on the TV.",
          )}
        >
          {tr("cheats_reapply", undefined, "Re-apply to running game")}
        </Button>
      )}
      <Button
        variant={status.enabled ? "secondary" : "primary"}
        size="sm"
        leftIcon={<Power size={13} />}
        onClick={onToggle}
      >
        {status.enabled
          ? tr("cheats_engine_turn_off", undefined, "Turn off")
          : tr("cheats_engine_turn_on", undefined, "Turn on")}
      </Button>
    </div>
  );
}
