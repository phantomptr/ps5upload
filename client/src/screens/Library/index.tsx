import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  LibraryBig,
  RefreshCw,
  Loader2,
  FileArchive,
  Gamepad2,
  Trash2,
  Shield,
  Play,
  Unplug,
  Download,
  FolderInput,
  Search,
  X,
  Info,
  ExternalLink,
  Boxes,
  PackageOpen,
  Sparkles,
  type LucideIcon,
} from "lucide-react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { open as openExternal } from "@tauri-apps/plugin-shell";

import { useConnectionStore, PS5_PAYLOAD_PORT } from "../../state/connection";
import SmpPanel from "./SmpPanel";
import RunningAppsPanel from "./RunningAppsPanel";
import { useRunningAppsStore } from "../../state/runningApps";
import { usePlayTimeStore, formatPlayTime } from "../../state/playTime";
import {
  scanLibrary,
  fsDelete,
  fsChmod,
  fsCopy,
  fsPathExists,
  fsMount,
  fsUnmount,
  appLaunch,
  appRegister,
  appUnregister,
  fsOpStatus,
  fsOpCancel,
  fetchVolumes,
  fetchGameMeta,
  gameIconUrl,
  jobStatus,
  startTransferDownload,
  healAppmeta,
  type LibraryEntry,
  type GameMeta,
  type Volume,
} from "../../api/ps5";
import {
  defaultMoveSubpath,
  detectSourceVolume,
  isInvalidName,
  isMoveNoop,
  resolveMoveDestination,
  sourceBasename,
} from "../../lib/moveTarget";
import { useLibraryStore, findOwningImage } from "../../state/library";
import { useElapsed } from "../../lib/useElapsed";
import { formatBytes } from "../../lib/format";
import { mgmtAddr } from "../../lib/addr";
import { createLimiter } from "../../lib/limitConcurrency";
import { deleteWithRetry } from "../../lib/deleteWithRetry";
import {
  classifyMovePollError,
  isExpectedNotInFlight,
} from "../../lib/movePollerPolicy";
import { filterLibraryEntries } from "../../lib/libraryFilter";
import { humanizePs5Error } from "../../lib/humanizeError";
import {
  fetchTitleInfo,
  patchesSiteName,
  patchesSiteUrl,
  type TitleInfo,
} from "../../lib/titleDetails";
import {
  MOUNT_DEFAULT_SUBPATH,
  MOUNT_PRESETS,
  loadMountDest,
  payloadSupportsMountPoint,
  resolveMountPath,
  saveMountDest,
} from "../../lib/mountDest";
import { useActivityHistoryStore } from "../../state/activityHistory";
import { pushNotification } from "../../state/notifications";
import {
  PageHeader,
  EmptyState,
  ErrorCard,
  Button,
  OverflowMenu,
  type OverflowMenuItem,
} from "../../components";
import { useTr } from "../../state/lang";

// Module-level limiter shared across every LibraryRow mounted at once.
// 4 concurrent FS_READs keeps the metadata fetch snappy (browser
// overlap masks the per-request RTT) while leaving bandwidth on the
// payload's single-threaded mgmt port for other UI-driven calls
// (Volumes refresh, File System navigation) to interleave cleanly.
const metaLimit = createLimiter(4);

