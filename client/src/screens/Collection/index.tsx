import { useEffect, useMemo, useState } from "react";
import {
  FolderPlus,
  FolderSearch,
  LayoutGrid,
  Library,
  List,
  RefreshCw,
  Search,
  Square,
} from "lucide-react";

import {
  Button,
  EmptyState,
  ErrorCard,
  Input,
  OverflowMenu,
  PageHeader,
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
  importPsGameLibrary,
  loadCollection,
  loadConsoleStates,
  startCollectionScan,
  useCollectionStore,
} from "../../state/collection";
import { useConnectionStore } from "../../state/connection";
import { pushNotification } from "../../state/notifications";
import { useConfirm } from "../../components/ConfirmDialog";
import type { GameConsoleState } from "../../api/collection";
import { consoleCounts, type ConsoleFilter } from "../../lib/collectionView";
import { installOffers } from "./collectionInstall";
import { useTr } from "../../state/lang";
import { CollectionCard } from "./CollectionCard";
import { CollectionTable } from "./CollectionTable";
import { CleanupModal } from "./CleanupModal";
import { FoldersModal } from "./FoldersModal";
import { OrganizeModal } from "./OrganizeModal";
import { GameDetail } from "./GameDetail";
import { ServingLinks } from "./ServingLinks";
import { trashCopies } from "./trashCopies";
import { exportCollection } from "./exportCollection";

