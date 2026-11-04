import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  CheckSquare,
  FolderPlus,
  FolderSearch,
  PackageOpen,
  Send,
  ShieldCheck,
  Shrink,
  LayoutGrid,
  Library,
  List,
  RefreshCw,
  Search,
  Server,
  Square,
} from "lucide-react";

import {
  BrowseButton,
  Button,
  EmptyState,
  ErrorCard,
  Input,
  OverflowMenu,
  PageHeader,
  ProgressBar,
  SegmentedControl,
  Select,
  Spinner,
} from "../../components";
import { pickPath } from "../../lib/pickPath";
import {
  COLLECTION_FILTERS,
  COLLECTION_SORTS,
  filterCounts,
  formatCollectionBytes,
  viewGames,
  type CollectionFilter,
  type CollectionPlatform,
  type CollectionSort,
} from "../../lib/collectionView";
import {
  addCollectionRoot,
  cancelCollectionScan,
  collectionGames,
  followEngineScan,
  loadCollection,
  loadConsoleStates,
  startCollectionScan,
  useCollectionStore,
} from "../../state/collection";
import { usePoll } from "../../lib/usePoll";
import { useConnectionStore } from "../../state/connection";
import { pushNotification } from "../../state/notifications";
import { useNavigate } from "react-router";
import { useConfirm } from "../../components/ConfirmDialog";
import type { GameConsoleState } from "../../api/collection";
import { consoleCounts, type ConsoleFilter } from "../../lib/collectionView";
import { installOffers } from "../GameHub/collectionInstall";
import { useTr } from "../../state/lang";
import { useResponsiveTier } from "../../lib/platform";
import { CollectionCard } from "./CollectionCard";
import { CollectionTable } from "./CollectionTable";
import { CleanupModal } from "./CleanupModal";
import { FoldersModal } from "./FoldersModal";
import { elapsedLabel, scanView } from "./scanProgress";
import { useFormatDate } from "../../lib/formatDate";
import { OrganizeModal } from "./OrganizeModal";
import { ServingLinks } from "./ServingLinks";
import { gamePath } from "../../lib/gamePage";
import { exportCollection } from "./exportCollection";
import { useActiveCopies } from "../../state/copyActivity";
import { useShallow } from "zustand/react/shallow";
import { useCheatTitleIds } from "../../lib/cheatIndex";
import { parsePS5Firmware } from "../../lib/ps5Firmware";
import { usePlayTimeStore } from "../../state/playTime";
import { playFor, useTrackedPlay } from "../../lib/trackedPlay";
import { openInConvert } from "../../lib/collectionActions";
import { bestConvertible } from "../../lib/collectionConvert";
import { shrinkCandidate } from "../../lib/collectionInsights";
import { startCopyChecks } from "../../state/copyChecks";
import { SendManyModal } from "./SendManyModal";
import type { PlayState } from "./PlayControl";
import { useGameLaunch } from "../../lib/useGameLaunch";
import { installedKindOf } from "../GameHub/installedKind";
import { useSmpState } from "../GameHub/SmpNeeded";
import type { CollectionGame } from "../../api/collection";
import { useInstalledTitles } from "../../state/installedTitles";
import {
  driveTitleIds,
  entryTitleId,
  libraryForHost,
  useLibraryStore,
} from "../../state/library";
import { useRunningTitleIds } from "../../state/runningApps";

/** How often an open grid looks whether the engine finished a scan of its own. */
const COLLECTION_FOLLOW_MS = 30_000;

/** The games kept on this computer's drives: PS Game Library, inside ps5upload.
 *  `embedded`: shown as the Games screen's grid, under that screen's own
 *  header, so only the actions are drawn here (no page title or page padding). */
