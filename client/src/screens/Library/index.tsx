import { useCallback, useEffect, useMemo, useState } from "react";
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
  FolderInput,
  type LucideIcon,
} from "lucide-react";

import { useConnectionStore, PS5_PAYLOAD_PORT } from "../../state/connection";
import {
  scanLibrary,
  fsDelete,
  fsChmod,
  fsCopy,
  fsMount,
  fsUnmount,
  fetchVolumes,
  fetchGameMeta,
  gameIconUrl,
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
import { useLibraryStore } from "../../state/library";
import { useElapsed } from "../../lib/useElapsed";
import { createLimiter } from "../../lib/limitConcurrency";
import { PageHeader, EmptyState, ErrorCard, Button } from "../../components";
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

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v.toFixed(v >= 100 ? 0 : v >= 10 ? 1 : 2)} ${units[i]}`;
}

export default function LibraryScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const entries = useLibraryStore((s) => s.entries);
  const mountMap = useLibraryStore((s) => s.mountMap);
  const volumes = useLibraryStore((s) => s.volumes);
  const loading = useLibraryStore((s) => s.loading);
  const error = useLibraryStore((s) => s.error);
  const lastRefreshedAt = useLibraryStore((s) => s.lastRefreshedAt);
  const setData = useLibraryStore((s) => s.setData);
  const setLoading = useLibraryStore((s) => s.setLoading);
  const setError = useLibraryStore((s) => s.setError);

  const refresh = useCallback(async () => {
    if (!host?.trim()) return;
    setLoading(true);
    setError(null);
    try {
      const addr = `${host}:${PS5_PAYLOAD_PORT}`;
      const [result, volumes] = await Promise.all([
        scanLibrary(addr),
        fetchVolumes(addr).catch(() => []),
      ]);
      const next = new Map<string, string>();
      for (const v of volumes) {
        if (v.source_image && v.path.startsWith("/mnt/ps5upload/")) {
          next.set(v.source_image, v.path);
        }
      }
      // Move-modal destinations only make sense for real attached
      // drives, so filter out placeholder/read-only volumes here.
      const writable = volumes.filter((v) => v.writable && !v.is_placeholder);
      setData(result, next, writable);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [host, setData, setLoading, setError]);

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

  // Split by kind so games/folders render separately from disk
  // images. Memoized so the filter isn't re-run every render.
  const split = useMemo(() => {
    const games = (entries ?? []).filter((e) => e.kind === "game");
    const images = (entries ?? []).filter((e) => e.kind === "image");
    return { games, images };
  }, [entries]);

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
          "Games and disk images anywhere on your PS5. Games are folders containing sce_sys/param.json; disk images are .exfat and .ffpkg files.",
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
                title={tr("library_disk_images", undefined, "Disk images (.exfat / .ffpkg)")}
                count={split.images.length}
              />
              <div className="grid gap-2">
                {split.images.map((e, i) => (
                  <LibraryRow
                    key={`${e.path}-${i}`}
                    entry={e}
                    host={host}
                    mountMap={mountMap}
                    volumes={volumes}
                    onChanged={refresh}
                  />
                ))}
              </div>
            </section>
          )}
        </div>
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
  | "move-deleting";

function LibraryRow({
  entry,
  host,
  mountMap,
  volumes,
  onChanged,
}: {
  entry: LibraryEntry;
  host: string;
  /** image_path → mount_point. Lets image rows render MOUNTED state
   *  + offer the right action (Mount vs Unmount). */
  mountMap: Map<string, string>;
  /** Writable PS5 volumes — surfaced to the Move modal as the
   *  destination dropdown. */
  volumes: Volume[];
  onChanged: () => void;
}) {
  const tr = useTr();
  const Icon = entry.kind === "game" ? Gamepad2 : FileArchive;
  const kindLabel =
    entry.kind === "game"
      ? tr("library_row_kind_game", undefined, "Game")
      : entry.imageFormat === "ffpkg"
        ? tr("library_row_kind_ffpkg", undefined, ".ffpkg image")
        : tr("library_row_kind_exfat", undefined, ".exfat image");
  const [confirm, setConfirm] = useState<PendingConfirm | null>(null);
  const [busy, setBusy] = useState<BusyState>(null);
  const [error, setError] = useState<string | null>(null);
  const [mountNote, setMountNote] = useState<string | null>(null);
  const [meta, setMeta] = useState<GameMeta | null>(null);
  const [moveOpen, setMoveOpen] = useState(false);
  const elapsedMs = useElapsed(busy !== null);

  /** Current mount point for this entry (null = not mounted). Only
   *  meaningful for image rows — games don't go through fs_mount. */
  const currentMount = entry.kind === "image"
    ? mountMap.get(entry.path) ?? null
    : null;
  const isMounted = currentMount !== null;

  // Metadata fetch: skipped for disk images (single file, no sce_sys
  // inside) and for generic "folder" kind under non-game scopes.
  // Cancellation guard prevents a late response from overwriting state
  // after the row unmounts or the host changes. Errors swallowed —
  // fetchGameMeta already returns a blank GameMeta on failure.
  useEffect(() => {
    if (entry.kind === "image") return;
    if (!host?.trim()) return;
    let cancelled = false;
    metaLimit(() =>
      fetchGameMeta(`${host}:${PS5_PAYLOAD_PORT}`, entry.path)
    ).then((m) => {
      if (!cancelled) setMeta(m);
    });
    return () => {
      cancelled = true;
    };
  }, [entry.kind, entry.path, host]);

  const runDelete = async () => {
    setBusy("delete");
    setError(null);
    try {
      await fsDelete(`${host}:${PS5_PAYLOAD_PORT}`, entry.path);
      onChanged();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
      setConfirm(null);
    }
  };
  const runChmod = async () => {
    setBusy("chmod");
    setError(null);
    try {
      await fsChmod(
        `${host}:${PS5_PAYLOAD_PORT}`,
        entry.path,
        "0777",
        entry.kind !== "image" // recursive on dirs, single-file on disk images
      );
      onChanged();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
      setConfirm(null);
    }
  };

  // Mount: the payload does this itself — attaches /dev/md<N> over the
  // image file, nmount's with exfatfs (.exfat) or ufs (.ffpkg), puts the
  // result under /mnt/ps5upload/<name>/. No external scene tool involved.
  // We surface the returned mount_point so the user knows where to find it.
  const runMount = async () => {
    if (entry.kind !== "image") return;
    setBusy("mount");
    setError(null);
    setMountNote(null);
    const addr = `${host}:${PS5_PAYLOAD_PORT}`;
    try {
      const res = await fsMount(addr, entry.path);
      setMountNote(
        `Mounted at ${res.mount_point}. Refresh to see the games inside — register + launch them from a PS5-side installer.`,
      );
      onChanged();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
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
    const addr = `${host}:${PS5_PAYLOAD_PORT}`;
    try {
      await fsCopy(addr, entry.path, destPath);
    } catch (e) {
      setError(
        tr(
          "library_move_copy_failed",
          { error: e instanceof Error ? e.message : String(e) },
          `Couldn't copy to the new location: ${
            e instanceof Error ? e.message : String(e)
          }. Source is unchanged.`,
        ),
      );
      setBusy(null);
      return;
    }
    setBusy("move-deleting");
    let lastErr: unknown = null;
    for (let attempt = 1; attempt <= 3; attempt++) {
      try {
        await fsDelete(addr, entry.path);
        lastErr = null;
        break;
      } catch (e) {
        lastErr = e;
        if (attempt < 3) {
          // Linear backoff — 500 ms, 1 s. Short enough that the user
          // doesn't notice unless we're persistently losing the race.
          await new Promise((resolve) => setTimeout(resolve, 500 * attempt));
        }
      }
    }
    if (lastErr !== null) {
      setError(
        tr(
          "library_move_delete_failed",
          {
            dest: destPath,
            src: entry.path,
            error:
              lastErr instanceof Error ? lastErr.message : String(lastErr),
          },
          `Copied to ${destPath}, but couldn't remove the source ${entry.path} after 3 attempts: ${
            lastErr instanceof Error ? lastErr.message : String(lastErr)
          }. Both copies now exist — delete the original yourself when ready.`,
        ),
      );
      setBusy(null);
      onChanged();
      return;
    }
    setMountNote(
      tr(
        "library_move_succeeded",
        { dest: destPath },
        `Moved to ${destPath}.`,
      ),
    );
    setBusy(null);
    onChanged();
  };

  /** Unmount: flipped from Mount when the archive is currently
   *  mounted. Runs the same fsUnmount the Volumes screen uses.
   *  onChanged refreshes the volumes list which feeds the mountMap. */
  const runUnmount = async () => {
    if (!isMounted || !currentMount) return;
    setBusy("unmount");
    setError(null);
    setMountNote(null);
    try {
      await fsUnmount(`${host}:${PS5_PAYLOAD_PORT}`, currentMount);
      setMountNote(`Unmounted ${currentMount}.`);
      onChanged();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
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
              mounted
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
                onClick={runMount}
                disabled={busy !== null}
                loading={busy === "mount"}
                title={tr("library_mount_tooltip", undefined, "Mount this image on your PS5")}
              >
                {tr("library_mount", undefined, "Mount")}
              </Button>
            )
          ) : (
            /* Game rows: Permission 777 + Delete only. Install / Run /
             * Uninstall are intentionally absent — Sony's install and
             * launch APIs wedge our standalone userland payload on
             * firmware 9.60 (hardware-verified 2026-04-19). Those
             * actions are delegated to PS5-side tools (etaHEN,
             * ShadowMountPlus, Itemzflow) which inject into SceShellUI
             * where the APIs behave normally. */
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<Shield size={12} />}
              onClick={() => setConfirm({ kind: "chmod", entry })}
              disabled={busy !== null}
              loading={busy === "chmod"}
              title={tr("library_chmod_tooltip", undefined, "Open read/write/execute to every user on this PS5 (Permission 777)")}
            >
              {tr("library_permission_777", undefined, "Permission 777")}
            </Button>
          )}
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<FolderInput size={12} />}
            onClick={() => setMoveOpen(true)}
            disabled={busy !== null || volumes.length === 0}
            title={
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
                  )
            }
          >
            {tr("library_move", undefined, "Move")}
          </Button>
          <Button
            variant="danger"
            size="sm"
            leftIcon={<Trash2 size={12} />}
            onClick={() => setConfirm({ kind: "delete", entry })}
            disabled={busy !== null}
            loading={busy === "delete"}
            title={tr("library_delete_tooltip", undefined, "Delete this path from the PS5")}
          >
            {tr("library_delete", undefined, "Delete")}
          </Button>
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
                      : tr(
                          "library_busy_move_deleting",
                          undefined,
                          "Cleaning up source",
                        )}
          </span>
          <span className="text-[var(--color-muted)]">
            {entry.name} · {formatDuration(elapsedMs / 1000)}
          </span>
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
          onCancel={() => setMoveOpen(false)}
          onConfirm={runMove}
        />
      )}
    </article>
  );
}

/** Move-target picker. Source path is read-only; user picks the
 *  destination volume + subpath, sees the resolved final path before
 *  committing. Pre-fills with the entry's current volume + parent dir
 *  so a "move within the same drive" stays one click away. */
function MoveModal({
  entry,
  volumes,
  onCancel,
  onConfirm,
}: {
  entry: LibraryEntry;
  volumes: Volume[];
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
            `Move "${entry.name}"`,
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
              `Leave blank or matching "${sourceBasename(entry.path)}" to keep the original name. No slashes.`,
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

        <div className="flex items-center justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={onCancel}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
          <Button
            variant="primary"
            size="sm"
            onClick={() => onConfirm(resolved)}
            disabled={noop || nameInvalid || volume === ""}
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