function formatDuration(sec: number): string {
  if (!isFinite(sec) || sec < 0) return "—";
  if (sec < 60) return `${Math.ceil(sec)}s`;
  const m = Math.floor(sec / 60);
  const s = Math.floor(sec % 60);
  if (m < 60) return `${m}m ${s}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}

// formatBytes moved to lib/format.ts — kept consistent across screens.

export default function LibraryScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const entries = useLibraryStore((s) => s.entries);
  const mountMap = useLibraryStore((s) => s.mountMap);
  const pendingMounts = useLibraryStore((s) => s.pendingMounts);
  const volumes = useLibraryStore((s) => s.volumes);
  const loading = useLibraryStore((s) => s.loading);
  const error = useLibraryStore((s) => s.error);
  const lastRefreshedAt = useLibraryStore((s) => s.lastRefreshedAt);
  const setLoading = useLibraryStore((s) => s.setLoading);
  const setError = useLibraryStore((s) => s.setError);

  const refresh = useCallback(async () => {
    if (!host?.trim()) return;
    // Host-switch race guard: refresh() is async and writes into the
    // shared useLibraryStore. If the user points the app at a new PS5
    // mid-scan, a slow scan of the PREVIOUS host can land after the new
    // host's scan and repaint the old console's library under the new
    // host's name. Capture the host this run targets and drop the
    // result (store write, error, loading-clear) if it's been
    // superseded. refresh itself is keyed on `host`, so the new host
    // gets its own fresh invocation.
    const probedHost = host;
    const isStale = () =>
      useConnectionStore.getState().host !== probedHost;
    setLoading(true);
    setError(null);
    try {
      const addr = `${probedHost}:${PS5_PAYLOAD_PORT}`;
      // Volume probe failure is non-fatal for the library scan
      // itself (the Move modal degrades to "no destinations
      // available" instead of blocking the whole screen), but we
      // still log so diagnosis is possible. Without the log, a
      // transient mgmt-port timeout looked indistinguishable from
      // "no drives attached" — same empty UI, no breadcrumb.
      const [result, volumes] = await Promise.all([
        scanLibrary(addr),
        fetchVolumes(addr).catch((e) => {
          console.warn(
            "[library] volume probe failed; Move modal will offer no destinations:",
            e,
          );
          return [];
        }),
      ]);
      // image_path → mount_point. Pre-2.2.36 this gated on the mount
      // landing under /mnt/ps5upload/, which broke the MOUNTED badge
      // and the Unmount button for any user-chosen mount point (e.g.
      // /data/homebrew/Mafia/) — the volume row carries the right
      // source_image either way, so the prefix check was wrong. The
      // payload's tracker file is the consent record on the unmount
      // side, so accepting any source_image here doesn't open a
      // surprise-unmount path: clicking Unmount calls fs_unmount
      // which still validates "is this our mount?".
      const next = new Map<string, string>();
      for (const v of volumes) {
        if (v.source_image) {
          next.set(v.source_image, v.path);
        }
      }
      // Move-modal destinations only make sense for real attached
      // drives, so filter out placeholder/read-only volumes here.
      const writable = volumes.filter((v) => v.writable && !v.is_placeholder);
      // Drop the result if the user switched hosts while the scan was
      // in flight — see the probedHost guard at the top of refresh().
      if (isStale()) return;
      // Stale-empty guard: a transient race during fs_mount /
      // fs_unmount can cause scanLibrary to return zero entries
      // even though there are still games on disk (volumes are
      // mid-update on the kernel side). If we previously had
      // entries and the new scan returned nothing, keep the old
      // entries — only update mountMap + volumes from the fresh
      // probe — and let the next scheduled refresh produce a real
      // count. The user-visible symptom this fixes: "library goes
      // blank after mounting an image, must click Refresh manually."
      //
      // Both branches go through a single `useLibraryStore.setState`
      // callback so the read-of-entries and the conditional write
      // are atomic against any concurrent setData (e.g. from a
      // parallel `ps5upload:library:invalidate` event). Reading
      // `getState().entries` first and then committing via a
      // separate `setState` would TOCTOU between the two: a
      // concurrent `clear()` could flip entries to null between
      // the read and the write, causing us to wipe the display
      // with the stale empty result.
      useLibraryStore.setState((s) => {
        const hadEntries = (s.entries ?? []).length > 0;
        const prunedPending = new Map<string, string>();
        for (const [imagePath, mountPoint] of s.pendingMounts) {
          if (!next.has(imagePath)) {
            prunedPending.set(imagePath, mountPoint);
          }
        }
        if (result.length === 0 && hadEntries) {
          return {
            mountMap: next,
            pendingMounts: prunedPending,
            volumes: writable,
            lastRefreshedAt: Date.now(),
            error: null,
          };
        }
        return {
          entries: result,
          mountMap: next,
          pendingMounts: prunedPending,
          volumes: writable,
          lastRefreshedAt: Date.now(),
          error: null,
        };
      });
    } catch (e) {
      if (isStale()) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      // Leave `loading` alone if we've been superseded — the new
      // host's own refresh now owns the loading indicator.
      if (!isStale()) setLoading(false);
    }
  }, [host, setLoading, setError]);

  // Auto-refresh policy:
  //   - First time Library is visited after mount → always refresh
  //   - Subsequent visits → only refresh if data is older than 30s
  //     (or never loaded). Keeps "flip Library ↔ another tab" snappy
  //     without serving week-old data after the app has been open
  //     for a while.
  useEffect(() => {
    if (payloadStatus !== "up") return;
    const stale =
      lastRefreshedAt === null || Date.now() - lastRefreshedAt > 30_000;
    if (stale) refresh();
  }, [payloadStatus, lastRefreshedAt, refresh]);

  // Listen for Install Package completion events. The install queue
  // dispatches `ps5upload:library:invalidate` when a BGFT install
  // reports phase=done — and a fresh title appears in /user/app/.
  // We force a refresh on the next render even if the 30-s stale
  // window hasn't elapsed, so the just-installed title shows up
  // without the user having to click Refresh.
  useEffect(() => {
    function onInvalidate() {
      if (payloadStatus === "up") refresh();
    }
    window.addEventListener("ps5upload:library:invalidate", onInvalidate);
    return () => {
      window.removeEventListener(
        "ps5upload:library:invalidate",
        onInvalidate,
      );
    };
  }, [payloadStatus, refresh]);

  // Live search filter. Matches `query` against name / titleId /
  // path / scope / volume so users can find a title by any of the
  // identifiers they happen to remember (the folder name they
  // uploaded, the PPSA id from psprices, the drive it lives on, …).
  // Kept transient — closing + reopening the screen clears the
  // query, so a stale filter doesn't surprise the user later.
  const [query, setQuery] = useState("");

  // Filter, then split by kind so games and disk images render in
  // their own sections. Memoization keys on the entries array
  // identity (set by setData on refresh) and the query string —
  // both cheap, neither flips on every render.
  // Sort key persisted to localStorage so reopening the screen
  // remembers the user's last preference. Default is "recent" so
  // freshly-uploaded games appear at the top — the most common
  // "where did the thing I just uploaded go" experience.
  type SortKey = "recent" | "oldest" | "name-asc" | "name-desc";
  const [sortKey, setSortKey] = useState<SortKey>(() => {
    if (typeof window === "undefined") return "recent";
    const saved = window.localStorage.getItem("ps5upload.library.sort");
    return saved === "recent" ||
      saved === "oldest" ||
      saved === "name-asc" ||
      saved === "name-desc"
      ? saved
      : "recent";
  });
  useEffect(() => {
    if (typeof window !== "undefined") {
      window.localStorage.setItem("ps5upload.library.sort", sortKey);
    }
  }, [sortKey]);

  function applySort(arr: LibraryEntry[]): LibraryEntry[] {
    const sorted = [...arr];
    if (sortKey === "name-asc") {
      sorted.sort((a, b) => a.name.localeCompare(b.name));
    } else if (sortKey === "name-desc") {
      sorted.sort((a, b) => b.name.localeCompare(a.name));
    } else if (sortKey === "recent") {
      // mtime=0 means unknown (older payload, stat failure) — sort
      // those to the bottom so they don't pollute the "recent" top.
      sorted.sort((a, b) => {
        const am = a.mtime ?? 0;
        const bm = b.mtime ?? 0;
        if (am === 0 && bm === 0) return a.name.localeCompare(b.name);
        if (am === 0) return 1;
        if (bm === 0) return -1;
        return bm - am;
      });
    } else {
      // oldest first — same null-aware ordering, reversed.
      sorted.sort((a, b) => {
        const am = a.mtime ?? 0;
        const bm = b.mtime ?? 0;
        if (am === 0 && bm === 0) return a.name.localeCompare(b.name);
        if (am === 0) return 1;
        if (bm === 0) return -1;
        return am - bm;
      });
    }
    return sorted;
  }

  const split = useMemo(() => {
    const filtered = filterLibraryEntries(entries ?? [], query);
    const games = applySort(filtered.filter((e) => e.kind === "game"));
    const images = applySort(filtered.filter((e) => e.kind === "image"));
    return { games, images, total: filtered.length };
    // applySort closes over sortKey — eslint-deps catches it via the
    // sortKey dep below, no need to memoize the function itself.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [entries, query, sortKey]);
  const totalUnfiltered = entries?.length ?? 0;
  const querying = query.trim() !== "";

  return (
    <div className="p-6">
      <PageHeader
        icon={LibraryBig}
        title={tr("library", undefined, "Library")}
        count={entries?.length}
        loading={loading}
        description={tr(
          "library_description",
          undefined,
          "Games and disk images anywhere on your PS5. Games are folders containing sce_sys/param.json; disk images are .exfat, .ffpkg, and .ffpfs files.",
        )}
        right={
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<RefreshCw size={12} />}
            onClick={refresh}
            disabled={loading || !host?.trim()}
            loading={loading}
          >
            {tr("refresh", undefined, "Refresh")}
          </Button>
        }
      />

      {error && (
        <div className="mb-4">
          <ErrorCard
            title={tr("library_scan_error", undefined, "Couldn't scan the PS5")}
            detail={error}
          />
        </div>
      )}

      {/* ShadowMount+ awareness panel — auto-hides when SMP isn't
          installed, expands inline when it is. Read-only. Mounts
          here because SMP's mounted-image dirs (under /mnt/shadowmnt)
          are exactly the things this Library tab lists. */}
      <SmpPanel mgmtAddr={host?.trim() ? `${host.trim()}:9114` : null} />

      {/* Running apps with suspend/resume/kill controls. Auto-hides
          when nothing is running. Wires Phase 18 (app lifecycle RPCs)
          + Phase 33 (app.db query) so each row shows a friendly name. */}
      {host?.trim() && payloadStatus === "up" && (
        <RunningAppsPanel mgmtAddr={`${host.trim()}:9114`} />
      )}

      {entries === null && !loading && !error && (
        <EmptyState
          message={tr(
            "library_waiting",
            undefined,
            "Waiting for the PS5 payload to become reachable…",
          )}
        />
      )}

      {entries && entries.length === 0 && (
        <EmptyState
          icon={LibraryBig}
          size="hero"
          title={tr("library_empty_title", undefined, "Nothing in the scan folders yet")}
          message={tr(
            "library_empty_message",
            undefined,
            "Upload a game folder or disk image, or register titles with a PS5-side installer — they'll show up here.",
          )}
        />
      )}

      {entries && entries.length > 0 && (
        <>
          {/* Search bar. Matches name / titleId / path / scope /
              volume so a query like "dead", "PPSA03845", or "ext1"
              all narrow the list. Empty query renders the full list
              unchanged (filterLibraryEntries returns the same
              reference, so memoization downstream stays warm). */}
          <div className="mb-4 flex items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] px-3 py-2">
            <Search size={14} className="shrink-0 text-[var(--color-muted)]" />
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                // Standard search-input convention: Escape clears
                // the query. Match what the X button does so the
                // keyboard path mirrors the mouse path.
                if (e.key === "Escape" && query !== "") {
                  setQuery("");
                  e.stopPropagation();
                }
              }}
              placeholder={tr(
                "library_search_placeholder",
                undefined,
                "Search by name, title ID, or path…",
              )}
              className="flex-1 bg-transparent text-sm outline-none placeholder:text-[var(--color-muted)]"
              aria-label={tr(
                "library_search_aria",
                undefined,
                "Filter library entries",
              )}
            />
            {querying && (
              <button
                type="button"
                onClick={() => setQuery("")}
                className="rounded p-0.5 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)]"
                aria-label={tr(
                  "library_search_clear",
                  undefined,
                  "Clear search",
                )}
              >
                <X size={12} />
              </button>
            )}
            {querying && (
              <span className="shrink-0 text-xs text-[var(--color-muted)]">
                {split.total} / {totalUnfiltered}
              </span>
            )}
            <select
              value={sortKey}
              onChange={(e) => setSortKey(e.target.value as SortKey)}
              className="shrink-0 rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-0.5 text-xs text-[var(--color-text)] outline-none hover:bg-[var(--color-surface-3)]"
              aria-label={tr(
                "library_sort_aria",
                undefined,
                "Sort library entries",
              )}
              title={tr(
                "library_sort_tooltip",
                undefined,
                "Choose how the list is ordered. 'Most recent' uses each entry's last-modified time on the PS5.",
              )}
            >
              <option value="recent">
                {tr("library_sort_recent", undefined, "Most recent")}
              </option>
              <option value="oldest">
                {tr("library_sort_oldest", undefined, "Oldest first")}
              </option>
              <option value="name-asc">
                {tr("library_sort_name_asc", undefined, "Name (A→Z)")}
              </option>
              <option value="name-desc">
                {tr("library_sort_name_desc", undefined, "Name (Z→A)")}
              </option>
            </select>
          </div>

          {querying && split.total === 0 && (
            <EmptyState
              icon={Search}
              title={tr(
                "library_search_no_matches_title",
                undefined,
                "No matches",
              )}
              message={tr(
                "library_search_no_matches_message",
                { query },
                `Nothing in your library matches "${query}". Try a shorter phrase, the title ID (e.g. PPSA01342), or a fragment of the path.`,
              )}
            />
          )}

          {split.total > 0 && (
            <div className="flex flex-col gap-6">
              {split.games.length > 0 && (
                <section>
                  <SectionHeader
                    icon={<Gamepad2 size={13} />}
                    title={tr("library_games", undefined, "Games")}
                    count={split.games.length}
                  />
                  <div className="grid gap-2">
                    {split.games.map((e, i) => (
                      <LibraryRow
                        key={`${e.path}-${i}`}
                        entry={e}
                        host={host}
                        mountMap={mountMap}
                        pendingMounts={pendingMounts}
                        volumes={volumes}
                        onChanged={refresh}
                      />
                    ))}
                  </div>
                </section>
              )}
              {split.images.length > 0 && (
                <section>
                  <SectionHeader
                    icon={<FileArchive size={13} />}
                    title={tr("library_disk_images", undefined, "Disk images (.exfat / .ffpkg / .ffpfs)")}
                    count={split.images.length}
                  />
                  <FpkgKstuffTip />
                  <div className="grid gap-2">
                    {split.images.map((e, i) => (
                      <LibraryRow
                        key={`${e.path}-${i}`}
                        entry={e}
                        host={host}
                        mountMap={mountMap}
                        pendingMounts={pendingMounts}
                        volumes={volumes}
                        onChanged={refresh}
                      />
                    ))}
                  </div>
                </section>
              )}
            </div>
          )}
        </>
      )}
    </div>
  );
}

function SectionHeader({
  icon,
  title,
  count,
}: {
  icon: React.ReactNode;
  title: string;
  count: number;
}) {
  return (
    <header className="mb-2 flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
      {icon}
      <span>{title}</span>
      <span className="text-[10px] font-normal normal-case">· {count}</span>
    </header>
  );
}

interface PendingConfirm {
  kind: "delete" | "chmod";
  entry: LibraryEntry;
}

/** Two-phase Move busy state. We split the spinner messages so the
 *  user knows whether the destructive `fsDelete` of the source has
 *  started — once "Cleaning up source…" shows, the new copy is
 *  already on disk and the source is being torn down. */
type BusyState =
  | null
  | "delete"
  | "chmod"
  | "mount"
  | "unmount"
  | "move-copying"
  | "move-deleting"
  | "download"
  | "launch"
  | "register"
  | "unregister";

interface DownloadProgress {
  bytesReceived: number;
  totalBytes: number;
}

function LibraryRow({
  entry,
  host,
  mountMap,
  pendingMounts,
  volumes,
  onChanged,
}: {
  entry: LibraryEntry;
  host: string;
  /** image_path → mount_point from the volumes probe (authoritative).
   *  Lets image rows render MOUNTED state + offer the right action
   *  (Mount vs Unmount). */
  mountMap: Map<string, string>;
  /** image_path → mount_point for fs_mount calls the client just
   *  made but the probe hasn't surfaced yet. The row reads the union
   *  so a freshly-mounted image flips to MOUNTED in the same render
   *  the user clicks (no waiting on the next probe). */
  pendingMounts: Map<string, string>;
  /** Writable PS5 volumes — surfaced to the Move modal as the
   *  destination dropdown. */
  volumes: Volume[];
  onChanged: () => void;
}) {
  const tr = useTr();
  const Icon = entry.kind === "game" ? Gamepad2 : FileArchive;
  const runningTitleIds = useRunningAppsStore((s) => s.titleIds);
  const isTitleRunning =
    entry.kind === "game" &&
    !!entry.titleId &&
    runningTitleIds.has(entry.titleId);
  const playSeconds = usePlayTimeStore((s) =>
    entry.kind === "game" && entry.titleId
      ? s.seconds[entry.titleId]
      : undefined,
  );
  const kindLabel =
    entry.kind === "game"
      ? tr("library_row_kind_game", undefined, "Game")
      : entry.imageFormat === "ffpkg"
        ? tr("library_row_kind_ffpkg", undefined, ".ffpkg image")
        : entry.imageFormat === "ffpfs"
          ? tr("library_row_kind_ffpfs", undefined, ".ffpfs image")
          : tr("library_row_kind_exfat", undefined, ".exfat image");

  /** If this entry lives inside a currently-mounted disk image,
   *  return the backing image's path. Lets the row render a
   *  visually-distinct "from disk image" badge so the user can
   *  tell uploaded-folder games apart from games-inside-a-mount-
   *  that-disappears-when-unmounted. Uses the same `findOwningImage`
   *  helper that lives in state/library.ts so this badge stays in
   *  sync with the row's MOUNTED-state semantics — both ultimately
   *  read from the same union of probe + pending mounts. Memoized
   *  so the per-row reverse-walk doesn't run on every sort/query
   *  change. */
  const fromImagePath = useMemo(
    () =>
      entry.kind === "game"
        ? findOwningImage({ mountMap, pendingMounts }, entry.path)
        : null,
    [entry.kind, entry.path, mountMap, pendingMounts],
  );
  const fromImageBasename = fromImagePath
    ? (fromImagePath.split("/").pop() ?? fromImagePath)
    : null;
  const [confirm, setConfirm] = useState<PendingConfirm | null>(null);
  const [busy, setBusy] = useState<BusyState>(null);
  const [error, setError] = useState<string | null>(null);
  const [mountNote, setMountNote] = useState<string | null>(null);
  const [meta, setMeta] = useState<GameMeta | null>(null);
  const [moveOpen, setMoveOpen] = useState(false);
  const [mountOpen, setMountOpen] = useState(false);
  const [detailsOpen, setDetailsOpen] = useState(false);
  const [downloadProgress, setDownloadProgress] =
    useState<DownloadProgress | null>(null);
  // Cancellation flag for the download poll loop. The loop runs for
  // the entire duration of the engine job (potentially minutes for a
  // multi-GiB game folder). Without this, navigating away from the
  // Library mid-download leaves an orphan loop calling setError /
  // setBusy on an unmounted component — React 18 warns and the next
  // mount of the same row inherits no state context.
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);
  // User-requested abort for the download poll loop. The Stop button
  // sets this to true; the loop's next iteration sees it and exits
  // cleanly (the engine job continues server-side — no engine-side
  // cancel API today, so the .part file may still finish landing).
  const downloadStopRef = useRef(false);
  // Per-row Move progress (bytes copied from the in-flight PS5 fs_copy)
  // — fed by the FS_OP_STATUS poller spawned in runMove. null when no
  // move is in flight or the poller hasn't seen a reply yet.
  const [moveProgress, setMoveProgress] =
    useState<{ bytesCopied: number; totalBytes: number } | null>(null);
  // Set when classifyMovePollError decides the running payload is
  // too old to drive byte-progress (predates the FS_OP_STATUS handler
  // in 2.2.7, or the FS_OP_STATUS_ACK body buffer fix in 2.2.16). The
  // banner uses `moveProgressUnsupportedThreshold` to render the
  // matching version in its hint so the user knows which fix they're
  // missing. Stays null on transient errors / current payloads —
  // we'd rather show no banner than gaslight a user on the latest
  // payload (the original bug this rework addresses).
  const [moveProgressUnsupportedThreshold, setMoveProgressUnsupportedThreshold] =
    useState<"2.2.7" | "2.2.16" | null>(null);
  // Read the running-payload version from the Connection store so the
  // poller can decide whether a poll error reflects a real old-payload
  // (latch the banner) or just a transient hiccup on a current build
  // (retry / give up silently). Set by the Connection screen's STATUS
  // probe; null until the probe completes or on a payload too old to
  // report a version.
  const payloadVersion = useConnectionStore((s) => s.payloadVersion);
  // User-requested abort for the move's in-flight fs_copy. Set by the
  // Stop button; the side-watcher fires fsOpCancel as soon as it
  // observes this flip, and the payload's cp_rf bails within ~one
  // 16 MiB buffer.
  const moveStopRef = useRef(false);
  const elapsedMs = useElapsed(busy !== null);

  /** Current mount point for this entry (null = not mounted). Only
   *  meaningful for image rows — games don't go through fs_mount.
   *  Probe-derived `mountMap` is authoritative when present;
   *  `pendingMounts` covers the post-fs_mount window where the
   *  probe hasn't caught up yet so the row immediately reflects a
   *  successful Mount click. */
  const currentMount = entry.kind === "image"
    ? mountMap.get(entry.path) ?? pendingMounts.get(entry.path) ?? null
    : null;
  const isMounted = currentMount !== null;

  // Metadata fetch: skipped for disk images (single file, no sce_sys
  // inside) and for generic "folder" kind under non-game scopes.
  // Cancellation guard prevents a late response from overwriting state
  // after the row unmounts or the host changes. Errors are
  // expected-degraded (fetchGameMeta returns a blank GameMeta when
  // the parse fails) — but a thrown promise (Tauri invoke threw,
  // network drop) needs an explicit catch or it'd surface as an
  // unhandled rejection in the dev console.
  useEffect(() => {
    if (entry.kind === "image") return;
    if (!host?.trim()) return;
    let cancelled = false;
    metaLimit(() =>
      fetchGameMeta(`${host}:${PS5_PAYLOAD_PORT}`, entry.path),
    )
      .then((m) => {
        if (!cancelled) setMeta(m);
      })
      .catch((e) => {
        // Don't surface to the row UI — the meta block degrades
        // gracefully to "show entry.name" without it. Just log so
        // the rejection doesn't hit the unhandled-promise channel.
        if (!cancelled) {
          console.warn(`[library] meta fetch failed for ${entry.path}:`, e);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [entry.kind, entry.path, host]);

  const runDelete = async () => {
    setBusy("delete");
    setError(null);
    // Library row Delete shows up in the global OperationBar so the
    // user can see "Deleting Dead Space" while navigating away mid-
    // delete. fs-delete is the same kind FileSystem bulk + single
    // delete use; the label disambiguates by name.
    const activityId = useActivityHistoryStore
      .getState()
      .start("library-delete", `Deleting ${entry.name}`, {
        fromPath: entry.path,
        files: 1,
      });
    let okOutcome = true;
    let errMsg: string | null = null;
    try {
      await fsDelete(`${host}:${PS5_PAYLOAD_PORT}`, entry.path);
      onChanged();
    } catch (e) {
      okOutcome = false;
      errMsg = e instanceof Error ? e.message : String(e);
      setError(humanizePs5Error(errMsg));
    } finally {
      useActivityHistoryStore
        .getState()
        .finish(activityId, okOutcome ? "done" : "failed", {
          error: errMsg ?? undefined,
        });
      setBusy(null);
      setConfirm(null);
    }
  };
  const runChmod = async () => {
    setBusy("chmod");
    setError(null);
    // Recursive chmod on a 100k-file game folder takes seconds-to-
    // minutes on PS5 UFS — long enough that the user needs to know
    // the op is still running if they tab away.
    const activityId = useActivityHistoryStore
      .getState()
      .start("library-chmod", `Setting permissions on ${entry.name}`, {
        fromPath: entry.path,
      });
    let okOutcome = true;
    let errMsg: string | null = null;
    try {
      await fsChmod(
        `${host}:${PS5_PAYLOAD_PORT}`,
        entry.path,
        "0777",
        entry.kind !== "image" // recursive on dirs, single-file on disk images
      );
      onChanged();
    } catch (e) {
      okOutcome = false;
      errMsg = e instanceof Error ? e.message : String(e);
      setError(humanizePs5Error(errMsg));
    } finally {
      useActivityHistoryStore
        .getState()
        .finish(activityId, okOutcome ? "done" : "failed", {
          error: errMsg ?? undefined,
        });
      setBusy(null);
      setConfirm(null);
    }
  };

  // Mount: the payload does this itself — attaches /dev/md<N> over the
  // image file, nmount's with exfatfs (.exfat) or ufs (.ffpkg), puts the
  // result under the chosen mount path (or /mnt/ps5upload/<name>/ if
  // the user accepts the default / runs an old payload). External
  // third-party scene tools are NOT involved — the payload's own
  // MD/LVD attach + nmount pipeline does the whole thing.
  // We surface the returned mount_point so the user knows where to
  // find it.
  //
  // Resolution rules (mirror fsMount's contract):
  //   - opts.mountPoint set → mount at that exact path (2.2.25+).
  //   - opts.mountName set → mount under /mnt/ps5upload/<name>/.
  //   - both null → payload derives a name from the image basename
  //     and mounts at /mnt/ps5upload/<derived>/.
  //
  // `opts.persistDest` (when set) is what the modal hands us so we
  // can save the user's preset choice without re-deriving it from
  // the resolved path. The reverse-derive path was fragile against
  // a stale/incomplete `volumes` list (a probe that hadn't completed
  // yet meant the longest-prefix match returned undefined and the
  // save was silently skipped). Modal state is the source of truth.
  const runMount = async (
    opts: {
      mountName?: string;
      mountPoint?: string;
      readOnly?: boolean;
      persistDest?: { volume: string; subpath: string };
    } = {},
  ) => {
    if (entry.kind !== "image") return;
    setMountOpen(false);
    setBusy("mount");
    setError(null);
    setMountNote(null);
    const addr = `${host}:${PS5_PAYLOAD_PORT}`;
    // Mount is usually fast (~1-2s for the lvd attach + nmount) but
    // a misbehaving image can hang for the FS_MOUNT timeout (30 s).
    // Tracking it gives the user a global "still running" signal
    // for the slow case.
    const activityId = useActivityHistoryStore
      .getState()
      .start("library-mount", `Mounting ${entry.name}`, {
        fromPath: entry.path,
        toPath: opts.mountPoint,
      });
    let okOutcome = true;
    let errMsg: string | null = null;
    try {
      // First-mount-of-a-session for an image path occasionally hits
      // a transient lvd/md driver init or device-node race that
      // cleanly succeeds on the very next call (~50–500 ms later).
      // Users have reported this as "click Mount, see error, click
      // again, works." Retry once with a brief backoff to absorb
      // those without a UI round-trip; the second failure is
      // surfaced normally.
      //
      // Also retry once on `fs_mount_source_unstable` — the payload
      // gate that refuses to mount a file whose mtime is < N
      // seconds old. We disabled that gate in the 2.2.39 payload
      // (the COMMIT_TX_ACK is the real stability signal), but
      // older payloads still reject; honoring the suggested wait
      // and retrying transparently keeps the UX clean for users
      // who haven't pushed the new payload yet.
      let res;
      try {
        res = await fsMount(addr, entry.path, opts);
      } catch (firstErr) {
        const msg = firstErr instanceof Error ? firstErr.message : String(firstErr);
        const sourceUnstable = /fs_mount_source_unstable/i.test(msg);
        // Only retry on errors that are plausibly transient. A retry
        // for `path_not_allowed`, `disk_full`, `mount_point_busy`,
        // `bad_image_magic`, etc. just doubles user-visible latency
        // before the same error surfaces. Allowlist of what we know
        // benefits from a wait-and-retry.
        const transient =
          sourceUnstable ||
          /\b(driver_init|device_busy|temporarily|EAGAIN|EBUSY)\b/i.test(msg);
        if (!transient) {
          throw firstErr;
        }
        // 3500 ms covers the legacy 3-second payload gate plus a
        // small safety margin; 350 ms is enough for transient
        // driver-init races. Picked branch matches the user-
        // visible cause so the retry's wait isn't a black box.
        const waitMs = sourceUnstable ? 3500 : 350;
        if (sourceUnstable) {
          setMountNote(
            `Image was modified seconds ago — waiting ${(waitMs / 1000).toFixed(1)}s for the upload to settle, then retrying mount…`,
          );
        }
        await new Promise((resolve) => setTimeout(resolve, waitMs));
        // Don't wrap-and-rethrow — let the outer catch handle the
        // retry's failure naturally. The retry's error (disk full,
        // bad mount point, etc.) is what the user actually needs
        // to see; the prior shape that caught and rethrew firstErr
        // hid the real second-attempt cause and sent users down
        // the wrong debug path.
        res = await fsMount(addr, entry.path, opts);
      }
      // Optimistic mountMap update — flips the row to MOUNTED +
      // Unmount-button immediately, before the background rescan
      // refreshes volumes. The volumes-driven authoritative state
      // overwrites this on the next setData(), but the user gets
      // an instantly-correct UI in the meantime.
      useLibraryStore
        .getState()
        .addMount(entry.path, res.mount_point);
      // The activity entry started with toPath = opts.mountPoint
      // which is undefined on the legacy fall-through (no
      // user-chosen path — the payload picks
      // /mnt/ps5upload/<derived>). Patch toPath to the actually-
      // resolved mount point so the Activity tab shows "From: …\n
      // To: …" instead of dangling "From:" alone.
      useActivityHistoryStore
        .getState()
        .update(activityId, { toPath: res.mount_point });
      // Persist the (volume, subpath) the modal handed us so the
      // next Mount on this host opens with the same selection.
      // Source of truth is the modal's own state — `volumes` may
      // still be loading at this point and reverse-deriving from
      // the resolved path would silently skip the save when the
      // longest-prefix match found nothing.
      if (opts.persistDest) {
        saveMountDest(host, opts.persistDest);
      }
      // The payload echoes `reused: true` when it short-circuited a
      // re-mount of the same image_path. Surface that distinctly so
      // the user understands "Mount" was a no-op (the image was
      // already mounted), not a new mount they should expect to see
      // appear in Volumes for the first time.
      const roSuffix = res.read_only ? " (read-only)" : "";
      // Image-layout pre-flight (added 2.2.32 payload). When the
      // payload reports `layout_valid: false`, the image was built
      // with an extra top-level folder so Register + Launch will
      // fail. Surface the warning prominently so the user fixes the
      // image instead of registering + retrying. Older payloads omit
      // the field; engine defaults it to `true` so a missing value
      // doesn't false-warn on pre-2.2.32 mounts.
      const layoutWarning =
        res.layout_valid === false
          ? " ⚠ Image is missing sce_sys/param.json at root — Register/Launch will fail. Re-build the image with files at root (no extra folder)."
          : "";
      // Mount-point mismatch warning. Triggered when the user
      // picked a mountPoint via the modal but the payload landed
      // the mount somewhere different — the most common cause is
      // running an older payload that ignored the `mount_point`
      // field and silently fell back to /mnt/ps5upload/<name>.
      // Surfacing the mismatch (rather than just showing the
      // landed path) makes the cause obvious so the user knows
      // why their /mnt/usb0 pick didn't take effect, and what to
      // do (update the payload).
      const mismatch =
        opts.mountPoint && res.mount_point !== opts.mountPoint
          ? ` ⚠ Note: payload landed the mount at ${res.mount_point} instead of the path you picked (${opts.mountPoint}). This usually means the running payload predates 2.2.25 and silently ignored the mount-point. Push the bundled payload from Connection → Refresh to update.`
          : "";
      // Kernel-RO surprise. The user picked RW but the mount came
      // back read-only — Sony's UFS_DOWNLOAD_DATA image_type does
      // this on some firmwares regardless of the LVD-RW flag we
      // pass. Surfacing the kernel state lets the user understand
      // why writes (e.g. registering as a game folder) won't land.
      // Suppressed when the user explicitly asked for RO. Field is
      // optional so older payloads without the diagnostic don't
      // false-warn.
      const kernelRoNote =
        res.kernel_ro && !res.read_only
          ? ` ⚠ Kernel mounted this read-only despite the RW pick — common for UFS .ffpkg images on some firmwares. Reads (and the Library scan) work; writes through the mount will fail.`
          : "";
      // Geometry diagnostics. Show the f_bsize / f_iosize so a user
      // reporting "mount succeeded but games are invisible" can
      // share concrete numbers without ssh — sector-size mismatches
      // between the .ffpkg image and the LVD device are the leading
      // suspect for empty-mount symptoms. Only shown when the
      // payload provides them (2.2.52+); pre-2.2.52 mounts simply
      // omit the diagnostic line.
      const geomNote =
        res.f_bsize && res.f_bsize > 0
          ? ` (f_bsize=${res.f_bsize}${res.f_iosize && res.f_iosize !== res.f_bsize ? `, f_iosize=${res.f_iosize}` : ""})`
          : "";
      setMountNote(
        res.reused
          ? `Already mounted at ${res.mount_point}${roSuffix}${geomNote} — reused the existing mount.${layoutWarning}${mismatch}${kernelRoNote}`
          : `Mounted at ${res.mount_point}${roSuffix}${geomNote}. Games inside the image will appear in the Library shortly.${layoutWarning}${mismatch}${kernelRoNote}`,
      );
      // Delay the rescan + retry once. The freshly-mounted volume
      // can take 1-2 seconds to surface in getmntinfo on some
      // firmwares, and the games inside aren't readable until the
      // ufs/exfatfs cache warms. The first refresh at ~1s gives
      // most cases a chance; the second at ~3s is a safety net so
      // the user sees the game appear without manually clicking
      // Refresh. Pre-2.2.48 a single 400 ms delay sometimes ran
      // before the volume was even visible, producing the
      // user-reported "I can see no game folder at the mount,
      // refresh doesn't help" symptom.
      //
      // Both timers gate on `mountedRef.current` so a user who
      // navigates away mid-mount doesn't leave orphan refreshes
      // hammering the network on a dead component.
      setTimeout(() => {
        if (mountedRef.current) onChanged();
      }, 1000);
      setTimeout(() => {
        if (mountedRef.current) onChanged();
      }, 3000);
    } catch (e) {
      okOutcome = false;
      errMsg = e instanceof Error ? e.message : String(e);
      setError(humanizePs5Error(errMsg));
    } finally {
      useActivityHistoryStore
        .getState()
        .finish(activityId, okOutcome ? "done" : "failed", {
          error: errMsg ?? undefined,
        });
      setBusy(null);
    }
  };

  /** Move: copy → verify-by-success → delete-with-retry. The payload's
   *  fs_copy is a sync command — it doesn't return until the recursive
   *  copy is complete and refuses to overwrite an existing destination,
   *  so a successful return means the destination has the bytes and we
   *  can safely tear down the source. fs_delete gets up to 3 attempts
   *  with linear backoff because a transient busy state (something on
   *  the PS5 side holding the source dir open) shouldn't strand a copy
   *  that already succeeded; persistent failure surfaces both paths so
   *  the user can clean up manually instead of guessing what happened. */
  const runMove = async (destPath: string) => {
    setMoveOpen(false);
    setBusy("move-copying");
    setError(null);
    setMountNote(null);
    setMoveProgress({ bytesCopied: 0, totalBytes: 0 });
    setMoveProgressUnsupportedThreshold(null);
    moveStopRef.current = false;
    const addr = `${host}:${PS5_PAYLOAD_PORT}`;
    // Generate a unique op_id so the payload can stamp the in-flight
    // fs_copy with it; we can then poll FS_OP_STATUS for live byte
    // progress and fire FS_OP_CANCEL on Stop.
    const opId = Math.floor(Math.random() * Number.MAX_SAFE_INTEGER);
    // Record into the cross-screen Activity log so the OperationBar
    // and Activity tab can show this op alongside FS bulk + transfer
    // ops. Library has its own component-local state, so without
    // this entry the move would never appear in the global view.
    // Storing op_id + addr lets the Activity tab's Stop button call
    // fsOpCancel directly without needing a reference back to this
    // component.
    const activityId = useActivityHistoryStore
      .getState()
      .start("library-move", `Moving ${entry.name}`, {
        fromPath: entry.path,
        toPath: destPath,
        opId,
        addr,
      });
    let pollerStopped = false;
    const pollerDone = (async () => {
      // Small initial delay before the first poll. The payload now
      // registers the op slot *before* the recursive_size walk
      // (runtime.c handle_fs_copy), so this delay is no longer
      // load-bearing for the register race — it just amortizes the
      // engine connect cost so we don't pay it twice (once for poll,
      // once for the FS_COPY frame the engine sends in parallel).
      await new Promise((r) => setTimeout(r, 250));
      // Tracks back-to-back poll failures (non-404). Reset on every
      // successful snapshot. The threshold lives in
      // movePollerPolicy.ts so it can be shared with tests.
      let consecutiveFailures = 0;
      while (!pollerStopped) {
        try {
          const snap = await fsOpStatus(addr, opId);
          consecutiveFailures = 0;
          if (mountedRef.current) {
            setMoveProgress({
              bytesCopied: snap.bytes_copied,
              totalBytes: snap.total_bytes,
            });
          }
          // Mirror to Activity so the OperationBar / Activity tab
          // tick in lockstep with the local row.
          useActivityHistoryStore.getState().update(activityId, {
            bytes: snap.bytes_copied,
            totalBytes: snap.total_bytes,
          });
        } catch (e) {
          const msg = e instanceof Error ? e.message : String(e);
          // 404 / "not in flight" is the engine's surface for the
          // payload's `{found:false}` ACK. That happens twice in a
          // healthy op: briefly at the start (frame in flight before
          // fs_op_register; rare now that the payload registers
          // before walking) and once at the end (slot released as
          // the FS_COPY handler returns). Don't count either toward
          // the consecutive-failure budget.
          if (isExpectedNotInFlight(msg)) {
            await new Promise((r) => setTimeout(r, 500));
            continue;
          }
          consecutiveFailures += 1;
          const outcome = classifyMovePollError(
            payloadVersion,
            msg,
            consecutiveFailures,
          );
          if (outcome.kind === "stop-old-payload") {
            // The version probe (or the error-string fallback) says
            // the running payload is too old to drive progress.
            // Latch the banner with the matching threshold so the
            // user can see exactly which fix they're missing.
            if (mountedRef.current) {
              setMoveProgressUnsupportedThreshold(outcome.threshold);
            }
            useActivityHistoryStore.getState().update(activityId, {
              error:
                outcome.threshold === "2.2.16"
                  ? "Live progress unavailable — payload predates 2.2.16 FS_OP_STATUS_ACK fix. Click Replace payload on the Connection screen, or run `make send-payload`."
                  : "Live progress unavailable — payload predates 2.2.7 FS_OP_STATUS. Click Replace payload on the Connection screen.",
            });
            break;
          }
          if (outcome.kind === "stop-silent") {
            // Healthy or unknown payload, repeated transient
            // failures. Stop polling but don't show a banner — the
            // move continues on its own connection regardless of
            // whether we can render progress. Console-warn so the
            // failure is visible during debug instead of swallowed.
            console.warn(
              `[library] FS_OP_STATUS poll gave up after ${consecutiveFailures} consecutive failures:`,
              msg,
            );
            break;
          }
          // outcome.kind === "retry" — log only the first one in a
          // run so we don't spam the console at 2 Hz on a sustained
          // outage.
          if (consecutiveFailures === 1) {
            console.warn("[library] FS_OP_STATUS poll failed (will retry):", msg);
          }
        }
        await new Promise((r) => setTimeout(r, 500));
      }
    })();
    const cancelWatcher = (async () => {
      while (!pollerStopped) {
        if (moveStopRef.current) {
          try {
            await fsOpCancel(addr, opId);
          } catch {
            // Best effort — the payload's cp_rf will still bail at
            // its next cancel check via the in-band flag set by
            // the engine's RPC; even if our cancel call lost the
            // race or hit a transient error, the user-visible
            // "Stop" goal is met by the between-iterations check.
          }
          break;
        }
        await new Promise((r) => setTimeout(r, 200));
      }
    })();
    let copyOk = true;
    let copyErr: unknown = null;
    try {
      await fsCopy(addr, entry.path, destPath, opId);
    } catch (e) {
      copyOk = false;
      copyErr = e;
    } finally {
      pollerStopped = true;
      await Promise.allSettled([pollerDone, cancelWatcher]);
      if (mountedRef.current) setMoveProgress(null);
    }
    if (!copyOk) {
      const msg = copyErr instanceof Error ? copyErr.message : String(copyErr);
      // The payload returns "fs_copy_cancelled" → engine maps to
      // 409 with body "cancelled". Surface that as a user-facing
      // "you stopped this" rather than a generic copy failure.
      if (msg.includes("cancelled")) {
        setError(
          tr(
            "library_move_cancelled",
            undefined,
            "Move cancelled. The source is unchanged.",
          ),
        );
        useActivityHistoryStore.getState().finish(activityId, "stopped", {
          error: "cancelled by user",
        });
      } else {
        setError(
          tr(
            "library_move_copy_failed",
            { error: msg },
            "Couldn't copy to the new location: {error}. Source is unchanged.",
          ),
        );
        useActivityHistoryStore.getState().finish(activityId, "failed", {
          error: msg,
        });
      }
      setBusy(null);
      return;
    }
    setBusy("move-deleting");
    const delResult = await deleteWithRetry({
      deleter: () => fsDelete(addr, entry.path),
      onAttemptFail: (attempt, e) =>
        console.warn(
          `[library] move delete attempt ${attempt}/3 for ${entry.path} failed:`,
          e,
        ),
    });
    if (!delResult.ok) {
      const lastErr = delResult.lastError;
      const lastErrMsg =
        lastErr instanceof Error ? lastErr.message : String(lastErr);
      setError(
        tr(
          "library_move_delete_failed",
          {
            dest: destPath,
            src: entry.path,
            error: lastErrMsg,
          },
          "Copied to {dest}, but couldn't remove the source {src} after 3 attempts: {error}. Both copies now exist — delete the original yourself when ready.",
        ),
      );
      // Treat a "copied but couldn't delete source" as a partial
      // failure so the Activity tab makes the duplicate-files
      // situation visible.
      useActivityHistoryStore.getState().finish(activityId, "failed", {
        error: `copy ok, source delete failed: ${lastErrMsg}`,
      });
      setBusy(null);
      onChanged();
      return;
    }
    setMountNote(
      tr(
        "library_move_succeeded",
        { dest: destPath },
        "Moved to {dest}.",
      ),
    );
    // Explicitly clear `error` on the success finish. The poller may
    // have written a "Live progress unavailable — payload predates
    // 2.2.16…" *note* into the entry's error field while the move
    // was running (see lines ~631; the field doubles as a "note"
    // during running per the design comment). finish() spreads its
    // extras over the entry, so without this the successful "done"
    // entry would persist that stale note as its terminal error
    // message — the Activity tab would render a green checkmark
    // next to a red-looking error string, which is confusing.
    useActivityHistoryStore
      .getState()
      .finish(activityId, "done", { error: undefined });
    setBusy(null);
    onChanged();
  };

  /** Save a copy of this library entry to the host. Games are
   *  folders (recursive download); disk images are single files.
   *  The dest folder picker uses Tauri's plugin-dialog; the engine
   *  appends the remote basename underneath whatever the user
   *  picks (so picking ~/Downloads with entry "MyGame" lands
   *  ~/Downloads/MyGame). Polls jobStatus on a 500 ms cadence to
   *  drive the in-row progress bar; bails on the first failure
   *  and surfaces the engine's error verbatim. */
  const runDownload = async () => {
    const picked = await openDialog({
      multiple: false,
      directory: true,
      title: tr(
        "library_download_dialog_title",
        { name: entry.name },
        "Pick a destination folder for \"{name}\"",
      ),
    });
    if (typeof picked !== "string") return;
    setBusy("download");
    setError(null);
    setMountNote(null);
    setDownloadProgress({ bytesReceived: 0, totalBytes: 0 });
    downloadStopRef.current = false;
    const addr = `${host}:${PS5_PAYLOAD_PORT}`;
    const kind: "file" | "folder" =
      entry.kind === "image" ? "file" : "folder";
    // Track the download in the global activity log so the
    // OperationBar shows live progress + "still running" while the
    // user is on another tab. Library Download has its own polling
    // loop (it doesn't go through useFsDownloadOpStore that
    // FileSystem uses, since the row holds component-local state),
    // so without an explicit activity entry these multi-GiB game
    // downloads were invisible globally.
    const activityId = useActivityHistoryStore
      .getState()
      .start("library-download", `Downloading ${entry.name}`, {
        fromPath: entry.path,
        toPath: picked,
      });
    let jobId: string;
    try {
      jobId = await startTransferDownload(entry.path, picked, addr, kind);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(
        tr(
          "library_download_start_failed",
          { error: msg },
          "Couldn't start the download: {error}",
        ),
      );
      useActivityHistoryStore
        .getState()
        .finish(activityId, "failed", { error: msg });
      setBusy(null);
      setDownloadProgress(null);
      return;
    }
    // Poll until terminal. The mountedRef gate makes a navigate-away
    // mid-download exit the loop instead of writing state on an
    // unmounted component. The engine job keeps running on the
    // engine side (no engine cancel API today); the file still
    // lands on disk and the user finds it where they picked, just
    // without the in-row "Done" note.
    //
    // Closing the activity entry on unmount: an orphan-running
    // entry would be confusing (Activity tab shows "Downloading X"
    // forever; if the user starts the download again from a re-
    // mount, they get TWO running rows for the same file). The
    // previous behavior relied on `loadInitial` to convert orphans
    // to "stopped" on the next app launch — fine for a crash, not
    // fine for a tab switch. So we close the entry on every
    // unmounted-bail with a "stopped watching" note that mirrors
    // the user-Stop wording: the engine job may still finish, the
    // entry just reflects that we stopped observing it.
    const bailOnUnmount = () => {
      useActivityHistoryStore.getState().finish(activityId, "stopped", {
        error: "stopped watching (engine job may continue)",
      });
    };
    while (true) {
      if (!mountedRef.current) {
        bailOnUnmount();
        return;
      }
      if (downloadStopRef.current) {
        // User clicked Stop. Engine job continues server-side; we
        // just stop polling. Surface a note so the row clears the
        // spinner with a clear "you stopped this" instead of going
        // back to idle silently. The activity entry transitions to
        // "stopped" with the same wording.
        useActivityHistoryStore.getState().finish(activityId, "stopped", {
          error: "stopped by user (engine job may continue)",
        });
        setMountNote(
          tr(
            "library_download_stopped",
            undefined,
            "Download stopped. The engine may still finish writing the file in the background.",
          ),
        );
        setBusy(null);
        setDownloadProgress(null);
        return;
      }
      try {
        const snap = await jobStatus(jobId);
        if (!mountedRef.current) {
          bailOnUnmount();
          return;
        }
        if (snap.status === "done") {
          useActivityHistoryStore.getState().finish(activityId, "done", {
            bytes: snap.bytes_sent ?? 0,
            totalBytes: snap.total_bytes ?? 0,
          });
          setMountNote(
            tr(
              "library_download_succeeded",
              {
                dest: snap.dest ?? picked,
                bytes: snap.bytes_sent ?? 0,
              },
              "Downloaded to {dest} ({bytes} bytes).",
            ),
          );
          setBusy(null);
          setDownloadProgress(null);
          return;
        }
        if (snap.status === "failed") {
          const errMsg = snap.error ?? "download failed";
          useActivityHistoryStore
            .getState()
            .finish(activityId, "failed", { error: errMsg });
          setError(
            tr(
              "library_download_failed",
              { error: errMsg },
              "Download failed: {error}",
            ),
          );
          setBusy(null);
          setDownloadProgress(null);
          return;
        }
        setDownloadProgress({
          bytesReceived: snap.bytes_sent ?? 0,
          totalBytes: snap.total_bytes ?? 0,
        });
        // Mirror live progress to the activity entry so the
        // OperationBar speedometer ticks.
        useActivityHistoryStore.getState().update(activityId, {
          bytes: snap.bytes_sent ?? 0,
          totalBytes: snap.total_bytes ?? 0,
        });
      } catch (e) {
        if (!mountedRef.current) {
          bailOnUnmount();
          return;
        }
        const msg = e instanceof Error ? e.message : String(e);
        useActivityHistoryStore
          .getState()
          .finish(activityId, "failed", { error: msg });
        setError(
          tr(
            "library_download_poll_failed",
            { error: msg },
            "Lost contact with the engine while downloading: {error}",
          ),
        );
        setBusy(null);
        setDownloadProgress(null);
        return;
      }
      await new Promise((resolve) => setTimeout(resolve, 500));
    }
  };

  /** Unmount: flipped from Mount when the archive is currently
   *  mounted. Runs the same fsUnmount the Volumes screen uses.
   *  onChanged refreshes the volumes list which feeds the mountMap. */
  const runUnmount = async () => {
    // 2.2.55: works for both image rows AND game rows whose backing
    // image is mounted. For image rows: mount_point comes from
    // currentMount (mountMap.get(entry.path)). For game rows:
    // mount_point comes from the backing image's mountMap entry,
    // looked up via fromImagePath. Either way, fsUnmount needs the
    // mount_point (not the image path or game path).
    let mountPointToUnmount: string | null = currentMount;
    let imageKey: string | null = entry.path;
    if (entry.kind === "game" && fromImagePath) {
      mountPointToUnmount =
        mountMap.get(fromImagePath) ??
        pendingMounts.get(fromImagePath) ??
        null;
      imageKey = fromImagePath;
    }
    if (!mountPointToUnmount) return;
    setBusy("unmount");
    setError(null);
    setMountNote(null);
    const activityId = useActivityHistoryStore
      .getState()
      .start("library-unmount", `Unmounting ${entry.name}`, {
        fromPath: mountPointToUnmount,
      });
    let okOutcome = true;
    let errMsg: string | null = null;
    try {
      const addr = `${host}:${PS5_PAYLOAD_PORT}`;
      // 2.2.60: unregister any titles inside this image BEFORE
      // unmount. Pre-fix the kernel unmount worked but Sony's
      // app.db was left with stale rows pointing at the now-gone
      // path, surfacing later as ghost tiles in the dashboard or
      // "0x80980103 invalid title id" on the next launch attempt
      // for the same image. Sony's appinst Uninstall removes the
      // app.db row + frees the slot.
      //
      // Scope:
      //   - Game row: unregister this specific title.
      //   - Image row: unregister every game in the library whose
      //     fromImagePath matches this image. The library scan
      //     populated `entries` with the right relations earlier,
      //     so we can resolve them locally without an extra round-
      //     trip to the PS5.
      //
      // Best-effort: a failed unregister doesn't block the unmount
      // (might already be unregistered, or the API is unavailable
      // on this firmware). Errors are logged + the unmount proceeds.
      const titleIdsToUnregister: string[] = [];
      if (entry.kind === "game" && entry.titleId) {
        titleIdsToUnregister.push(entry.titleId);
      } else if (entry.kind !== "game" && imageKey) {
        const k = imageKey;
        const libState = useLibraryStore.getState();
        const allEntries = libState.entries ?? [];
        for (const e of allEntries) {
          if (
            e.kind === "game" &&
            e.titleId &&
            findOwningImage(libState, e.path) === k
          ) {
            titleIdsToUnregister.push(e.titleId);
          }
        }
      }
      if (titleIdsToUnregister.length > 0) {
        setMountNote(
          titleIdsToUnregister.length === 1
            ? `Unregistering ${titleIdsToUnregister[0]}…`
            : `Unregistering ${titleIdsToUnregister.length} titles…`,
        );
        for (const tid of titleIdsToUnregister) {
          try {
            await appUnregister(addr, tid);
          } catch (uErr) {
            console.warn(`appUnregister(${tid}) failed (best-effort):`, uErr);
          }
        }
        // Settle window so app.db's commit hits disk before the
        // unmount yanks the source path. Same 400 ms used by the
        // post-unmount onChanged below — kernel-side propagation
        // delays are similar in magnitude.
        await new Promise((resolve) => setTimeout(resolve, 400));
      }
      await fsUnmount(addr, mountPointToUnmount);
      // Optimistic mountMap remove so the row flips to NOT-mounted
      // immediately; the background rescan will confirm.
      useLibraryStore.getState().removeMount(imageKey);
      setMountNote(`Unmounted ${mountPointToUnmount}.`);
      // Delay the rescan slightly so the unmount fully propagates
      // through the kernel mount table before scanLibrary reads it
      // — without the delay, the just-unmounted volume sometimes
      // still appears, briefly producing a "library shows mounted-
      // again" flicker before the next manual refresh.
      setTimeout(() => onChanged(), 400);
    } catch (e) {
      okOutcome = false;
      errMsg = e instanceof Error ? e.message : String(e);
      setError(humanizePs5Error(errMsg));
    } finally {
      useActivityHistoryStore
        .getState()
        .finish(activityId, okOutcome ? "done" : "failed", {
          error: errMsg ?? undefined,
        });
      setBusy(null);
    }
  };

  // Launch is a re-exposure of the existing payload + engine
  // app_launch wiring. Re-enabled in 2.2.26 — the payload's
  // launch_title runs the triple-strategy chain (LncUtil zeroed-param
  // → LncUtil NULL → SystemServiceLaunchApp) and surfaces a
  // composite error string on failure. Only valid when entry is a
  // game with a known title_id (image rows don't have one until
  // they're mounted and re-scanned). The button is gated on
  // entry.titleId so a row with no title_id can't trigger an
  // unhelpful "Invalid title ID format" error from the payload.
  const runLaunch = async () => {
    if (entry.kind !== "game" || !entry.titleId) return;
    setBusy("register");
    setError(null);
    setMountNote(null);
    const activityId = useActivityHistoryStore
      .getState()
      .start("library-launch", `Launching ${entry.name}`, {
        fromPath: entry.path,
      });
    let okOutcome = true;
    let errMsg: string | null = null;
    try {
      const addr = `${host}:${PS5_PAYLOAD_PORT}`;

      // 2.2.55: ALWAYS register first, then launch. Pre-fix the
      // flow tried launch first and registered as a fallback only
      // when launch reported "not registered" — but on first-Play
      // for a freshly-mounted image the launch attempt would emit
      // a misleading error before the registration kicked in. User
      // feedback: always register first (idempotent if already
      // registered), and try patch-DRM register if normal fails.
      // Then launch.
      setMountNote(`Registering ${entry.name}…`);
      try {
        await appRegister(addr, entry.path);
      } catch (regErr) {
        // Try patch-DRM fallback. Some PKG-extracted dirs have a
        // DRM type Sony's installer rejects without the patch
        // (param.sfo's CONTENT_TYPE != GD); the patch fixes that
        // up before the register call is dispatched. Surface the
        // FIRST error if patch-DRM also fails so the user has the
        // root cause, not the secondary symptom.
        const regMsg = regErr instanceof Error ? regErr.message : String(regErr);
        // Idempotent re-register returns 0x80990002 ("already
        // registered") which the payload normalises to success;
        // if we somehow see it here, treat as success.
        if (/0x80990002|already\s*registered/i.test(regMsg)) {
          // proceed to launch
        } else {
          setMountNote(
            `${entry.name} register failed — retrying with DRM-type patch…`,
          );
          try {
            await appRegister(addr, entry.path, { patchDrmType: true });
          } catch (patchErr) {
            const patchMsg =
              patchErr instanceof Error ? patchErr.message : String(patchErr);
            if (!/0x80990002|already\s*registered/i.test(patchMsg)) {
              throw regErr;
            }
          }
        }
      }
      // Settle delay so the registration commit hits app.db
      // before the launch query reads it.
      await new Promise((resolve) => setTimeout(resolve, 600));
      setBusy("launch");
      setMountNote(`Launching ${entry.titleId}…`);
      await appLaunch(addr, entry.titleId);
      setMountNote(
        `Launching ${entry.titleId}. Check the PS5 — the title should appear in the foreground in a few seconds.`,
      );
      // Refresh so the just-registered title surfaces correctly in
      // any "registered" UI surfaces and stays consistent with
      // app.db state.
      onChanged();
    } catch (e) {
      okOutcome = false;
      errMsg = e instanceof Error ? e.message : String(e);
      setError(humanizePs5Error(errMsg));
    } finally {
      useActivityHistoryStore
        .getState()
        .finish(activityId, okOutcome ? "done" : "failed", {
          error: errMsg ?? undefined,
        });
      setBusy(null);
    }
  };

  // Register stages sce_sys into /user/app/<title_id>/ + nullfs-binds
  // the source over /system_ex/app/<title_id>/ + calls Sony's
  // installer. Re-exposed in 2.2.26 alongside Launch — the payload
  // and engine were already wired (we use it from the lab CLI and
  // tests); the Library row was the only missing piece.
  //
  // Idempotent: re-registering the same path is a no-op (Sony's
  // installer returns 0x80990002 which the payload normalises to
  // success). Show the resulting tile name + title id in the
  // mountNote so the user sees what landed in app.db.
  const runRegister = async (opts: { patchDrmType?: boolean } = {}) => {
    if (entry.kind !== "game") return;
    setBusy("register");
    setError(null);
    setMountNote(null);
    const activityId = useActivityHistoryStore
      .getState()
      .start("library-register", `Registering ${entry.name}`, {
        fromPath: entry.path,
      });
    let okOutcome = true;
    let errMsg: string | null = null;
    try {
      const res = await appRegister(`${host}:${PS5_PAYLOAD_PORT}`, entry.path, {
        patchDrmType: opts.patchDrmType,
      });
      const drmSuffix = opts.patchDrmType ? " (DRM type patched to standard)" : "";
      setMountNote(
        `Registered ${res.title_name || res.title_id} (${res.title_id})${drmSuffix}. Launch should now succeed; tile appears in the PS5 home screen.`,
      );
      onChanged();
    } catch (e) {
      okOutcome = false;
      errMsg = e instanceof Error ? e.message : String(e);
      setError(humanizePs5Error(errMsg));
    } finally {
      useActivityHistoryStore
        .getState()
        .finish(activityId, okOutcome ? "done" : "failed", {
          error: errMsg ?? undefined,
        });
      setBusy(null);
    }
  };

  const runBackupGame = async () => {
    if (entry.kind !== "game") return;
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const dest = await open({
        directory: true,
        title: tr(
          "library_backup_picker",
          undefined,
          "Pick a folder to back up the game into",
        ),
      });
      if (!dest || typeof dest !== "string") return;
      const addr = `${host}:${PS5_PAYLOAD_PORT}`;
      await startTransferDownload(entry.path, dest, addr, "folder");
      pushNotification("info", `Backup started: ${entry.name}`, {
        body: `Downloading ${entry.path} → ${dest}. Track progress in the Activity tab.`,
      });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const runHealAppmeta = async () => {
    if (entry.kind !== "game" || !entry.titleId) return;
    setBusy("register");
    setError(null);
    setMountNote(null);
    try {
      const addr = mgmtAddr(host);
      const res = await healAppmeta(addr, entry.titleId, entry.path);
      const summary =
        res.copied > 0
          ? `Healed ${res.copied} file${res.copied === 1 ? "" : "s"} to ${res.appmeta_dir}`
          : `Nothing to heal — ${res.already_present} files already present in ${res.appmeta_dir}`;
      setMountNote(summary);
      if (res.errors > 0) {
        const firstErr = res.outcomes.find((o) => o.status === "error");
        setError(
          `Some files failed: ${firstErr?.error ?? "unknown error"} — see Logs for details.`,
        );
        pushNotification("warning", `Heal partial: ${entry.titleId}`, {
          body: `${res.copied} copied, ${res.errors} failed for ${entry.name}`,
          link: "/library",
        });
      } else if (res.copied > 0) {
        pushNotification("success", `Healed ${entry.name}`, {
          body: `Restored ${res.copied} file${res.copied === 1 ? "" : "s"} for ${entry.titleId} — home-screen tile should refresh.`,
          link: "/library",
        });
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const runUnregister = async () => {
    if (entry.kind !== "game" || !entry.titleId) return;
    setBusy("unregister");
    setError(null);
    setMountNote(null);
    const activityId = useActivityHistoryStore
      .getState()
      .start("library-unregister", `Unregistering ${entry.name}`, {
        fromPath: entry.path,
      });
    let okOutcome = true;
    let errMsg: string | null = null;
    try {
      await appUnregister(`${host}:${PS5_PAYLOAD_PORT}`, entry.titleId);
      setMountNote(
        `Unregistered ${entry.titleId}. The XMB tile is removed; the source files on disk were not touched.`,
      );
      onChanged();
    } catch (e) {
      okOutcome = false;
      errMsg = e instanceof Error ? e.message : String(e);
      setError(humanizePs5Error(errMsg));
    } finally {
      useActivityHistoryStore
        .getState()
        .finish(activityId, okOutcome ? "done" : "failed", {
          error: errMsg ?? undefined,
        });
      setBusy(null);
    }
  };

  return (
    <article className="flex flex-col gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3">
      <div className="flex items-center gap-3">
        <LibraryThumb
          entry={entry}
          meta={meta}
          host={host}
          fallbackIcon={Icon}
        />
        <div className="min-w-0 flex-1">
          <div className="truncate text-sm font-medium">
            {meta?.title ?? entry.name}
          </div>
          <div className="mt-0.5 truncate font-mono text-xs text-[var(--color-muted)]">
            {meta?.title_id ? (
              <>
                <span className="text-[var(--color-accent)]">
                  {meta.title_id}
                </span>
                {meta.content_version ? ` · v${meta.content_version}` : ""}
                {" · "}
              </>
            ) : null}
            {entry.path}
          </div>
        </div>
        <div className="shrink-0 text-right text-xs text-[var(--color-muted)]">
          <div>{kindLabel}</div>
          {entry.size > 0 && (
            <div className="tabular-nums">{formatBytes(entry.size)}</div>
          )}
          {isMounted && (
            <div
              className="mt-1 inline-block rounded-full border border-[var(--color-accent)] bg-[var(--color-accent-soft)] px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-[var(--color-accent)]"
              title={`Mounted at ${currentMount}`}
            >
              {tr("library_badge_mounted", undefined, "mounted")}
            </div>
          )}
          {entry.kind === "game" && entry.titleId && isTitleRunning && (
            <div
              className="mt-1 inline-block rounded-full border border-[var(--color-good)] bg-[var(--color-good-soft)] px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-[var(--color-good)]"
              title={tr(
                "library_badge_running_tooltip",
                undefined,
                "This title is currently running on the PS5 — see the Running apps panel above for suspend/kill controls.",
              )}
            >
              {tr("library_badge_running", undefined, "running")}
            </div>
          )}
          {playSeconds !== undefined && playSeconds > 60 && (
            <div
              className="mt-1 text-[10px] tabular-nums text-[var(--color-muted)]"
              title={tr(
                "library_play_time_tooltip",
                undefined,
                "Tracked while ps5upload is open and the title shows in PROC_LIST. Approximate.",
              )}
            >
              {formatPlayTime(playSeconds)}
            </div>
          )}
          {fromImagePath && fromImageBasename && (
            <div
              className="mt-1 inline-flex items-center gap-1 rounded-full border border-[var(--color-border-strong)] bg-[var(--color-surface-2)] px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-[var(--color-muted)]"
              title={tr(
                "library_badge_from_image_tooltip",
                { image: fromImagePath },
                `Lives inside ${fromImagePath}. Unmounting that image will hide this title until you remount it.`,
              )}
            >
              <FileArchive size={9} />
              <span>
                {tr(
                  "library_badge_from_image",
                  { image: fromImageBasename },
                  `from ${fromImageBasename}`,
                )}
              </span>
            </div>
          )}
        </div>
        <div className="ml-2 flex shrink-0 items-center gap-1">
          {entry.kind === "image" ? (
            isMounted ? (
              <Button
                variant="secondary"
                size="sm"
                leftIcon={<Unplug size={12} />}
                onClick={runUnmount}
                disabled={busy !== null}
                loading={busy === "unmount"}
                title={`Unmount ${currentMount}`}
              >
                {tr("library_unmount", undefined, "Unmount")}
              </Button>
            ) : (
              <Button
                variant="primary"
                size="sm"
                leftIcon={<Play size={12} />}
                onClick={() => setMountOpen(true)}
                /* Pre-disable on entries with no recognized image
                 * format (imageFormat null/undefined) — the payload
                 * would respond with `fs_mount_unsupported_format`
                 * after the user clicks, which produces a back-end
                 * error string the user has to read in the row
                 * banner. Disabling the button up front avoids the
                 * detour. Real cause is usually a renamed file
                 * with a non-.exfat / non-.ffpkg / non-.ffpfs
                 * extension that still got into the library scan via
                 * the size / existence checks. */
                disabled={busy !== null || !entry.imageFormat}
                loading={busy === "mount"}
                title={
                  entry.imageFormat
                    ? tr(
                        "library_mount_tooltip",
                        undefined,
                        "Mount this image on your PS5",
                      )
                    : tr(
                        "library_mount_unsupported_tooltip",
                        undefined,
                        "Unsupported image format — only .exfat, .ffpkg and .ffpfs can be mounted",
                      )
                }
              >
                {tr("library_mount", undefined, "Mount")}
              </Button>
            )
          ) : (
            /* Game rows: one primary action (Play — runs the auto-
             * register-then-launch flow), one Details ghost button
             * for cover art, and an overflow (...) menu for the
             * less-common actions. Pre-2.2.37 the row had 7+
             * always-visible buttons that crowded smaller screens
             * and made the row hard to scan; collapsing them into
             * one verb + ... reads as one decision per row at a
             * glance. */
            <>
              <Button
                variant="primary"
                size="sm"
                leftIcon={<Play size={12} />}
                onClick={runLaunch}
                disabled={busy !== null || !entry.titleId}
                loading={busy === "launch" || busy === "register"}
                title={
                  entry.titleId
                    ? tr(
                        "library_launch_tooltip",
                        undefined,
                        "Launch this game (auto-registers if it isn't in app.db yet)",
                      )
                    : tr(
                        "library_launch_no_titleid_tooltip",
                        undefined,
                        "No title id detected — Launch needs sce_sys/param.json to read",
                      )
                }
              >
                {busy === "register"
                  ? tr("library_busy_register", undefined, "Registering…")
                  : tr("library_play", undefined, "Play")}
              </Button>
              <Button
                variant="ghost"
                size="sm"
                leftIcon={<Info size={12} />}
                onClick={() => setDetailsOpen(true)}
                disabled={busy !== null || !entry.titleId}
                title={
                  entry.titleId
                    ? tr(
                        "library_details_tooltip",
                        undefined,
                        "Game details — fetches cover art from PROSPEROPatches (PS5) or ORBISPatches (PS4)",
                      )
                    : tr(
                        "library_details_no_titleid_tooltip",
                        undefined,
                        "Need a title id from sce_sys/param.json to look up details",
                      )
                }
              >
                {tr("library_details", undefined, "Details")}
              </Button>
              {/* 2.2.55: Surface unmount on game rows whose backing
                * image is currently mounted. Pre-fix the unmount
                * affordance was only on the image row — once a game
                * is registered, the user opens that game row and
                * had no way back to unmount without switching to the
                * Volumes screen. fromImagePath is non-null exactly
                * when this game lives inside a mounted image; clicking
                * Unmount calls fs_unmount on the image's mount point. */}
              {fromImagePath && (
                <Button
                  variant="ghost"
                  size="sm"
                  leftIcon={<Unplug size={12} />}
                  onClick={runUnmount}
                  disabled={busy !== null}
                  loading={busy === "unmount"}
                  title={tr(
                    "library_unmount_game_tooltip",
                    undefined,
                    "Unmount the disk image this game lives in. Sony will refuse if the title is currently running on the PS5.",
                  )}
                >
                  {tr("library_unmount", undefined, "Unmount")}
                </Button>
              )}
            </>
          )}
          {/* eslint-disable react-hooks/refs --
              The IIFE below builds menu items that capture row-
              local handlers (runDownload, runUnregister, etc.).
              The handlers themselves read refs (mountedRef et al.)
              but only when the user clicks — NOT during render —
              so the rule's "ref read during render" warning is a
              false positive. The closures are intentional. */}
          {(() => {
            // Overflow menu items, kind-specific. Order is the
            // expected frequency-of-use: register actions first
            // for game rows (most common when Launch reports a
            // DRM mismatch), then file ops (Download, Move),
            // then destructive actions (Delete) at the bottom
            // and styled red so they don't blend.
            const items: OverflowMenuItem[] = [];
            if (entry.kind === "game") {
              items.push({
                label: tr("library_register", undefined, "Register"),
                icon: <Boxes size={12} />,
                onSelect: () => runRegister(),
                disabled: busy !== null,
                loading: busy === "register",
                title: tr(
                  "library_register_tooltip",
                  undefined,
                  "Stage sce_sys + register with Sony's installer so the title appears in the PS5 XMB",
                ),
              });
              items.push({
                label: tr(
                  "library_register_drm",
                  undefined,
                  "Register (patch DRM)",
                ),
                icon: <Boxes size={12} />,
                onSelect: () => runRegister({ patchDrmType: true }),
                disabled: busy !== null,
                loading: busy === "register",
                title: tr(
                  "library_register_drm_tooltip",
                  undefined,
                  "Same as Register, but first patch the source's applicationDrmType to \"standard\" — modifies the source file. Use only when a normal Register fails with a DRM error.",
                ),
              });
              if (entry.titleId) {
                items.push({
                  label: tr("library_unregister", undefined, "Unregister"),
                  icon: <PackageOpen size={12} />,
                  onSelect: runUnregister,
                  disabled: busy !== null,
                  loading: busy === "unregister",
                  title: tr(
                    "library_unregister_tooltip",
                    undefined,
                    "Remove the XMB tile. Source files on disk are not touched.",
                  ),
                });
                items.push({
                  label: tr(
                    "library_heal_metadata",
                    undefined,
                    "Heal home-screen metadata",
                  ),
                  icon: <Sparkles size={12} />,
                  onSelect: runHealAppmeta,
                  disabled: busy !== null,
                  loading: busy === "register",
                  title: tr(
                    "library_heal_metadata_tooltip",
                    undefined,
                    "Copies missing icon0.png / param.json / snd0.at9 from this game's sce_sys/ to /user/appmeta/<title_id>/. Idempotent — files already present are left alone. Use when the home-screen tile is blank or the menu music is missing.",
                  ),
                });
                items.push({
                  label: tr(
                    "library_backup_to_folder",
                    undefined,
                    "Backup to folder…",
                  ),
                  icon: <Download size={12} />,
                  onSelect: runBackupGame,
                  disabled: busy !== null,
                  loading: busy === "register",
                  title: tr(
                    "library_backup_to_folder_tooltip",
                    undefined,
                    "Download this game's source folder to a local directory. Useful for archiving registered games before unregistering, or for bulk-rebuilding library on another PS5.",
                  ),
                });
              }
              items.push({
                label: tr(
                  "library_permission_777",
                  undefined,
                  "Permission 777",
                ),
                icon: <Shield size={12} />,
                onSelect: () => setConfirm({ kind: "chmod", entry }),
                disabled: busy !== null,
                loading: busy === "chmod",
                title: tr(
                  "library_chmod_tooltip",
                  undefined,
                  "Open read/write/execute to every user on this PS5",
                ),
              });
            }
            items.push({
              label: tr("library_download", undefined, "Download"),
              icon: <Download size={12} />,
              onSelect: runDownload,
              disabled: busy !== null,
              loading: busy === "download",
              title: tr(
                "library_download_tooltip",
                undefined,
                "Save a copy of this entry to a folder on this computer",
              ),
            });
            items.push({
              label: tr("library_move", undefined, "Move"),
              icon: <FolderInput size={12} />,
              onSelect: () => setMoveOpen(true),
              disabled: busy !== null || volumes.length === 0,
              title:
                volumes.length === 0
                  ? tr(
                      "library_move_no_volumes_tooltip",
                      undefined,
                      "Move needs at least one writable PS5 drive — none are attached right now.",
                    )
                  : tr(
                      "library_move_tooltip",
                      undefined,
                      "Copy this to another PS5 location, then delete the original",
                    ),
            });
            items.push({
              label: tr("library_delete", undefined, "Delete"),
              icon: <Trash2 size={12} />,
              onSelect: () => setConfirm({ kind: "delete", entry }),
              disabled: busy !== null,
              loading: busy === "delete",
              destructive: true,
              title: tr(
                "library_delete_tooltip",
                undefined,
                "Delete this path from the PS5",
              ),
            });
            return (
              <OverflowMenu
                items={items}
                ariaLabel={tr(
                  "library_more_actions",
                  undefined,
                  "More actions",
                )}
                buttonTitle={tr(
                  "library_more_actions",
                  undefined,
                  "More actions",
                )}
              />
            );
          })()}
          {/* eslint-enable react-hooks/refs */}
        </div>
      </div>

      {busy && (
        <div className="flex items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-xs">
          <Loader2
            size={12}
            className="animate-spin text-[var(--color-accent)]"
          />
          <span className="font-medium">
            {busy === "delete"
              ? tr("library_busy_delete", undefined, "Deleting")
              : busy === "chmod"
                ? tr("library_busy_chmod", undefined, "Applying Permission 777")
                : busy === "unmount"
                  ? tr("library_busy_unmount", undefined, "Unmounting")
                  : busy === "mount"
                    ? tr("library_busy_mount", undefined, "Mounting")
                    : busy === "move-copying"
                      ? tr(
                          "library_busy_move_copying",
                          undefined,
                          "Copying to new location",
                        )
                      : busy === "move-deleting"
                        ? tr(
                            "library_busy_move_deleting",
                            undefined,
                            "Cleaning up source",
                          )
                        : tr(
                            "library_busy_download",
                            undefined,
                            "Downloading from PS5",
                          )}
          </span>
          <span className="text-[var(--color-muted)]">
            {entry.name} · {formatDuration(elapsedMs / 1000)}
            {downloadProgress &&
              downloadProgress.totalBytes > 0 &&
              ` · ${formatBytes(downloadProgress.bytesReceived)} / ${formatBytes(downloadProgress.totalBytes)} (${(
                (downloadProgress.bytesReceived /
                  downloadProgress.totalBytes) *
                100
              ).toFixed(0)}%)`}
            {downloadProgress &&
              downloadProgress.totalBytes === 0 &&
              ` · ${formatBytes(downloadProgress.bytesReceived)}`}
            {/* Live throughput. Driven off `elapsedMs` (which the
                useElapsed hook ticks every 500 ms) so the value
                refreshes naturally; computing it inline avoids a
                separate state field for what's a derived value. */}
            {busy === "download" &&
              downloadProgress &&
              downloadProgress.bytesReceived > 0 &&
              elapsedMs > 0 &&
              ` · ${formatBytes(
                (downloadProgress.bytesReceived * 1000) / elapsedMs,
              )}/s`}
            {/* Same shape for the move's in-flight fs_copy: the
                FS_OP_STATUS poller writes bytesCopied/totalBytes
                into moveProgress on a 500 ms cadence. */}
            {busy === "move-copying" &&
              moveProgress &&
              moveProgress.totalBytes > 0 &&
              ` · ${formatBytes(moveProgress.bytesCopied)} / ${formatBytes(moveProgress.totalBytes)} (${(
                (moveProgress.bytesCopied /
                  moveProgress.totalBytes) *
                100
              ).toFixed(0)}%)`}
            {busy === "move-copying" &&
              moveProgress &&
              moveProgress.bytesCopied > 0 &&
              elapsedMs > 0 &&
              ` · ${formatBytes(
                (moveProgress.bytesCopied * 1000) / elapsedMs,
              )}/s`}
          </span>
          {busy === "download" && (
            <button
              type="button"
              onClick={() => {
                downloadStopRef.current = true;
              }}
              className="ml-auto rounded-md border border-[var(--color-border)] px-2 py-0.5 text-[10px] hover:bg-[var(--color-surface-3)]"
              title={tr(
                "library_download_stop_tooltip",
                undefined,
                "Stop watching this download (engine job continues server-side)",
              )}
            >
              {tr("fs_download_stop", undefined, "Stop")}
            </button>
          )}
          {busy === "move-copying" && (
            <button
              type="button"
              onClick={() => {
                moveStopRef.current = true;
              }}
              className="ml-auto rounded-md border border-[var(--color-border)] px-2 py-0.5 text-[10px] hover:bg-[var(--color-surface-3)]"
              title={tr(
                "library_move_stop_tooltip",
                undefined,
                "Cancel the in-flight copy. Source is unchanged; partial destination is removed.",
              )}
            >
              {tr("fs_download_stop", undefined, "Stop")}
            </button>
          )}
        </div>
      )}

      {/* Hint for the case where the user's PS5 is running an older
          payload that doesn't speak FS_OP_STATUS. Names the specific
          threshold (2.2.7 introduced the frame, 2.2.16 fixed the ACK
          body buffer) so the user knows which fix they're missing.
          Only renders when classifyMovePollError has actually
          identified an old payload — a healthy 2.2.16+ payload that
          hits transient errors will see no banner. */}
      {busy === "move-copying" &&
        moveProgressUnsupportedThreshold === "2.2.16" && (
          <div className="rounded-md border border-[var(--color-warn)] bg-[var(--color-warn-soft)] p-2 text-[11px] text-[var(--color-warn)]">
            {tr(
              "library_move_progress_unsupported_2_2_16",
              undefined,
              "Live progress unavailable — running payload predates the 2.2.16 FS_OP_STATUS_ACK body fix. Click \"Replace payload\" on the Connection screen to enable per-byte progress + cancel.",
            )}
          </div>
        )}
      {busy === "move-copying" &&
        moveProgressUnsupportedThreshold === "2.2.7" && (
          <div className="rounded-md border border-[var(--color-warn)] bg-[var(--color-warn-soft)] p-2 text-[11px] text-[var(--color-warn)]">
            {tr(
              "library_move_progress_unsupported_2_2_7",
              undefined,
              "Live progress unavailable — running payload predates 2.2.7 FS_OP_STATUS. Click \"Replace payload\" on the Connection screen to enable per-byte progress + cancel.",
            )}
          </div>
        )}

      {mountNote && (
        <div className="flex items-start gap-2 rounded-md border border-[var(--color-accent)] bg-[var(--color-surface)] p-2 text-xs">
          <Play size={12} className="mt-0.5 text-[var(--color-accent)]" />
          <span className="flex-1">{mountNote}</span>
          <button
            type="button"
            onClick={() => setMountNote(null)}
            className="rounded px-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)]"
            aria-label={tr("dismiss", undefined, "Dismiss")}
          >
            ×
          </button>
        </div>
      )}

      {error && (
        <div className="rounded-md border border-[var(--color-bad)] bg-[var(--color-surface)] p-2 text-xs text-[var(--color-bad)]">
          {error}
        </div>
      )}

      {confirm && (
        <ConfirmRow
          confirm={confirm}
          onCancel={() => setConfirm(null)}
          onRun={confirm.kind === "delete" ? runDelete : runChmod}
        />
      )}

      {moveOpen && (
        <MoveModal
          entry={entry}
          volumes={volumes}
          addr={`${host}:${PS5_PAYLOAD_PORT}`}
          onCancel={() => setMoveOpen(false)}
          onConfirm={runMove}
        />
      )}

      {mountOpen && entry.kind === "image" && (
        <MountModal
          entry={entry}
          host={host}
          volumes={volumes}
          payloadVersion={payloadVersion}
          onCancel={() => setMountOpen(false)}
          onConfirm={(opts) => {
            void runMount(opts);
          }}
        />
      )}

      {detailsOpen && entry.kind === "game" && entry.titleId && (
        <GameDetailsModal
          entry={entry}
          meta={meta}
          onCancel={() => setDetailsOpen(false)}
        />
      )}
    </article>
  );
}

/** Game details modal — local sce_sys/param.json fields plus best-
 *  effort PSN store metadata (cover art, description, genre,
 *  publisher, age rating). Pure client-side: PSN fetch via plain
 *  `fetch()` against the public valkyrie + chihiro endpoints, cached
 *  for 7 days in localStorage. Falls back to a "Search PSN store"
 *  link in the user's browser when the API gives nothing back. */
function GameDetailsModal({
  entry,
  meta,
  onCancel,
}: {
  entry: LibraryEntry;
  meta: GameMeta | null;
  onCancel: () => void;
}) {
  const tr = useTr();
  const [info, setInfo] = useState<TitleInfo | null>(null);
  const [loading, setLoading] = useState(false);
  const [fetchError, setFetchError] = useState<string | null>(null);

  // Escape closes the modal — same standard-dialog UX as Mount/Move.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel]);

  // Kick off the PSN fetch on mount. AbortController so closing the
  // modal mid-fetch doesn't write into a torn-down component (React
  // would warn about state-update-after-unmount; the abort makes the
  // promise resolve in a no-op path before setInfo).
  useEffect(() => {
    if (!entry.titleId) return;
    const controller = new AbortController();
    setLoading(true);
    setFetchError(null);
    fetchTitleInfo(entry.titleId, controller.signal)
      .then((result) => {
        if (controller.signal.aborted) return;
        setInfo(result);
      })
      .catch((e: unknown) => {
        if (controller.signal.aborted) return;
        setFetchError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
  }, [entry.titleId]);

  const displayTitle = info?.title ?? meta?.title ?? entry.name;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
      onClick={onCancel}
    >
      <div
        className="w-full max-w-2xl rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="mb-3 flex items-center gap-2 text-sm font-semibold">
          <Info size={14} />
          {tr("library_details_modal_title", undefined, "Game details")}
        </header>

        <div className="grid gap-4 md:grid-cols-[200px_1fr]">
          <div className="flex flex-col gap-2">
            {info?.coverImageUrl ? (
              <img
                src={info.coverImageUrl}
                alt={displayTitle}
                className="w-full rounded-md border border-[var(--color-border)]"
                /* PSN cover-art URLs sometimes 404 for region-mismatched
                 * titles. Hide the broken-image placeholder rather than
                 * leaving a raw cracked-icon. */
                onError={(e) => {
                  e.currentTarget.style.display = "none";
                }}
              />
            ) : (
              <div className="flex aspect-[3/4] w-full items-center justify-center rounded-md border border-dashed border-[var(--color-border)] text-xs text-[var(--color-muted)]">
                {loading
                  ? tr(
                      "library_details_modal_loading_cover",
                      undefined,
                      "Loading cover art…",
                    )
                  : tr(
                      "library_details_modal_no_cover",
                      undefined,
                      "No cover art available",
                    )}
              </div>
            )}
            {entry.titleId &&
              (() => {
                const url = patchesSiteUrl(entry.titleId);
                const siteName = patchesSiteName(entry.titleId);
                if (!url || !siteName) return null;
                return (
                  <button
                    type="button"
                    onClick={() => {
                      void openExternal(url);
                    }}
                    className="inline-flex items-center justify-center gap-1 rounded-md border border-[var(--color-border)] px-2 py-1 text-xs hover:bg-[var(--color-surface-3)]"
                  >
                    <ExternalLink size={11} />
                    {tr(
                      "library_details_modal_open_patches_site",
                      { site: siteName },
                      `View on ${siteName}`,
                    )}
                  </button>
                );
              })()}
          </div>

          <div className="flex min-w-0 flex-col gap-3 text-sm">
            <div>
              <div className="break-words text-base font-semibold">
                {displayTitle}
              </div>
              <div className="mt-0.5 flex flex-wrap gap-x-3 gap-y-0.5 text-xs text-[var(--color-muted)]">
                {entry.titleId && (
                  <span className="font-mono">{entry.titleId}</span>
                )}
              </div>
            </div>

            {!loading && !info && !fetchError && (
              <p className="text-xs text-[var(--color-muted)]">
                {tr(
                  "library_details_modal_no_remote_meta",
                  undefined,
                  "No remote metadata for this title id. The local sce_sys/param.json info below is what we have on disk.",
                )}
              </p>
            )}
            {fetchError && (
              <p className="text-xs text-[var(--color-warn)]">
                {tr(
                  "library_details_modal_fetch_error",
                  { error: fetchError },
                  `Title metadata fetch failed: ${fetchError}`,
                )}
              </p>
            )}

            <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
              <dt className="text-[var(--color-muted)]">
                {tr("library_details_modal_path", undefined, "Path")}
              </dt>
              <dd className="break-all font-mono">{entry.path}</dd>
              {meta?.title && (
                <>
                  <dt className="text-[var(--color-muted)]">
                    {tr(
                      "library_details_modal_local_title",
                      undefined,
                      "Local title",
                    )}
                  </dt>
                  <dd>{meta.title}</dd>
                </>
              )}
            </dl>
          </div>
        </div>

        <div className="mt-4 flex items-center justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={onCancel}>
            {tr("close", undefined, "Close")}
          </Button>
        </div>
      </div>
    </div>
  );
}

/** Move-target picker. Source path is read-only; user picks the
 *  destination volume + subpath, sees the resolved final path before
 *  committing. Pre-fills with the entry's current volume + parent dir
 *  so a "move within the same drive" stays one click away. */
function MoveModal({
  entry,
  volumes,
  addr,
  onCancel,
  onConfirm,
}: {
  entry: LibraryEntry;
  volumes: Volume[];
  addr: string;
  onCancel: () => void;
  onConfirm: (destination: string) => void;
}) {
  const tr = useTr();
  const volumePaths = useMemo(() => volumes.map((v) => v.path), [volumes]);
  const initialVolume = useMemo(
    () => detectSourceVolume(entry.path, volumePaths) ?? volumePaths[0] ?? "",
    [entry.path, volumePaths],
  );
  const [volume, setVolume] = useState<string>(initialVolume);
  const [subpath, setSubpath] = useState<string>(() =>
    defaultMoveSubpath(entry.path, initialVolume),
  );
  const [customName, setCustomName] = useState<string>(() =>
    sourceBasename(entry.path),
  );
  const resolved = resolveMoveDestination(
    volume,
    subpath,
    entry.path,
    customName,
  );
  const noop = isMoveNoop(entry.path, resolved);
  const nameInvalid = isInvalidName(customName);

  // Escape closes the modal — standard dialog UX. Click-outside is
  // already wired to onCancel via the backdrop, but keyboards
  // shouldn't have to reach for the mouse to dismiss. Window-level
  // listener so it works regardless of focus inside the modal.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel]);

  // Pre-flight: probe whether the resolved destination already exists
  // and warn the user inline. Catches both name clashes against an
  // unrelated folder and stale partials left behind by a previously
  // cancelled / failed move whose rm_rf cleanup didn't fully drain
  // (exfat USB quirks). Debounced 300ms so each keystroke in the
  // rename field doesn't spam FS_LIST_DIR; cancellation flag prevents
  // a stale resolution from clobbering a fresher one.
  const [destExists, setDestExists] = useState<"unknown" | "yes" | "no">(
    "unknown",
  );
  useEffect(() => {
    if (noop || nameInvalid || !resolved) {
      setDestExists("unknown");
      return;
    }
    let cancelled = false;
    setDestExists("unknown");
    const timer = setTimeout(() => {
      fsPathExists(addr, resolved)
        .then((exists) => {
          if (!cancelled) setDestExists(exists ? "yes" : "no");
        })
        .catch(() => {
          if (!cancelled) setDestExists("unknown");
        });
    }, 300);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [addr, resolved, noop, nameInvalid]);
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
      onClick={onCancel}
    >
      <div
        className="w-full max-w-lg rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="mb-3 flex items-center gap-2 text-sm font-semibold">
          <FolderInput size={14} />
          {tr(
            "library_move_modal_title",
            { name: entry.name },
            "Move \"{name}\"",
          )}
        </header>

        <dl className="mb-4 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
          <dt className="text-[var(--color-muted)]">
            {tr("library_move_modal_source", undefined, "From")}
          </dt>
          <dd className="font-mono text-[var(--color-text)]">{entry.path}</dd>
        </dl>

        <label className="mb-2 block text-[10px] font-semibold uppercase tracking-wide text-[var(--color-muted)]">
          {tr("library_move_modal_destination", undefined, "To")}
        </label>
        <div className="mb-3 flex items-center gap-2 text-sm">
          <select
            value={volume}
            onChange={(e) => setVolume(e.target.value)}
            className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-1.5 text-sm"
          >
            {volumePaths.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
          <span className="text-[var(--color-muted)]">/</span>
          <input
            value={subpath}
            onChange={(e) => setSubpath(e.target.value)}
            placeholder={tr(
              "library_move_modal_subpath_placeholder",
              undefined,
              "subpath (e.g. games)",
            )}
            className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-1.5 text-sm"
          />
        </div>

        {/* Optional rename — defaults to the source basename so a
            plain "move to a different folder" is still one click.
            Editing this is a destination-side rename: useful for
            de-duplicating titles already present in the destination,
            or for normalising long auto-generated dump folder names
            into something readable. Embedded slashes are rejected
            below to keep this a single-segment rename — if a user
            really wants to inject a deeper subpath, they can put it
            in the subpath field above. */}
        <label className="mb-2 block text-[10px] font-semibold uppercase tracking-wide text-[var(--color-muted)]">
          {tr("library_move_modal_name", undefined, "Name (optional rename)")}
        </label>
        <div className="mb-3">
          <input
            value={customName}
            onChange={(e) => setCustomName(e.target.value)}
            placeholder={sourceBasename(entry.path)}
            className={`w-full rounded-md border bg-[var(--color-surface)] px-3 py-1.5 text-sm ${
              nameInvalid
                ? "border-[var(--color-bad)]"
                : "border-[var(--color-border)]"
            }`}
          />
          <p className="mt-1 text-[10px] text-[var(--color-muted)]">
            {tr(
              "library_move_modal_name_hint",
              { default: sourceBasename(entry.path) },
              "Leave blank or matching \"{default}\" to keep the original name. No slashes.",
            )}
          </p>
        </div>

        <div className="mb-4 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-xs">
          <div className="text-[10px] uppercase tracking-wide text-[var(--color-muted)]">
            {tr(
              "library_move_modal_resolved",
              undefined,
              "Will move to",
            )}
          </div>
          <div className="mt-0.5 break-all font-mono">{resolved}</div>
        </div>

        {nameInvalid && (
          <div className="mb-3 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface)] p-2 text-xs text-[var(--color-bad)]">
            {tr(
              "library_move_modal_name_invalid",
              undefined,
              "Name can't contain / or \\ and can't be \".\" or \"..\". Use the subpath field above to nest into a folder.",
            )}
          </div>
        )}

        {noop && !nameInvalid && (
          <div className="mb-3 rounded-md border border-[var(--color-warn)] bg-[var(--color-surface)] p-2 text-xs text-[var(--color-warn)]">
            {tr(
              "library_move_modal_noop",
              undefined,
              "Source and destination are the same — pick a different folder or change the name.",
            )}
          </div>
        )}

        {destExists === "yes" && !noop && !nameInvalid && (
          <div className="mb-3 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface)] p-2 text-xs text-[var(--color-bad)]">
            {tr(
              "library_move_modal_dest_exists",
              { path: resolved },
              "Destination already exists: {path}. Rename above or pick a different folder. The PS5 won't overwrite an existing folder — if it's a leftover from a cancelled move, delete it from the File System screen first.",
            )}
          </div>
        )}

        <div className="flex items-center justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={onCancel}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
          <Button
            variant="primary"
            size="sm"
            onClick={() => onConfirm(resolved)}
            disabled={
              noop || nameInvalid || volume === "" || destExists === "yes"
            }
          >
            {tr("library_move_modal_run", undefined, "Move")}
          </Button>
        </div>

        <p className="mt-3 text-[11px] text-[var(--color-muted)]">
          {tr(
            "library_move_modal_explainer",
            undefined,
            "Copies to the new location first, then removes the original once the copy succeeds. If anything goes wrong mid-copy, the original stays put.",
          )}
        </p>
      </div>
    </div>
  );
}

/** Mount-target picker for `.ffpkg` / `.exfat` images. Volume defaults
 *  to the image's host volume (kernel-friendly — cross-volume nmount
 *  hits EPERM on some firmwares; see 2.2.42 humanizer note) but can be
 *  overridden — users legitimately want to mount a USB-stored image
 *  under `/data/homebrew/` for launcher discovery, or vice versa.
 *  Cross-volume picks surface a soft warning so the EPERM trade-off
 *  is visible without blocking the workflow.
 *
 *  Pre-2.2.25 payloads ignore `mount_point` and only honor
 *  `mount_name`. Detected via `payloadSupportsMountPoint`; when
 *  false the volume + subpath rows are hidden and the modal collapses
 *  to a name-only form (the legacy 2.2.24 behavior). */
function MountModal({
  entry,
  host,
  volumes,
  payloadVersion,
  onCancel,
  onConfirm,
}: {
  entry: LibraryEntry;
  host: string;
  /** Live writable-volumes list from `fetchVolumes`. Drives the volume
   *  dropdown options — we trust the live probe over a hardcoded
   *  fallback so a ghost USB slot doesn't appear in the picker. */
  volumes: Volume[];
  /** Reported by STATUS_ACK. Drives the
   *  `payloadSupportsMountPoint` check that gates the volume + subpath
   *  picker. */
  payloadVersion: string | null;
  onCancel: () => void;
  /** Caller decides which payload knob to set. We pass `mountPoint`
   *  when the user is using the picker (2.2.25+) and `mountName`
   *  alone when falling back. `persistDest` carries the modal's own
   *  (volume, subpath) state to runMount so it can remember the
   *  preset directly without reverse-deriving from the resolved
   *  path. */
  onConfirm: (opts: {
    mountName?: string;
    mountPoint?: string;
    readOnly?: boolean;
    persistDest?: { volume: string; subpath: string };
  }) => void;
}) {
  const tr = useTr();

  const supportsMountPoint = payloadSupportsMountPoint(payloadVersion);

  // Volume picker options. Two sources merged:
  //   1. Live writable, non-placeholder mounts from the FS_LIST_VOLUMES
  //      probe. Hot-plug aware: a USB or M.2 slot only appears when
  //      the kernel has the drive attached.
  //   2. A small set of always-allowed bases the payload's
  //      `is_path_allowed` accepts even when the live probe hasn't
  //      returned yet:
  //        - `/data` — internal SSD; the live probe normally surfaces
  //          this, but during cold-start the modal would otherwise
  //          show an empty dropdown and block the user.
  //        - `/mnt/ps5upload` — payload's tool-private mount namespace.
  //          The kernel doesn't expose this as a "volume" until at
  //          least one mount exists there, so without the static
  //          inclusion the legacy mount path becomes inaccessible
  //          via the picker.
  //   `/user` (a nullfs view of `/data`) is intentionally omitted —
  //   mounting on /user/foo and /data/foo land on the same bytes,
  //   and exposing both in the picker confuses more than it helps.
  //   Per-volume listings sorted alphabetically; ghost USB slots
  //   ("/mnt/usb1" with no hardware) stay out because they only
  //   appear when the live probe reports them.
  const dropdownPaths = useMemo(() => {
    const live = volumes
      .filter((v) => v.writable && !v.is_placeholder)
      .map((v) => v.path);
    const merged = new Set([...live, "/data", "/mnt/ps5upload"]);
    return [...merged].sort();
  }, [volumes]);
  const freeBytesByPath = useMemo(() => {
    const m = new Map<string, number>();
    for (const v of volumes) m.set(v.path, v.free_bytes);
    return m;
  }, [volumes]);

  // Image's host volume — the kernel-friendly default. Cross-volume
  // picks surface a soft warning but aren't blocked.
  const imageVolume =
    entry.kind === "image" && entry.volume ? entry.volume : null;

  // Auto-derive a leaf name from the image basename. Strip the
  // extension so a `dead-space.exfat` image lands at
  // `…/dead-space/`, not `…/dead-space.exfat/`. The user can override.
  const derivedName = useMemo(() => {
    const base = entry.name.replace(/\\/g, "/").split("/").pop() ?? entry.name;
    return base.replace(/\.(exfat|ffpkg|ffpfs)$/i, "");
  }, [entry.name]);

  // Initial volume + subpath. Saved dest (per-host) wins ONLY when its
  // volume matches the image's host volume — otherwise the saved value
  // could surprise the user with a cross-volume default that hits EPERM
  // (see 2.2.46 fix). For mismatches: default to image's volume +
  // saved subpath (preserves the user's chosen folder convention).
  const initialDest = useMemo(() => {
    const saved = loadMountDest(host);
    if (saved && (!imageVolume || saved.volume === imageVolume)) {
      return saved;
    }
    return {
      volume: imageVolume ?? dropdownPaths[0] ?? "/data",
      subpath: saved?.subpath ?? MOUNT_DEFAULT_SUBPATH,
    };
    // dropdownPaths is computed once on first render and we don't
    // need to recompute when it changes — the user's saved dest +
    // image-volume is the source of truth.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host, imageVolume]);
  const [volume, setVolume] = useState<string>(initialDest.volume);
  const [subpath, setSubpath] = useState<string>(initialDest.subpath);
  const [name, setName] = useState<string>(derivedName);
  // Read-only mount toggle. Default false (RW) — matches the behavior
  // of every prior payload version. PFS images are typically save
  // data and benefit from RO; .ffpkg / .exfat depend on the user's
  // intent. Other PS5 mount tools support per-image overrides; we keep
  // it as a single checkbox per mount click for now. Pre-2.2.26 payloads silently
  // ignore the field and always mount RW; we don't surface a banner
  // for that since the user always sees the result in the resolved
  // mount and can re-mount with the right toggle.
  const [readOnly, setReadOnly] = useState<boolean>(false);
  const supportsReadOnly = useMemo(
    () => {
      // Reuse semver compare via the existing payloadSupportsMountPoint
      // pattern — if the payload supports 2.2.25's mount_point it
      // probably hasn't been upgraded to 2.2.26 yet either, but we
      // surface the toggle anyway and let the payload silently ignore
      // it on older builds. Effect: a checkbox the user can tick that
      // becomes effective the moment they upgrade. No false-promise.
      return payloadVersion ? true : false;
    },
    [payloadVersion],
  );

  // Escape closes the modal — same standard-dialog UX as MoveModal.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel]);

  // Resolved final mount path. The picker is hidden on pre-2.2.25
  // payloads, so falling back to a "/mnt/ps5upload/<name>" preview
  // matches what the payload would actually do.
  const resolvedPath = supportsMountPoint
    ? resolveMountPath(volume, subpath, name)
    : `/mnt/ps5upload/${name}`;

  // Validation. Reject names with slashes / "." / ".." since the
  // payload would refuse them, and surface that inline rather than
  // letting the user click Mount and read a back-end error.
  const nameInvalid =
    name.trim() === "" ||
    name.includes("/") ||
    name.includes("\\") ||
    name === "." ||
    name === "..";

  // Soft warning: mounting outside /mnt/ps5upload/ is allowed but
  // some third-party PS5 game scanners only look at /mnt/ps5upload/.
  // Show the note when the user picks a non-default location so they
  // know what they're trading off.
  const isLegacyRoot =
    !supportsMountPoint || resolvedPath.startsWith("/mnt/ps5upload/");

  // Cross-volume warning: image lives on volume A, user picked volume B.
  // Some firmwares EPERM the nmount; we don't block (other firmwares
  // accept it fine, and there are legitimate workflows like "image on
  // USB, mount under /data/homebrew/ for launcher discovery"). Just
  // surface the trade-off so an EPERM failure later makes sense.
  //
  // Exemption: `/mnt/ps5upload/*` is the payload's tool-private mount
  // namespace, not a separate physical drive — it's the historical
  // default mount root and works regardless of which volume the image
  // lives on. Warning there would be noise.
  const crossVolume =
    supportsMountPoint &&
    imageVolume !== null &&
    volume !== imageVolume &&
    volume !== "/mnt/ps5upload";

  const formatFree = (bytes: number) => {
    const gib = bytes / 1024 ** 3;
    if (gib >= 1024) return `${(gib / 1024).toFixed(1)} TB free`;
    if (gib >= 10) return `${gib.toFixed(0)} GB free`;
    return `${gib.toFixed(1)} GB free`;
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
      onClick={onCancel}
    >
      <div
        className="w-full max-w-lg rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="mb-3 flex items-center gap-2 text-sm font-semibold">
          <Play size={14} />
          {tr(
            "library_mount_modal_title",
            { name: entry.name },
            `Mount "${entry.name}"`,
          )}
        </header>

        <dl className="mb-4 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
          <dt className="text-[var(--color-muted)]">
            {tr("library_mount_modal_source", undefined, "From")}
          </dt>
          <dd className="font-mono break-all text-[var(--color-text)]">
            {entry.path}
          </dd>
        </dl>

        {!supportsMountPoint && (
          <div className="mb-3 rounded-md border border-[var(--color-warn)] bg-[var(--color-surface)] p-2 text-[11px] text-[var(--color-warn)]">
            {tr(
              "library_mount_modal_old_payload",
              undefined,
              "Volume picker requires payload 2.2.25+. The running payload only supports naming the mount under /mnt/ps5upload/. Click \"Replace payload\" on the Connection screen to enable picking a different volume.",
            )}
          </div>
        )}

        {supportsMountPoint && (
          <>
            <label className="mb-2 block text-[10px] font-semibold uppercase tracking-wide text-[var(--color-muted)]">
              {tr("library_mount_modal_destination", undefined, "Mount under")}
            </label>
            <div className="mb-3 flex items-center gap-2 text-sm">
              <select
                value={volume}
                onChange={(e) => setVolume(e.target.value)}
                className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-1.5 text-sm"
              >
                {(dropdownPaths.includes(volume)
                  ? dropdownPaths
                  : [volume, ...dropdownPaths]
                ).map((p) => {
                  const free = freeBytesByPath.get(p);
                  return (
                    <option key={p} value={p}>
                      {free !== undefined ? `${p} (${formatFree(free)})` : p}
                    </option>
                  );
                })}
              </select>
              <span className="text-[var(--color-muted)]">/</span>
              <input
                value={subpath}
                onChange={(e) => setSubpath(e.target.value)}
                placeholder={tr(
                  "library_mount_modal_subpath_placeholder",
                  undefined,
                  "subpath (e.g. homebrew)",
                )}
                className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-1.5 text-sm"
              />
            </div>

            <div className="mb-3 flex flex-wrap items-center gap-1.5">
              <span className="text-xs text-[var(--color-muted)]">
                {tr("library_mount_modal_presets", undefined, "Presets:")}
              </span>
              {MOUNT_PRESETS.map((p) => {
                const active = subpath === p.subpath;
                return (
                  <button
                    key={p.subpath}
                    type="button"
                    onClick={() => setSubpath(p.subpath)}
                    title={p.hint}
                    className={`rounded-full border px-2.5 py-0.5 text-xs transition-colors ${
                      active
                        ? "border-[var(--color-accent)] bg-[var(--color-accent)] text-[var(--color-accent-contrast)]"
                        : "border-[var(--color-border)] hover:bg-[var(--color-surface-3)]"
                    }`}
                  >
                    {p.label}
                  </button>
                );
              })}
            </div>
          </>
        )}

        <label className="mb-2 block text-[10px] font-semibold uppercase tracking-wide text-[var(--color-muted)]">
          {tr("library_mount_modal_name", undefined, "Name")}
        </label>
        <div className="mb-3">
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder={derivedName}
            className={`w-full rounded-md border bg-[var(--color-surface)] px-3 py-1.5 text-sm ${
              nameInvalid
                ? "border-[var(--color-bad)]"
                : "border-[var(--color-border)]"
            }`}
          />
          <p className="mt-1 text-[10px] text-[var(--color-muted)]">
            {tr(
              "library_mount_modal_name_hint",
              { default: derivedName },
              `Folder name under the chosen path. Defaults to "${derivedName}". No slashes.`,
            )}
          </p>
        </div>

        <div className="mb-4 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-xs">
          <div className="text-[10px] uppercase tracking-wide text-[var(--color-muted)]">
            {tr(
              "library_mount_modal_resolved",
              undefined,
              "Will mount at",
            )}
          </div>
          <div className="mt-0.5 break-all font-mono">{resolvedPath}</div>
        </div>

        {/* Pre-flight warning for the Sony-reserved /mnt/usb* and
         * /mnt/ext* namespaces. The PS5 kernel binds these to its USB
         * hotplug daemon's authid, and nmount from a debugger-authid
         * payload typically returns EPERM ("Operation not permitted")
         * — well-documented behaviour on retail FW. Surface the
         * trade-off BEFORE the user clicks Mount, so a likely-to-fail
         * pick gets a chance to switch path without a round-trip to
         * the engine. Doesn't disable the button; some firmware /
         * setup combinations DO accept it. */}
        {supportsMountPoint && /^\/mnt\/(usb|ext)\d/.test(volume) && (
          <div className="mb-4 rounded-md border border-[var(--color-warn)] bg-[var(--color-surface)] p-2 text-[11px] text-[var(--color-warn)]">
            <div className="mb-1 font-semibold">
              {tr(
                "library_mount_modal_usb_warn_title",
                undefined,
                "Sony-reserved volume — mount may be refused (EPERM)",
              )}
            </div>
            <div className="opacity-90">
              {tr(
                "library_mount_modal_usb_warn_body",
                undefined,
                "/mnt/usb* and /mnt/ext* belong to the PS5 kernel's USB hotplug namespace; mounting an image there is blocked by kernel policy on most firmware. If this fails with \"Operation not permitted\", switch to /mnt/ps5upload (the payload's private mount root) or /data — both work without kernel patches.",
              )}
            </div>
            <button
              type="button"
              onClick={() => {
                setVolume("/mnt/ps5upload");
                setSubpath("");
              }}
              className="mt-1.5 rounded border border-[var(--color-warn)] bg-[var(--color-surface)] px-2 py-0.5 text-[10px] hover:bg-[var(--color-surface-3)]"
            >
              {tr(
                "library_mount_modal_usb_warn_switch",
                undefined,
                "Switch to /mnt/ps5upload/",
              )}
            </button>
          </div>
        )}

        {supportsReadOnly && (
          <label className="mb-3 flex cursor-pointer items-start gap-2 text-xs">
            <input
              type="checkbox"
              className="mt-[3px]"
              checked={readOnly}
              onChange={(e) => setReadOnly(e.target.checked)}
            />
            <span>
              <span className="font-medium">
                {tr(
                  "library_mount_modal_read_only",
                  undefined,
                  "Mount read-only",
                )}
              </span>
              <span className="ml-1 text-[var(--color-muted)]">
                {tr(
                  "library_mount_modal_read_only_hint",
                  undefined,
                  "— prevents writes through the mount. Useful for shared dumps and save-data PFS images. Requires payload 2.2.26+.",
                )}
              </span>
            </span>
          </label>
        )}

        {nameInvalid && (
          <div className="mb-3 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface)] p-2 text-xs text-[var(--color-bad)]">
            {tr(
              "library_mount_modal_name_invalid",
              undefined,
              "Name can't be empty, contain / or \\, or be \".\" / \"..\".",
            )}
          </div>
        )}

        {!isLegacyRoot && !nameInvalid && (
          <div className="mb-3 rounded-md border border-[var(--color-warn)] bg-[var(--color-surface)] p-2 text-[11px] text-[var(--color-warn)]">
            {tr(
              "library_mount_modal_outside_default",
              undefined,
              "Heads-up: third-party PS5 game scanners typically scan /mnt/ps5upload/ for installed games. Mounting outside that root works for the payload, but those scanners may not see this title.",
            )}
          </div>
        )}

        {crossVolume && !nameInvalid && imageVolume && (
          <div className="mb-3 rounded-md border border-[var(--color-warn)] bg-[var(--color-surface)] p-2 text-[11px] text-[var(--color-warn)]">
            {tr(
              "library_mount_modal_cross_volume",
              { imageVolume, volume },
              `Heads-up: image lives on ${imageVolume} but you picked ${volume}. Some PS5 firmwares refuse cross-volume nmount with EPERM (the kernel won't mount a file from one drive into a path on another). If the mount fails, retry with ${imageVolume} as the volume.`,
            )}
          </div>
        )}

        <div className="flex items-center justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={onCancel}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
          <Button
            variant="primary"
            size="sm"
            disabled={nameInvalid}
            onClick={() => {
              if (supportsMountPoint) {
                onConfirm({
                  mountPoint: resolvedPath,
                  readOnly: readOnly || undefined,
                  persistDest: { volume, subpath },
                });
              } else {
                // Pre-2.2.25 payload — only mount_name is supported,
                // and the payload anchors it under /mnt/ps5upload/.
                // No `persistDest`: we don't have a real volume
                // choice to remember in this branch.
                onConfirm({ mountName: name, readOnly: readOnly || undefined });
              }
            }}
          >
            {tr("library_mount_modal_run", undefined, "Mount")}
          </Button>
        </div>
      </div>
    </div>
  );
}