export default function CollectionScreen({ embedded = false }: { embedded?: boolean } = {}) {
  const tr = useTr();
  // Everything but the scan's progress counters: those change every second
  // while a scan runs, and only the progress line (ScanProgress) needs them.
  const s = useCollectionStore(
    useShallow((st) => ({
      library: st.library,
      settings: st.settings,
      loading: st.loading,
      error: st.error,
      search: st.search,
      filter: st.filter,
      platform: st.platform,
      sort: st.sort,
      view: st.view,
      consoleByHost: st.consoleByHost,
      consoleFilter: st.consoleFilter,
      set: st.set,
      scanning: !!st.scan?.running,
      scanError: st.scan?.error ?? null,
    })),
  );
  const [foldersOpen, setFoldersOpen] = useState(false);
  const [organizeOpen, setOrganizeOpen] = useState(false);
  const [cleanupOpen, setCleanupOpen] = useState(false);
  const { confirm, dialog } = useConfirm();
  const navigate = useNavigate();
  const host = useConnectionStore((c) => c.host?.trim() ?? "");
  const helperUp = useConnectionStore((c) => c.payloadStatus === "up");

  useEffect(() => {
    void loadCollection();
  }, []);

  // The engine rescans by itself (once a minute by default): a scan that ended while this is open
  // shows without leaving and coming back.
  usePoll(() => followEngineScan(), COLLECTION_FOLLOW_MS, { immediate: false, duringTransfers: true });

  // Cmd/Ctrl+R scans, as in PS Game Library. Capture phase: the desktop app's reload guard
  // swallows the keystroke on the way down, and in a browser it would reload the page.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.shiftKey || e.altKey) return;
      if (e.key.toLowerCase() !== "r") return;
      e.preventDefault();
      const st = useCollectionStore.getState();
      if ((st.settings?.roots.length ?? 0) > 0 && !st.scan?.running) {
        void startCollectionScan(false);
      }
    };
    document.addEventListener("keydown", onKey, { capture: true });
    return () =>
      document.removeEventListener("keydown", onKey, { capture: true });
  }, []);

  // What the selected console has: read when it connects and after every scan.
  const generatedAt = s.library?.generated_at;
  useEffect(() => {
    if (host && helperUp && generatedAt) void loadConsoleStates(host);
  }, [host, helperUp, generatedAt]);
  const consoleEntry = host ? s.consoleByHost[host] : undefined;
  const copyStates = useMemo(
    () =>
      helperUp && consoleEntry
        ? new Map<string, GameConsoleState>(Object.entries(consoleEntry.states))
        : undefined,
    [helperUp, consoleEntry],
  );

  // The selected console's firmware, the cheat index and play time: for the badges, the
  // "Needs newer firmware" / "Has cheats" filters and the "Recently played" sort.
  const kernel = useConnectionStore((c) => c.ps5Kernel);
  const consoleFw = helperUp ? parsePS5Firmware(kernel) : null;
  const cheatIds = useCheatTitleIds();
  // The helper's tracked play time (what Game Activity listed), the app's own count without it.
  const playByHost = usePlayTimeStore((p) => p.byHost);
  const seenByHost = usePlayTimeStore((p) => p.lastSeenByHost);
  const tracked = useTrackedPlay(helperUp ? host : null);
  const playOf = useCallback(
    (id: string) =>
      playFor(tracked, { byHost: playByHost, lastSeenByHost: seenByHost }, host, id),
    [tracked, playByHost, seenByHost, host],
  );
  const lastPlayed = useCallback((id: string) => playOf(id).lastSeenMs, [playOf]);
  const copies = useMemo(() => collectionGames(s.library), [s.library]);
  // Games is one view: the games installed on the selected console that have no copy here show
  // as cards too ("On the PS5 only"), so the console filter covers everything the PS5 has.
  const installedTitles = useInstalledTitles(helperUp ? host : undefined);
  const consoleOnly = useMemo(() => {
    const have = new Set(copies.map((g) => g.game_id.toUpperCase()));
    return (installedTitles ?? [])
      .filter((t) => !t.system && t.titleId && !have.has(t.titleId.toUpperCase()))
      .map(
        (t): CollectionGame => ({
          game_id: t.titleId,
          title: t.titleName || t.titleId,
          platform: /^CUSA/i.test(t.titleId) ? "PS4" : "PS5",
          sources: [],
          locations: [],
          total_size_bytes: 0,
          copies: 0,
          is_duplicate: false,
          added_ts: 0,
        }),
      );
  }, [copies, installedTitles]);
  const consoleOnlyIds = useMemo(() => new Set(consoleOnly.map((g) => g.game_id)), [consoleOnly]);
  // And the games on the PS5's drives that are neither (a game folder or image it has not
  // registered): cards too ("On the PS5's drives"), from the drive scan the Games screen keeps.
  const driveSlot = useLibraryStore((st) => libraryForHost(st, helperUp ? host : ""));
  const driveIds = useMemo(
    () => (helperUp && driveSlot.entries ? driveTitleIds(driveSlot) : null),
    [helperUp, driveSlot],
  );
  const [drivesOnly, drivePaths] = useMemo((): [CollectionGame[], Map<string, string>] => {
    const paths = new Map<string, string>();
    if (!helperUp || !driveSlot.entries) return [[], paths];
    const have = new Set(copies.map((g) => g.game_id.toUpperCase()));
    for (const t of installedTitles ?? []) have.add(t.titleId.toUpperCase());
    const out = new Map<string, CollectionGame>();
    for (const e of driveSlot.entries) {
      const id = entryTitleId(e, driveSlot);
      if (!id || have.has(id.toUpperCase()) || out.has(id.toUpperCase())) continue;
      // A folder's own icon; an image's is inside it.
      if (e.kind === "game") paths.set(id, e.path);
      out.set(id.toUpperCase(), {
        game_id: id,
        title: e.title || id,
        platform: /^CUSA/i.test(id) ? "PS4" : "PS5",
        sources: [],
        locations: [],
        total_size_bytes: 0,
        copies: 0,
        is_duplicate: false,
        added_ts: 0,
      });
    }
    return [[...out.values()], paths];
  }, [helperUp, driveSlot, copies, installedTitles]);
  const drivesOnlyIds = useMemo(() => new Set(drivesOnly.map((g) => g.game_id)), [drivesOnly]);
  const games = useMemo(
    () => copies.concat(consoleOnly, drivesOnly),
    [copies, consoleOnly, drivesOnly],
  );
  const consoleStates = useMemo(() => {
    if (!copyStates && consoleOnly.length === 0 && drivesOnly.length === 0) return undefined;
    const m = new Map(copyStates ?? []);
    for (const g of consoleOnly)
      m.set(g.game_id, { game_id: g.game_id, installed: true, dlc_missing: [], non_package_copy: false });
    for (const g of drivesOnly)
      m.set(g.game_id, { game_id: g.game_id, installed: false, dlc_missing: [], non_package_copy: false });
    return m;
  }, [copyStates, consoleOnly, drivesOnly]);
  const playSeconds = useCallback((id: string) => playOf(id).seconds, [playOf]);
  // What runs on this PS5 now, for the cards' "Playing" tag.
  const runningIds = useRunningTitleIds(helperUp ? host : null);

  // Play and Close game on the cards of games this PS5 has installed: the game page's own
  // (useGameLaunch: closes a running game first with the user's say-so, repairs a homebrew
  // folder the console refuses), held while ShadowMount+ is needed for a disk-image game and
  // not running. Only with the helper up (installedTitles is null otherwise).
  const playable = useMemo(() => {
    const m = new Map<string, { titleId: string; titleName: string; source?: string; disc: boolean }>();
    for (const t of installedTitles ?? []) {
      if (t.system || !t.titleId) continue;
      m.set(t.titleId.toUpperCase(), {
        titleId: t.titleId,
        titleName: t.titleName || t.titleId,
        source: t.source || undefined,
        disc: installedKindOf(t) === "disc",
      });
    }
    return m;
  }, [installedTitles]);
  const anyDisc = useMemo(() => [...playable.values()].some((p) => p.disc), [playable]);
  const smp = useSmpState(helperUp && anyDisc ? host : "");
  const nameOf = useCallback(
    (id: string) => playable.get(id.toUpperCase())?.titleName ?? id,
    [playable],
  );
  const launcher = useGameLaunch({ nameOf });
  const playStateOf = useCallback(
    (gameId: string): PlayState | undefined => {
      const p = playable.get(gameId.toUpperCase());
      if (!p) return undefined;
      if (launcher.stoppingId === p.titleId) return "closing";
      if (runningIds.has(p.titleId) || runningIds.has(gameId)) return "running";
      if (launcher.launchingId === p.titleId) return "starting";
      if (p.disc && smp.absent) return "blocked";
      if (launcher.launchingId) return "wait";
      return "play";
    },
    [playable, launcher.stoppingId, launcher.launchingId, runningIds, smp.absent],
  );
  // The cards are memoised: hand them callbacks that never change, reading the latest launcher.
  const launcherRef = useRef(launcher);
  const playableRef = useRef(playable);
  useLayoutEffect(() => {
    launcherRef.current = launcher;
    playableRef.current = playable;
  });
  const playGame = useCallback((gameId: string) => {
    const p = playableRef.current.get(gameId.toUpperCase());
    if (p) void launcherRef.current.launch(p);
  }, []);
  const stopGame = useCallback((gameId: string) => {
    const p = playableRef.current.get(gameId.toUpperCase());
    if (p) void launcherRef.current.stop(p);
  }, []);
  // Play time changes on every credit (each 10 s while a game runs): the view depends on it only
  // when this sort or filter reads it.
  const sortLastPlayed = s.sort === "played-desc" ? lastPlayed : undefined;
  const filterPlaySeconds = s.consoleFilter === "unplayed" ? playSeconds : undefined;
  const shown = useMemo(
    () =>
      viewGames(games, {
        search: s.search,
        filter: s.filter,
        platform: s.platform,
        sort: s.sort,
        console: s.consoleFilter,
        consoleStates,
        consoleFw,
        cheatIds,
        lastPlayed: sortLastPlayed,
        driveIds,
        playSeconds: filterPlaySeconds,
      }),
    [
      games,
      s.search,
      s.filter,
      s.platform,
      s.sort,
      s.consoleFilter,
      consoleStates,
      consoleFw,
      cheatIds,
      sortLastPlayed,
      driveIds,
      filterPlaySeconds,
    ],
  );
  const counts = useMemo(
    () => filterCounts(copies, { cheatIds }),
    [copies, cheatIds],
  );
  const cCounts = useMemo(
    () => consoleCounts(games, consoleStates, consoleFw, { driveIds, playSeconds }),
    [games, consoleStates, consoleFw, driveIds, playSeconds],
  );

  // Select mode: the cards pick instead of opening, and the bar acts on what is picked.
  const [selecting, setSelecting] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [sendOpen, setSendOpen] = useState(false);
  const toggleSelect = useCallback((id: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);
  const selectedGames = useMemo(
    () => games.filter((g) => selected.has(g.game_id)),
    [games, selected],
  );
  const endSelect = () => {
    setSelecting(false);
    setSelected(new Set());
  };
  const convertSelected = () =>
    openInConvert(
      navigate,
      selectedGames
        .map((g) => bestConvertible(g.locations))
        .filter((l) => l !== null),
    );
  const checkSelected = () => {
    const paths = selectedGames.flatMap((g) =>
      g.locations.map((l) => l.absolute_path),
    );
    void startCopyChecks(paths);
    pushNotification(
      "info",
      tr("collection.check_many", { n: paths.length }, "Checking {n} copies"),
      {
        body: tr(
          "collection.check_many_body",
          undefined,
          "One at a time, on this computer. Each game's page shows what was found.",
        ),
      },
    );
  };
  const shrinkable = useMemo(
    () =>
      s.filter === "smaller"
        ? shown.map(shrinkCandidate).filter((l) => l !== null)
        : [],
    [s.filter, shown],
  );
  // What is being sent or installed right now, by copy: one lookup for the whole grid.
  const active = useActiveCopies(host);
  const openGame = useCallback(
    (id: string) => navigate(gamePath(id)),
    [navigate],
  );
  const activityOf = (g: (typeof games)[number]) => {
    for (const l of g.locations) {
      const a = active.get(l.absolute_path);
      if (a) return a;
    }
    return null;
  };

  /** Every newer update and missing DLC for installed games, for one queue run. */
  async function bringUpToDate() {
    if (!consoleStates) return;
    const items = games.flatMap((g) => {
      const st = consoleStates.get(g.game_id);
      if (!st?.installed) return [];
      return [...(st.update ? [st.update] : []), ...st.dlc_missing].map(
        (offer) => ({ game: g, offer }),
      );
    });
    if (items.length === 0) return;
    const ok = await confirm({
      title: tr(
        "collection.update_all_title",
        undefined,
        "Bring this PS5 up to date?",
      ),
      message: tr(
        "collection.update_all_body",
        { n: items.length },
        "{n} packages from the collection go into the install queue: every newer update and every missing DLC for the games this PS5 has. They stream from this computer, which stays awake until they are done.",
      ),
      confirmLabel: tr("collection.update_all_go", undefined, "Queue them"),
    });
    if (!ok) return;
    const n = installOffers(host, items);
    pushNotification(
      "info",
      tr("collection.queued", { n }, "{n} packages queued for install"),
      { link: "/send" },
    );
  }
  const roots = s.settings?.roots ?? [];
  const phone = useResponsiveTier() === "xs";
  const scanning = s.scanning;
  const summary = s.library?.summary;

  const pickFolderTitle = tr(
    "collection.pick_folder",
    undefined,
    "Choose the folder that holds your games",
  );
  async function addFolder() {
    const path = await pickPath({ mode: "folder", title: pickFolderTitle });
    if (path) await addCollectionRoot(path);
  }
  /** "Add a folder", with a ▾ for a folder on a saved server (NAS, FTP, SFTP, WebDAV). The
   *  plain button picked this device's folders only, so on a phone a NAS could not be added
   *  until the Collection already had a folder (#534). */
  const addFolderButton = (primary: boolean) => (
    <BrowseButton
      mode="folder"
      remote
      primary={primary}
      title={pickFolderTitle}
      icon={<FolderPlus size={primary ? 14 : 12} />}
      label={tr("collection.add_folder", undefined, "Add a folder")}
      onPick={(p) => void addCollectionRoot(p)}
    />
  );

  const filterLabel: Record<CollectionFilter, string> = {
    all: tr("collection.filter.all", undefined, "All"),
    duplicates: tr("collection.filter.duplicates", undefined, "Duplicates"),
    cheats: tr("collection.filter.cheats", undefined, "Has cheats"),
    smaller: tr("collection.filter.smaller", undefined, "Could be smaller"),
    pkg: tr("collection.filter.pkg", undefined, "Packages"),
    mount: tr("collection.filter.mount", undefined, "Images"),
    folder: tr("collection.filter.folder", undefined, "Folders"),
    rar: "RAR",
    "7z": "7z",
    zip: "ZIP",
  };
  const sortLabel: Record<CollectionSort, string> = {
    "title-asc": tr("collection.sort.title_asc", undefined, "Title (A → Z)"),
    "title-desc": tr("collection.sort.title_desc", undefined, "Title (Z → A)"),
    "id-asc": tr("collection.sort.id_asc", undefined, "Game ID (A → Z)"),
    "id-desc": tr("collection.sort.id_desc", undefined, "Game ID (Z → A)"),
    "added-desc": tr(
      "collection.sort.added_desc",
      undefined,
      "Added (newest first)",
    ),
    "added-asc": tr(
      "collection.sort.added_asc",
      undefined,
      "Added (oldest first)",
    ),
    "size-desc": tr(
      "collection.sort.size_desc",
      undefined,
      "Size (largest first)",
    ),
    "size-asc": tr(
      "collection.sort.size_asc",
      undefined,
      "Size (smallest first)",
    ),
    "locations-desc": tr(
      "collection.sort.locations_desc",
      undefined,
      "Most copies",
    ),
    "played-desc": tr(
      "collection.sort.played_desc",
      undefined,
      "Recently played",
    ),
  };

  const actions =
    roots.length > 0 ? (
      <div className="flex flex-wrap items-center gap-2">
        <ServingLinks />
        {scanning ? (
          <Button
            size="sm"
            variant="secondary"
            leftIcon={<Square size={12} />}
            onClick={() => void cancelCollectionScan()}
          >
            {tr("collection.stop_scan", undefined, "Stop scan")}
          </Button>
        ) : (
          <Button
            size="sm"
            variant="primary"
            leftIcon={<RefreshCw size={12} />}
            onClick={() => void startCollectionScan(false)}
          >
            {tr("collection.scan", undefined, "Scan")}
          </Button>
        )}
        {/* On a phone three buttons fill the row and the "…" menu was left
            alone on a line of its own: Folders goes into the menu there. */}
        {!phone && (
          <Button
            size="sm"
            variant="secondary"
            leftIcon={<FolderSearch size={12} />}
            onClick={() => setFoldersOpen(true)}
          >
            {tr("collection.folders", undefined, "Folders")}
          </Button>
        )}
        <OverflowMenu
          items={[
            ...(phone
              ? [
                  {
                    label: tr("collection.folders", undefined, "Folders"),
                    icon: <FolderSearch size={12} />,
                    onSelect: () => setFoldersOpen(true),
                  },
                ]
              : []),
            {
              label: tr(
                "collection.rescan_all",
                undefined,
                "Rescan everything",
              ),
              title: tr(
                "collection.rescan_all_hint",
                undefined,
                "Read every folder size and every item again, ignoring what was cached.",
              ),
              disabled: scanning,
              onSelect: () => void startCollectionScan(true),
            },
            {
              label: tr(
                "collection.org_menu",
                undefined,
                "Organize collection\u2026",
              ),
              disabled: scanning,
              onSelect: () => setOrganizeOpen(true),
            },
            {
              label: tr(
                "collection.junk_menu",
                undefined,
                "Clean up junk files…",
              ),
              onSelect: () => setCleanupOpen(true),
            },
            {
              label: tr(
                "collection.export_csv",
                undefined,
                "Export as CSV",
              ),
              onSelect: () => void exportCollection("csv"),
            },
            {
              label: tr(
                "collection.export_md",
                undefined,
                "Export as Markdown",
              ),
              onSelect: () => void exportCollection("md"),
            },
            {
              label: tr(
                "collection.export_json",
                undefined,
                "Export as JSON",
              ),
              onSelect: () => void exportCollection("json"),
            },
            ...(consoleStates && cCounts.update + cCounts.dlc > 0
              ? [
                  {
                    label: tr(
                      "collection.update_all",
                      undefined,
                      "Bring this PS5 up to date",
                    ),
                    onSelect: () => void bringUpToDate(),
                  },
                ]
              : []),
          ]}
        />
      </div>
    ) : s.settings && (consoleOnly.length > 0 || drivesOnly.length > 0) ? (
      // No folder of your own yet, but the PS5 has games, so the grid is not empty and its
      // "Add a folder" empty state does not show: offer it here instead.
      addFolderButton(false)
    ) : undefined;
  const header = embedded ? (
    actions && <div className="mb-5 flex justify-end">{actions}</div>
  ) : (
    <PageHeader
      icon={Library}
      title={tr("collection.title", undefined, "Collection")}
      count={summary?.total_games}
      description={tr(
        "collection.description",
        undefined,
        "Every game you keep on this computer's drives, grouped by game: its copies, updates and DLC, duplicates and the space they take. Each item is read from the file itself, wherever it sits.",
      )}
      right={actions}
    />
  );
  const pageClass = embedded ? "" : "app-page";

  if (!s.settings && s.loading) {
    return (
      <div className={pageClass}>
        {header}
        <div className="flex items-center gap-2 text-sm text-[var(--color-muted)]">
          <Spinner size={14} />{" "}
          {tr("collection.loading", undefined, "Loading the collection…")}
        </div>
      </div>
    );
  }

  return (
    <div className={pageClass}>
      {header}
      {s.error && (
        <div className="mb-4">
          <ErrorCard
            title={tr(
              "collection.error",
              undefined,
              "The collection could not be read",
            )}
            detail={s.error}
            // Nothing loaded at all: the way out is to read it again.
            onRetry={s.settings ? undefined : () => void loadCollection()}
            retrying={s.loading}
          />
        </div>
      )}

      {/* A folder that would not list is not an empty folder: what it held stays listed. */}
      {(s.library?.unreadable?.length ?? 0) > 0 && (
        <p className="mb-4 rounded-2xl border border-[var(--color-warn)]/30 bg-[var(--color-warn)]/10 px-3 py-2 text-xs">
          {tr(
            "collection.unreadable_folders",
            { paths: (s.library?.unreadable ?? []).slice(0, 5).join(", ") },
            "The last scan could not read: {paths}. What it found there before is kept. Check the folder or the server, then scan again.",
          )}
        </p>
      )}

      {/* A folder the last scan could not open was skipped (#461): its games are missing
          until it is back, so say which and where to remove it. */}
      {(s.library?.missing_roots?.length ?? 0) > 0 && (
        <p className="mb-4 rounded-2xl border border-[var(--color-bad)]/30 bg-[var(--color-bad)]/10 px-3 py-2 text-xs">
          {tr(
            "collection.missing_roots",
            { paths: (s.library?.missing_roots ?? []).join(", ") },
            "The last scan skipped what it could not find: {paths}. If a folder was deleted, remove it in Folders; if it is on a drive or server, reconnect it and scan again.",
          )}{" "}
          <button
            type="button"
            className="font-medium underline"
            onClick={() => setFoldersOpen(true)}
          >
            {tr("collection.folders", undefined, "Folders")}
          </button>
        </p>
      )}

      {/* Not loaded (the read failed): an unread collection is not an empty
          one, so no "add the folder that holds your games" under the error. */}
      {!s.settings ? null : roots.length === 0 && consoleOnly.length === 0 && drivesOnly.length === 0 ? (
        <EmptyState
          fill
          icon={Library}
          headingTag="h2"
          title={tr(
            "collection.empty_title",
            undefined,
            "Add the folder that holds your games",
          )}
          body={
            <span>
              {tr(
                "collection.empty_body_v2",
                undefined,
                "Packages, game folders, game images and archives anywhere under it are found and grouped by game. Nothing in the folder is changed. A folder on this device, an external drive, or a folder on a NAS or server (SMB, FTP, SFTP, WebDAV) all work.",
              )}
            </span>
          }
          action={
            <div className="flex flex-wrap justify-center gap-2">
              {addFolderButton(true)}
              <Button
                variant="secondary"
                leftIcon={<Server size={14} />}
                onClick={() => setFoldersOpen(true)}
              >
                {tr(
                  "collection.add_nas_folder",
                  undefined,
                  "From a NAS or server",
                )}
              </Button>
            </div>
          }
        />
      ) : (
        <>
          {scanning ? <ScanProgress /> : <LastScan />}
          {!scanning && s.scanError && (
            <div className="mb-3">
              <ErrorCard
                title={tr(
                  "collection.scan_failed",
                  undefined,
                  "The last scan stopped",
                )}
                detail={s.scanError}
              />
            </div>
          )}

          {summary && (
            <div
              // Five tiles: two a row on a phone, 3 + 2 (as sixths) up to `xl`,
              // one row only where each tile is wide enough for its number.
              // Five across from `sm` broke "DUPLICAT/ES" and "195.58/GB".
              className="mb-5 grid grid-cols-2 gap-3 sm:grid-cols-6 xl:grid-cols-5"
              data-testid="collection-summary"
            >
              <Stat
                label={tr("collection.stat.games", undefined, "Games")}
                value={String(summary.total_games)}
              />
              <Stat
                label={tr("collection.stat.copies", undefined, "Items")}
                value={String(summary.total_locations)}
              />
              <Stat
                label={tr("collection.stat.size", undefined, "Size")}
                value={formatCollectionBytes(summary.total_size_bytes)}
              />
              <Stat
                row="two"
                label={tr(
                  "collection.stat.duplicates",
                  undefined,
                  "Duplicates",
                )}
                value={String(summary.duplicates_count)}
              />
              <Stat
                wide
                row="two"
                label={tr(
                  "collection.stat.reclaimable",
                  undefined,
                  "Reclaimable",
                )}
                value={formatCollectionBytes(summary.reclaimable_bytes)}
                hint={tr(
                  "collection.stat.reclaimable_hint",
                  undefined,
                  "Space held by every full copy of a game but its largest. Updates and DLC never count.",
                )}
              />
            </div>
          )}

          <div className="mb-3 flex flex-wrap items-end gap-2">
            <div className="min-w-[12rem] flex-1">
              <Input
                block
                leftIcon={<Search size={14} />}
                value={s.search}
                onChange={(e) => s.set({ search: e.target.value })}
                placeholder={tr(
                  "collection.search",
                  undefined,
                  "Search by title, game ID, content ID or path",
                )}
                aria-label={tr(
                  "collection.search_label",
                  undefined,
                  "Search the collection",
                )}
              />
            </div>
            <SegmentedControl
              ariaLabel={tr("collection.platform", undefined, "Platform")}
              value={s.platform}
              onChange={(v) => s.set({ platform: v as CollectionPlatform })}
              segments={[
                {
                  value: "all",
                  label: tr("collection.platform_all", undefined, "All"),
                },
                { value: "PS5", label: "PS5" },
                { value: "PS4", label: "PS4" },
                { value: "PS3", label: "PS3" },
                {
                  value: "other",
                  label: tr("collection.platform_other", undefined, "Other"),
                },
              ]}
            />
            <Select
              block={false}
              aria-label={tr("collection.sort", undefined, "Sort")}
              value={s.sort}
              onChange={(e) =>
                s.set({ sort: e.target.value as CollectionSort })
              }
              options={COLLECTION_SORTS.map((v) => ({
                value: v,
                label: sortLabel[v],
              }))}
            />
            <SegmentedControl
              ariaLabel={tr("collection.view", undefined, "View")}
              value={s.view}
              onChange={(v) => s.set({ view: v as "grid" | "table" })}
              segments={[
                {
                  value: "grid",
                  label: tr("collection.view_grid", undefined, "Grid"),
                  icon: LayoutGrid,
                },
                {
                  value: "table",
                  label: tr("collection.view_table", undefined, "Table"),
                  icon: List,
                },
              ]}
            />
            <Button
              size="sm"
              variant={selecting ? "primary" : "secondary"}
              leftIcon={<CheckSquare size={13} />}
              aria-pressed={selecting}
              onClick={() => (selecting ? endSelect() : setSelecting(true))}
            >
              {selecting
                ? tr("collection.select_done", undefined, "Done")
                : tr("collection.select", undefined, "Select")}
            </Button>
          </div>

          <div
            className="mb-4 flex flex-wrap gap-2"
            role="group"
            aria-label={tr("collection.filters", undefined, "Filters")}
          >
            {COLLECTION_FILTERS.filter((f) => f === "all" || counts[f] > 0).map(
              (f) => (
                <button
                  key={f}
                  type="button"
                  aria-pressed={s.filter === f}
                  onClick={() => s.set({ filter: f })}
                  className="chip min-h-9 gap-1.5 px-4 text-xs font-medium"
                >
                  {filterLabel[f]}
                  <span className="tabular-nums text-[var(--color-muted)]">
                    {counts[f]}
                  </span>
                </button>
              ),
            )}
          </div>

          {consoleStates && (
            <div
              className="mb-5 flex flex-wrap items-center gap-2"
              role="group"
              aria-label={tr(
                "collection.console_filters",
                undefined,
                "On this PS5",
              )}
            >
              <span className="me-1 text-[0.6875rem] font-medium uppercase tracking-[0.08em] text-[var(--color-muted)]">
                {consoleEntry?.loading
                  ? tr(
                      "collection.console_reading",
                      undefined,
                      "Reading this PS5…",
                    )
                  : tr("collection.console_on", undefined, "On this PS5:")}
              </span>
              {(
                [
                  [
                    "any",
                    tr("collection.console_any", undefined, "Everything"),
                    games.length,
                  ],
                  [
                    "installed",
                    tr("collection.console_installed", undefined, "Installed"),
                    cCounts.installed,
                    tr(
                      "collection.console_installed_hint",
                      undefined,
                      "Games this PS5 has installed.",
                    ),
                  ],
                  [
                    "missing",
                    tr(
                      "collection.console_missing",
                      undefined,
                      "Not installed",
                    ),
                    cCounts.missing,
                    tr(
                      "collection.console_missing_hint",
                      undefined,
                      "Games you have that this PS5 has not installed.",
                    ),
                  ],
                  // One chip for both kinds of add-on: "Update available" and "DLC missing"
                  // read as the console's own store state, which they are not.
                  ...(cCounts.addons > 0 || s.consoleFilter === "addons"
                    ? [
                        [
                          "addons",
                          tr(
                            "collection.console_addons",
                            undefined,
                            "Updates & DLC to install",
                          ),
                          cCounts.addons,
                          tr(
                            "collection.console_addons_hint",
                            undefined,
                            "Installed games for which this computer has a newer update, or DLC this PS5 does not have yet.",
                          ),
                        ],
                      ]
                    : []),
                  ...(cCounts.unplayed > 0 || s.consoleFilter === "unplayed"
                    ? [
                        [
                          "unplayed",
                          tr(
                            "collection.console_unplayed",
                            undefined,
                            "Never seen played",
                          ),
                          cCounts.unplayed,
                          tr(
                            "collection.console_unplayed_hint",
                            undefined,
                            "Installed games this app has never seen running on this PS5.",
                          ),
                        ],
                      ]
                    : []),
                  ...(cCounts.drives > 0 || s.consoleFilter === "drives"
                    ? [
                        [
                          "drives",
                          tr(
                            "collection.console_drives",
                            undefined,
                            "On the PS5's drives",
                          ),
                          cCounts.drives,
                          tr(
                            "collection.console_drives_hint",
                            undefined,
                            "Games with a folder or disk image on this PS5's drives.",
                          ),
                        ],
                      ]
                    : []),
                  ...(consoleFw &&
                  (cCounts.firmware > 0 || s.consoleFilter === "firmware")
                    ? [
                        [
                          "firmware",
                          tr(
                            "collection.console_firmware",
                            undefined,
                            "Needs newer firmware",
                          ),
                          cCounts.firmware,
                          tr(
                            "collection.console_firmware_hint",
                            { fw: consoleFw },
                            "Games that ask for newer firmware than this PS5's {fw}: they may not start unless backported.",
                          ),
                        ],
                      ]
                    : []),
                ] as [ConsoleFilter, string, number, string?][]
              ).map(([f, label, n, hint]) => (
                <button
                  key={f}
                  type="button"
                  aria-pressed={s.consoleFilter === f}
                  onClick={() => s.set({ consoleFilter: f })}
                  title={hint}
                  className="chip min-h-9 gap-1.5 px-4 text-xs font-medium"
                >
                  {label}
                  <span className="tabular-nums text-[var(--color-muted)]">
                    {n}
                  </span>
                </button>
              ))}
            </div>
          )}
          {consoleStates && cCounts.addons > 0 && (
            // The one-click catch-up, where it can be seen rather than in the ⋯ menu.
            <div className="surface-panel mb-5 flex flex-wrap items-center gap-3 !rounded-[var(--radius-card)] px-5 py-3 text-sm">
              <span className="min-w-0 flex-1">
                {tr(
                  "collection.uptodate_banner",
                  { u: cCounts.update, d: cCounts.dlc },
                  "This PS5 can take {u} update(s) and DLC for {d} game(s) from your collection.",
                )}
              </span>
              <Button
                size="sm"
                variant="primary"
                onClick={() => void bringUpToDate()}
              >
                {tr(
                  "collection.update_all",
                  undefined,
                  "Bring this PS5 up to date",
                )}
              </Button>
            </div>
          )}
          {s.filter === "smaller" && shrinkable.length > 0 && (
            <div className="surface-panel mb-5 flex flex-wrap items-center gap-3 !rounded-[var(--radius-card)] px-5 py-3 text-sm">
              <span className="min-w-0 flex-1">
                {tr(
                  "collection.smaller_banner",
                  { n: shrinkable.length },
                  "{n} PS5 game(s) are kept as a folder or an uncompressed image. As a compressed image (.ffpfsc) they take less room here and on the PS5; how much depends on the game.",
                )}
              </span>
              <Button
                size="sm"
                variant="primary"
                leftIcon={<Shrink size={13} />}
                onClick={() =>
                  openInConvert(navigate, shrinkable, { compress: true })
                }
              >
                {tr(
                  "collection.smaller_go_v2",
                  undefined,
                  "Queue them in Workshop",
                )}
              </Button>
            </div>
          )}
          {consoleEntry?.error && helperUp && (
            <p className="mb-3 text-xs text-[var(--color-warn)]">
              {tr(
                "collection.console_error",
                { error: consoleEntry.error },
                "This PS5 could not be read: {error}",
              )}
            </p>
          )}

          {shown.length === 0 ? (
            <EmptyState
              icon={Search}
              title={
                games.length === 0
                  ? scanning
                    ? tr("collection.none_yet", undefined, "Nothing found yet")
                    : tr(
                        "collection.none",
                        undefined,
                        "No games found in these folders",
                      )
                  : tr("collection.no_match", undefined, "No games match")
              }
            />
          ) : s.view === "grid" ? (
            <div className="grid grid-cols-2 gap-x-3 gap-y-5 sm:grid-cols-[repeat(auto-fill,minmax(12rem,1fr))] sm:gap-x-5 sm:gap-y-7">
              {shown.map((g) => (
                <CollectionCard
                  key={g.game_id}
                  game={g}
                  onlyOnConsole={consoleOnlyIds.has(g.game_id)}
                  onlyOnDrives={drivesOnlyIds.has(g.game_id)}
                  drivePath={drivePaths.get(g.game_id)}
                  running={runningIds.has(g.game_id)}
                  play={playStateOf(g.game_id)}
                  onPlay={playGame}
                  onStop={stopGame}
                  activity={activityOf(g)}
                  consoleState={consoleStates?.get(g.game_id)}
                  onOpen={openGame}
                  host={helperUp ? host : undefined}
                  consoleFw={consoleFw}
                  hasCheats={!!cheatIds?.has(g.game_id.toUpperCase())}
                  playSec={playOf(g.game_id).seconds}
                  selecting={selecting}
                  selected={selected.has(g.game_id)}
                  onToggleSelect={toggleSelect}
                />
              ))}
            </div>
          ) : (
            <CollectionTable
              games={shown}
              onOpen={(id) => navigate(gamePath(id))}
              host={helperUp ? host : undefined}
              consoleFw={consoleFw}
              consoleStates={consoleStates}
              cheatIds={cheatIds}
              playSec={(id) => playOf(id).seconds}
              selecting={selecting}
              selected={selected}
              onToggleSelect={toggleSelect}
              playOf={playStateOf}
              onPlay={playGame}
              onStop={stopGame}
            />
          )}
        </>
      )}

      {selecting && (
        // The selection's actions, kept in view while the grid scrolls.
        <div className="sticky bottom-3 z-20 mt-4 flex flex-wrap items-center gap-2 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-float)] px-4 py-3 text-sm shadow-lg backdrop-blur">
          <span className="me-auto tabular-nums">
            {tr("collection.selected_n", { n: selected.size }, "{n} selected")}
          </span>
          <Button
            size="sm"
            variant="ghost"
            onClick={() =>
              setSelected(
                selected.size === shown.length && shown.length > 0
                  ? new Set()
                  : new Set(shown.map((g) => g.game_id)),
              )
            }
          >
            {selected.size === shown.length && shown.length > 0
              ? tr("collection.select_none", undefined, "Select none")
              : tr(
                  "collection.select_all",
                  { n: shown.length },
                  "Select all {n}",
                )}
          </Button>
          <Button
            size="sm"
            variant="primary"
            leftIcon={<Send size={13} />}
            disabled={selected.size === 0}
            onClick={() => setSendOpen(true)}
          >
            {tr("collection.send_many", undefined, "Send to PS5…")}
          </Button>
          <Button
            size="sm"
            variant="secondary"
            leftIcon={<PackageOpen size={13} />}
            disabled={!selectedGames.some((g) => bestConvertible(g.locations))}
            onClick={convertSelected}
          >
            {tr("collection.convert_many", undefined, "Convert")}
          </Button>
          <Button
            size="sm"
            variant="secondary"
            leftIcon={<ShieldCheck size={13} />}
            disabled={selected.size === 0}
            onClick={checkSelected}
          >
            {tr("collection.check_many_go", undefined, "Check copies")}
          </Button>
        </div>
      )}
      <SendManyModal
        open={sendOpen}
        games={selectedGames}
        host={host}
        onClose={() => setSendOpen(false)}
        onSent={() => {
          setSendOpen(false);
          endSelect();
        }}
      />
      {dialog}
      {launcher.dialogs}
      <OrganizeModal
        open={organizeOpen}
        onClose={() => setOrganizeOpen(false)}
      />
      <CleanupModal open={cleanupOpen} onClose={() => setCleanupOpen(false)} />
      <FoldersModal
        open={foldersOpen}
        onClose={() => setFoldersOpen(false)}
        onAdd={() => void addFolder()}
      />
    </div>
  );
}

