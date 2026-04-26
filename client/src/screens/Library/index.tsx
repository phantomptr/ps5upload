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
  type LucideIcon,
} from "lucide-react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

import { useConnectionStore, PS5_PAYLOAD_PORT } from "../../state/connection";
import {
  scanLibrary,
  fsDelete,
  fsChmod,
  fsCopy,
  fsPathExists,
  fsMount,
  fsUnmount,
  fsOpStatus,
  fsOpCancel,
  fetchVolumes,
  fetchGameMeta,
  gameIconUrl,
  jobStatus,
  startTransferDownload,
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
import { deleteWithRetry } from "../../lib/deleteWithRetry";
import { useActivityHistoryStore } from "../../state/activityHistory";
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
  | "move-deleting"
  | "download";

interface DownloadProgress {
  bytesReceived: number;
  totalBytes: number;
}

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
  // Set true if the FS_OP_STATUS poller hits "unsupported_frame" —
  // means the running PS5 payload predates 2.2.7's status-frame
  // handler. The banner shows a hint so the user knows to "Replace
  // payload" on the Connection screen instead of assuming the
  // progress UI is broken.
  const [moveProgressUnsupported, setMoveProgressUnsupported] = useState(false);
  // User-requested abort for the move's in-flight fs_copy. Set by the
  // Stop button; the side-watcher fires fsOpCancel as soon as it
  // observes this flip, and the payload's cp_rf bails within ~one
  // 16 MiB buffer.
  const moveStopRef = useRef(false);
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
    setMoveProgress({ bytesCopied: 0, totalBytes: 0 });
    setMoveProgressUnsupported(false);
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
      // Small initial delay so the payload has time to register the
      // op (and to recursively walk total_bytes) before we ask for a
      // snapshot — saves a wasted 404 round-trip.
      await new Promise((r) => setTimeout(r, 250));
      while (!pollerStopped) {
        try {
          const snap = await fsOpStatus(addr, opId);
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
          // 404 (op not yet registered or already finished) and
          // transient errors keep the poller alive. Surface
          // non-404 errors to console so a payload version
          // mismatch (older payload that doesn't know
          // FS_OP_STATUS) is visible during debug.
          const msg = e instanceof Error ? e.message : String(e);
          if (msg.includes("unsupported_frame")) {
            // Older payload — won't ever produce progress. Stop
            // polling and surface the hint in both the row and the
            // Activity tab (the activity entry's `error` field
            // doubles as a "note" while `outcome === running`,
            // which the Activity row knows how to render in a
            // less-alarming style than for terminal failures).
            if (mountedRef.current) setMoveProgressUnsupported(true);
            useActivityHistoryStore.getState().update(activityId, {
              error:
                "Live progress unavailable — payload predates 2.2.7 FS_OP_STATUS. Click Replace payload on the Connection screen.",
            });
            break;
          }
          if (msg.includes("decode FS_OP_STATUS_ACK body")) {
            // Pre-2.2.16 payloads truncated the FS_OP_STATUS_ACK
            // JSON body when both paths were long, so every poll
            // produced unparseable bytes. The fix is on the payload
            // side; until it's redeployed there's no point hammering
            // the engine 2×/sec with the same parse failure. Treat
            // it the same as `unsupported_frame`: stop polling, hint
            // the user to push a fresh payload.
            if (mountedRef.current) setMoveProgressUnsupported(true);
            useActivityHistoryStore.getState().update(activityId, {
              error:
                "Live progress unavailable — payload predates 2.2.16 FS_OP_STATUS_ACK fix. Click Replace payload on the Connection screen, or run `make send-payload`.",
            });
            break;
          }
          if (!msg.includes("404") && !msg.includes("not in flight")) {
            console.warn("[library] FS_OP_STATUS poll failed:", msg);
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
            `Couldn't copy to the new location: ${msg}. Source is unchanged.`,
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
          `Copied to ${destPath}, but couldn't remove the source ${entry.path} after 3 attempts: ${lastErrMsg}. Both copies now exist — delete the original yourself when ready.`,
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
        `Moved to ${destPath}.`,
      ),
    );
    useActivityHistoryStore.getState().finish(activityId, "done");
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
        `Pick a destination folder for "${entry.name}"`,
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
    let jobId: string;
    try {
      jobId = await startTransferDownload(entry.path, picked, addr, kind);
    } catch (e) {
      setError(
        tr(
          "library_download_start_failed",
          { error: e instanceof Error ? e.message : String(e) },
          `Couldn't start the download: ${
            e instanceof Error ? e.message : String(e)
          }`,
        ),
      );
      setBusy(null);
      setDownloadProgress(null);
      return;
    }
    // Poll until terminal. The mountedRef gate makes a navigate-away
    // mid-download silently exit the loop instead of writing state
    // on an unmounted component. The engine job keeps running on the
    // engine side (no engine cancel API today); the file still lands
    // on disk and the user finds it where they picked, just without
    // the in-row "Done" note.
    while (true) {
      if (!mountedRef.current) return;
      if (downloadStopRef.current) {
        // User clicked Stop. Engine job continues server-side; we
        // just stop polling. Surface a note so the row clears the
        // spinner with a clear "you stopped this" instead of going
        // back to idle silently.
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
        if (!mountedRef.current) return;
        if (snap.status === "done") {
          setMountNote(
            tr(
              "library_download_succeeded",
              {
                dest: snap.dest ?? picked,
                bytes: snap.bytes_sent ?? 0,
              },
              `Downloaded to ${snap.dest ?? picked} (${snap.bytes_sent ?? 0} bytes).`,
            ),
          );
          setBusy(null);
          setDownloadProgress(null);
          return;
        }
        if (snap.status === "failed") {
          setError(
            tr(
              "library_download_failed",
              { error: snap.error ?? "download failed" },
              `Download failed: ${snap.error ?? "unknown error"}`,
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
      } catch (e) {
        if (!mountedRef.current) return;
        setError(
          tr(
            "library_download_poll_failed",
            { error: e instanceof Error ? e.message : String(e) },
            `Lost contact with the engine while downloading: ${
              e instanceof Error ? e.message : String(e)
            }`,
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
            leftIcon={<Download size={12} />}
            onClick={runDownload}
            disabled={busy !== null}
            loading={busy === "download"}
            title={tr(
              "library_download_tooltip",
              undefined,
              "Save a copy of this entry to a folder on this computer",
            )}
          >
            {tr("library_download", undefined, "Download")}
          </Button>
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
          payload that doesn't speak FS_OP_STATUS. Without this it
          looks like the progress UI is broken; in fact the protocol
          frame just isn't recognized on the other side. */}
      {busy === "move-copying" && moveProgressUnsupported && (
        <div className="rounded-md border border-[var(--color-warn)] bg-[var(--color-warn-soft)] p-2 text-[11px] text-[var(--color-warn)]">
          {tr(
            "library_move_progress_unsupported",
            undefined,
            "Live progress unavailable — your PS5 payload is older than this app. Click \"Replace payload\" on the Connection screen to enable per-byte progress + cancel.",
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

        {destExists === "yes" && !noop && !nameInvalid && (
          <div className="mb-3 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface)] p-2 text-xs text-[var(--color-bad)]">
            {tr(
              "library_move_modal_dest_exists",
              { path: resolved },
              `Destination already exists: ${resolved}. Rename above or pick a different folder. The PS5 won't overwrite an existing folder — if it's a leftover from a cancelled move, delete it from the File System screen first.`,
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