function LibraryThumb({
  entry,
  meta,
  host,
  fallbackIcon: FallbackIcon,
}: {
  entry: LibraryEntry;
  meta: GameMeta | null;
  host: string;
  fallbackIcon: LucideIcon;
}) {
  // Local swap-on-error: the meta.has_icon probe is best-effort; the
  // actual <img> load can still 404 (e.g. mid-refresh the folder was
  // just deleted). Keeping the fallback icon rendered under the img
  // (as a sibling) means we don't have to re-render the wrapper.
  const [failed, setFailed] = useState(false);
  const showIcon = meta?.has_icon && !failed && host?.trim();
  return (
    <div className="relative flex h-12 w-12 shrink-0 items-center justify-center overflow-hidden rounded-md bg-[var(--color-surface-3)]">
      {showIcon ? (
        <img
          src={gameIconUrl(`${host}:${PS5_PAYLOAD_PORT}`, entry.path)}
          alt=""
          className="h-full w-full object-cover"
          onError={() => setFailed(true)}
        />
      ) : (
        <FallbackIcon size={20} className="text-[var(--color-muted)]" />
      )}
    </div>
  );
}

function ConfirmRow({
  confirm,
  onCancel,
  onRun,
}: {
  confirm: PendingConfirm;
  onCancel: () => void;
  onRun: () => void;
}) {
  const tr = useTr();
  const { kind, entry } = confirm;
  const message =
    kind === "delete"
      ? tr(
          "library_confirm_delete",
          { name: entry.name },
          `Delete "${entry.name}"? This removes the path recursively from your PS5 and can't be undone.`,
        )
      : tr(
          "library_confirm_chmod",
          { name: entry.name },
          `Apply Permission 777 recursively to "${entry.name}"? Anyone on your PS5 will be able to read/write/execute its contents.`,
        );
  const runLabel =
    kind === "delete"
      ? tr("library_confirm_delete_yes", undefined, "Yes, delete")
      : tr("library_confirm_chmod_yes", undefined, "Yes, set Permission 777");
  return (
    <div className="flex items-center justify-between gap-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-xs">
      <span className="flex-1">{message}</span>
      <div className="flex shrink-0 items-center gap-1">
        <Button variant="ghost" size="sm" onClick={onCancel}>
          {tr("cancel", undefined, "Cancel")}
        </Button>
        <Button
          variant={kind === "delete" ? "danger" : "primary"}
          size="sm"
          onClick={onRun}
        >
          {runLabel}
        </Button>
      </div>
    </div>
  );
}