/** Re-renders every second while mounted: for the elapsed and "ago" times. */
function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);
  return now;
}

/** The running scan: what it is doing, how far it is, what it is reading, and how long it
 *  has taken. The one part of the screen that follows the scan's counters. */
function ScanProgress() {
  const tr = useTr();
  const scan = useCollectionStore((st) => st.scan);
  const now = useNow();
  if (!scan) return null;
  const v = scanView(scan);
  const title = {
    finding: tr(
      "collection.scan_phase_finding",
      undefined,
      "Looking for games in your folders",
    ),
    reading: tr(
      "collection.scan_phase_reading",
      undefined,
      "Reading each item",
    ),
    naming: tr(
      "collection.scan_phase_naming",
      undefined,
      "Finding names and covers",
    ),
    saving: tr(
      "collection.scan_phase_saving",
      undefined,
      "Saving the collection",
    ),
  }[v.phase];
  const detail = v.counts
    ? v.phase === "naming"
      ? tr(
          "collection.scan_games_n",
          { done: v.counts.done, total: v.counts.total },
          "{done} of {total} games",
        )
      : tr(
          "collection.scan_items_n",
          { done: v.counts.done, total: v.counts.total },
          "{done} of {total} items",
        )
    : v.folder
      ? tr(
          "collection.scan_folder_n",
          { n: v.folder.n, total: v.folder.total },
          "Folder {n} of {total}",
        )
      : null;
  return (
    <section
      aria-live="polite"
      data-testid="collection-scan-progress"
      className="surface-panel mb-4 !rounded-[var(--radius-card)] px-4 py-3"
    >
      <div className="flex items-center gap-2 text-sm">
        <Spinner size={14} />
        <span className="min-w-0 flex-1 truncate font-medium">{title}</span>
        {detail && (
          <span className="shrink-0 text-xs tabular-nums text-[var(--color-muted)]">
            {detail}
          </span>
        )}
        {scan.started_ms > 0 && (
          <span className="shrink-0 text-xs tabular-nums text-[var(--color-muted)]">
            {elapsedLabel(scan.started_ms, now)}
          </span>
        )}
      </div>
      <ProgressBar className="mt-2.5" size="sm" value={v.value} label={title} />
      {v.current && (
        <p
          className="mt-1.5 truncate text-xs text-[var(--color-muted)]"
          title={v.current}
        >
          {v.current}
        </p>
      )}
    </section>
  );
}