/** The games kept on this computer's drives: PS Game Library, inside ps5upload. */
export default function CollectionScreen() {
  const tr = useTr();
  const s = useCollectionStore();
  const [foldersOpen, setFoldersOpen] = useState(false);
  const [organizeOpen, setOrganizeOpen] = useState(false);
  const [cleanupOpen, setCleanupOpen] = useState(false);
  const { confirm, dialog } = useConfirm();
  const host = useConnectionStore((c) => c.host?.trim() ?? "");
  const helperUp = useConnectionStore((c) => c.payloadStatus === "up");

  useEffect(() => {
    void loadCollection();
  }, []);

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
  const consoleStates = useMemo(
    () =>
      helperUp && consoleEntry
        ? new Map<string, GameConsoleState>(Object.entries(consoleEntry.states))
        : undefined,
    [helperUp, consoleEntry],
  );

  const games = useMemo(() => collectionGames(s.library), [s.library]);
  const shown = useMemo(
    () =>
      viewGames(games, {
        search: s.search,
        filter: s.filter,
        platform: s.platform,
        sort: s.sort,
        console: s.consoleFilter,
        consoleStates,
      }),
    [
      games,
      s.search,
      s.filter,
      s.platform,
      s.sort,
      s.consoleFilter,
      consoleStates,
    ],
  );
  const counts = useMemo(() => filterCounts(games), [games]);
  const cCounts = useMemo(
    () => consoleCounts(games, consoleStates),
    [games, consoleStates],
  );

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
      { link: "/install" },
    );
  }
  const roots = s.settings?.roots ?? [];
  const scanning = !!s.scan?.running;
  const summary = s.library?.summary;
  const openGame = s.openGameId ? s.library?.games[s.openGameId] : undefined;

  async function addFolder() {
    const path = await pickPath({
      mode: "folder",
      title: tr(
        "collection.pick_folder",
        undefined,
        "Choose the folder that holds your games",
      ),
    });
    if (path) await addCollectionRoot(path);
  }

  const filterLabel: Record<CollectionFilter, string> = {
    all: tr("collection.filter.all", undefined, "All"),
    duplicates: tr("collection.filter.duplicates", undefined, "Duplicates"),
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
  };

  const header = (
    <PageHeader
      icon={Library}
      title={tr("collection.title", undefined, "Collection")}
      count={summary?.total_games}
      description={tr(
        "collection.description",
        undefined,
        "Every game you keep on this computer's drives, grouped by game: its copies, updates and DLC, duplicates and the space they take. Each item is read from the file itself, wherever it sits.",
      )}
      right={
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
            <Button
              size="sm"
              variant="secondary"
              leftIcon={<FolderSearch size={12} />}
              onClick={() => setFoldersOpen(true)}
            >
              {tr("collection.folders", undefined, "Folders")}
            </Button>
            <OverflowMenu
              items={[
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
                    "Organize packages…",
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
                ...(s.settings?.ps_game_library_found
                  ? [
                      {
                        label: tr(
                          "collection.import_pgl",
                          undefined,
                          "Import from PS Game Library",
                        ),
                        onSelect: () => void importPsGameLibrary(),
                      },
                    ]
                  : []),
              ]}
            />
          </div>
        ) : undefined
      }
    />
  );

  if (!s.settings && s.loading) {
    return (
      <div className="app-page">
        {header}
        <div className="flex items-center gap-2 text-sm text-[var(--color-muted)]">
          <Spinner size={14} />{" "}
          {tr("collection.loading", undefined, "Loading the collection…")}
        </div>
      </div>
    );
  }

  return (
    <div className="app-page">
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
          />
        </div>
      )}

      {roots.length === 0 ? (
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
                "collection.empty_body",
                undefined,
                "Packages, game folders, game images and archives anywhere under it are found and grouped by game. Nothing in the folder is changed. A drive, an external disk or a mounted network share all work.",
              )}
            </span>
          }
          action={
            <div className="flex flex-wrap justify-center gap-2">
              <Button
                variant="primary"
                leftIcon={<FolderPlus size={14} />}
                onClick={() => void addFolder()}
              >
                {tr("collection.add_folder", undefined, "Add a folder")}
              </Button>
              {s.settings?.ps_game_library_found && (
                <Button
                  variant="secondary"
                  onClick={() => void importPsGameLibrary()}
                >
                  {tr(
                    "collection.import_pgl",
                    undefined,
                    "Import from PS Game Library",
                  )}
                </Button>
              )}
            </div>
          }
        />
      ) : (
        <>
          {scanning && (
            <div className="mb-3 flex items-center gap-2 rounded-lg border border-[var(--color-border)] px-3 py-2 text-xs text-[var(--color-muted)]">
              <Spinner size={12} />
              {s.scan && s.scan.found > 0
                ? tr(
                    "collection.scanning_n",
                    { done: s.scan.done, found: s.scan.found },
                    "Scanning: {done} of {found} items read",
                  )
                : tr(
                    "collection.scanning",
                    undefined,
                    "Scanning: looking for games…",
                  )}
            </div>
          )}
          {!scanning && s.scan?.error && (
            <div className="mb-3">
              <ErrorCard
                title={tr(
                  "collection.scan_failed",
                  undefined,
                  "The last scan stopped",
                )}
                detail={s.scan.error}
              />
            </div>
          )}

          {summary && (
            <div
              className="mb-4 grid grid-cols-2 gap-2 sm:grid-cols-5"
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
                label={tr(
                  "collection.stat.duplicates",
                  undefined,
                  "Duplicates",
                )}
                value={String(summary.duplicates_count)}
              />
              <Stat
                wide
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
          </div>

          <div
            className="mb-4 flex flex-wrap gap-1.5"
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
                  className={`rounded-full border px-3 py-1 text-xs ${
                    s.filter === f
                      ? "border-[var(--color-accent)] bg-[var(--color-accent)]/15 text-[var(--color-text)]"
                      : "border-[var(--color-border)] text-[var(--color-muted)] hover:text-[var(--color-text)]"
                  }`}
                >
                  {filterLabel[f]}{" "}
                  <span className="opacity-70">{counts[f]}</span>
                </button>
              ),
            )}
          </div>

          {consoleStates && (
            <div
              className="mb-4 flex flex-wrap items-center gap-1.5"
              role="group"
              aria-label={tr(
                "collection.console_filters",
                undefined,
                "On this PS5",
              )}
            >
              <span className="mr-1 text-xs text-[var(--color-muted)]">
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
                    "missing",
                    tr(
                      "collection.console_missing",
                      undefined,
                      "Not installed",
                    ),
                    cCounts.missing,
                  ],
                  [
                    "update",
                    tr(
                      "collection.console_update",
                      undefined,
                      "Update available",
                    ),
                    cCounts.update,
                  ],
                  [
                    "dlc",
                    tr("collection.console_dlc", undefined, "DLC missing"),
                    cCounts.dlc,
                  ],
                ] as [ConsoleFilter, string, number][]
              ).map(([f, label, n]) => (
                <button
                  key={f}
                  type="button"
                  aria-pressed={s.consoleFilter === f}
                  onClick={() => s.set({ consoleFilter: f })}
                  className={`rounded-full border px-3 py-1 text-xs ${
                    s.consoleFilter === f
                      ? "border-[var(--color-accent)] bg-[var(--color-accent)]/15 text-[var(--color-text)]"
                      : "border-[var(--color-border)] text-[var(--color-muted)] hover:text-[var(--color-text)]"
                  }`}
                >
                  {label} <span className="opacity-70">{n}</span>
                </button>
              ))}
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
            <div className="grid grid-cols-2 gap-3 sm:grid-cols-[repeat(auto-fill,minmax(11rem,1fr))] sm:gap-4">
              {shown.map((g) => (
                <CollectionCard
                  key={g.game_id}
                  game={g}
                  consoleState={consoleStates?.get(g.game_id)}
                  onOpen={() => s.set({ openGameId: g.game_id })}
                />
              ))}
            </div>
          ) : (
            <CollectionTable
              games={shown}
              onOpen={(id) => s.set({ openGameId: id })}
            />
          )}
        </>
      )}

      {openGame && (
        <GameDetail
          game={openGame}
          host={helperUp ? host : ""}
          consoleState={consoleStates?.get(openGame.game_id)}
          onTrash={
            s.settings?.trash_available !== false ||
            s.settings?.allow_permanent_delete
              ? (paths) => void trashCopies(paths, confirm, tr)
              : undefined
          }
          onClose={() => s.set({ openGameId: null })}
        />
      )}
      {dialog}
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

function Stat({
  label,
  value,
  hint,
  wide,
}: {
  label: string;
  value: string;
  hint?: string;
  /** Spans both columns on a phone (the odd fifth tile). */
  wide?: boolean;
}) {
  return (
    <div
      className={`rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] px-3 py-2 ${wide ? "col-span-2 sm:col-span-1" : ""}`}
      title={hint}
    >
      <div className="text-[0.6875rem] uppercase tracking-wide text-[var(--color-muted)]">
        {label}
      </div>
      <div className="text-base font-semibold tabular-nums">{value}</div>
    </div>
  );
}