/**
 * One-line tip surfaced in the Library "Disk images" section: nudge
 * .ffpkg/.exfat users at drakmor/kstuff-lite which has a measured
 * 3-4× faster mount path than EchoStretch's lite build. Only renders
 * if the user has at least one disk image in their library (the
 * caller's `split.images.length > 0` gate handles that). Dismissible
 * — choice persists in localStorage so it doesn't keep nagging.
 *
 * We deliberately don't try to detect which kstuff is loaded (both
 * variants drop the same /data/kstuff.elf marker; the active one is
 * only inferrable from the user's autoload list). The cost of a
 * stale tip when the user is already on drakmor is one extra
 * click on the dismiss X — cheap. The cost of NOT showing it when
 * they're on the slower default is every mount being slower than
 * it has to be.
 */
function FpkgKstuffTip() {
  const tr = useTr();
  const [dismissed, setDismissed] = useState<boolean>(() => {
    if (typeof window === "undefined") return false;
    try {
      return (
        window.localStorage.getItem(
          "ps5upload.fpkg-kstuff-tip.dismissed",
        ) === "1"
      );
    } catch {
      return false;
    }
  });
  if (dismissed) return null;
  const dismiss = () => {
    try {
      window.localStorage.setItem(
        "ps5upload.fpkg-kstuff-tip.dismissed",
        "1",
      );
    } catch {
      // best-effort persistence
    }
    setDismissed(true);
  };
  return (
    <div className="mb-2 flex items-start gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-2 text-xs">
      <Sparkles
        size={13}
        className="mt-0.5 shrink-0 text-[var(--color-accent)]"
      />
      <div className="min-w-0 flex-1 text-[var(--color-muted)]">
        {tr(
          "library_fpkg_kstuff_tip",
          undefined,
          "Tip: for faster .ffpkg / .exfat mounting (3-4×), try drakmor/kstuff-lite (FW 3.00-10.01) instead of the default kstuff. Install from the Payloads library.",
        )}{" "}
        <button
          type="button"
          onClick={() =>
            openExternal("https://github.com/drakmor/kstuff-lite").catch(
              () => {},
            )
          }
          className="inline-flex items-center gap-0.5 underline decoration-dotted hover:text-[var(--color-text)]"
        >
          {tr("library_fpkg_kstuff_tip_repo", undefined, "View on GitHub")}
          <ExternalLink size={10} />
        </button>
      </div>
      <button
        type="button"
        onClick={dismiss}
        className="shrink-0 rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
        title={tr("library_fpkg_kstuff_tip_dismiss", undefined, "Don't show again")}
      >
        ✕
      </button>
    </div>
  );
}