/** When the last scan finished and how long it took, under the header while none runs. */
function LastScan() {
  const tr = useTr();
  const fmt = useFormatDate();
  const scan = useCollectionStore((st) => st.scan);
  if (
    !scan ||
    scan.running ||
    scan.error ||
    !scan.finished_ms ||
    !scan.started_ms
  )
    return null;
  const took = elapsedLabel(scan.started_ms, scan.finished_ms);
  return (
    <p
      className="mb-3 text-xs text-[var(--color-muted)]"
      data-testid="collection-last-scan"
    >
      {tr(
        "collection.last_scan_took",
        { when: fmt(scan.finished_ms), took },
        "Last scanned {when} · took {took}",
      )}
    </p>
  );
}

const SPAN = { three: "sm:col-span-2", two: "sm:col-span-3" } as const;

function Stat({
  label,
  value,
  hint,
  wide,
  row = "three",
}: {
  label: string;
  value: string;
  hint?: string;
  /** Spans both columns on a phone (the odd fifth tile). */
  wide?: boolean;
  /** Which row the tile is on between `sm` and `xl`: three a row, then two. */
  row?: "three" | "two";
}) {
  return (
    <div
      className={`surface-panel !rounded-[var(--radius-card)] min-w-0 px-4 py-3 xl:col-span-1 ${SPAN[row]} ${wide ? "col-span-2" : ""}`}
      title={hint}
    >
      <div className="text-[0.6875rem] font-medium uppercase tracking-[0.08em] text-[var(--color-muted)]">
        {label}
      </div>
      <div className="mt-0.5 whitespace-nowrap text-lg font-semibold tracking-[-0.01em] tabular-nums">
        {value}
      </div>
    </div>
  );
}
