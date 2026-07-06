import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Video as VideoIcon,
  RefreshCw,
  Loader2,
  Download,
  CheckSquare,
  Square,
  Eye,
  X,
} from "lucide-react";
import { convertFileSrc } from "@tauri-apps/api/core";
import {
  waitForJob,
  videosList,
  startTransferDownload,
  saveArchiveMakeTemp,
  saveArchiveCleanupTemp,
  type ScreenshotEntry,
} from "../../api/ps5";
import { useConnectionStore, PS5_PAYLOAD_PORT } from "../../state/connection";
import { mgmtAddr } from "../../lib/addr";
import { useScrollLock } from "../../lib/useScrollLock";
import { useStaleHostGuard } from "../../lib/staleHostGuard";
import { PageHeader, Button, EmptyState, ErrorCard } from "../../components";
import { useTr } from "../../state/lang";
import { pickPath } from "../../lib/pickPath";
import { formatBytes } from "../../lib/format";
import { basename } from "../../lib/uploadDest";
import { isMobile } from "../../lib/platform";

/** Join a dir + name the same way the screenshot flow does (mirrors
 *  lib/screenshotConvert.joinDir, kept local so Videos has no dependency on
 *  the JXR-conversion module it doesn't otherwise need). */
function joinDir(dir: string, name: string): string {
  return dir.endsWith("/") ? `${dir}${name}` : `${dir}/${name}`;
}

/**
 * Video clips screen — mirrors Screenshots but simpler: PS5 gameplay clips
 * (`.webm`/`.mp4` under `/user/av_contents/video`) are already playable
 * formats, so there's no `.jxr → PNG` conversion step. List with checkbox
 * selection, per-row + bulk download to the host, and an inline `<video>`
 * preview (desktop only — the preview streams a downloaded copy via
 * convertFileSrc).
 *
 * NB: the on-console video path/extension the payload walks is the
 * documented Sony layout but hasn't been confirmed against a live console
 * in-house — if this screen is empty with clips present, the payload's
 * walk_videos() root/extension filter is what to adjust (see runtime.c).
 */
export default function VideosScreen() {
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const tr = useTr();
  const guard = useStaleHostGuard();

  const [items, setItems] = useState<ScreenshotEntry[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [bulkBusy, setBulkBusy] = useState(false);
  const [busyPaths, setBusyPaths] = useState<Set<string>>(new Set());
  // Inline <video> preview is desktop-only (streams a downloaded copy from a
  // scratch dir via convertFileSrc — no mobile file-serving equivalent).
  const canPreview = !isMobile();

  const allSelected = useMemo(
    () => !!items && items.length > 0 && selected.size === items.length,
    [items, selected],
  );

  function toggleOne(path: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }

  function toggleAll() {
    if (!items) return;
    if (allSelected) setSelected(new Set());
    else setSelected(new Set(items.map((i) => i.path)));
  }

  const refresh = useCallback(async () => {
    if (!host?.trim() || payloadStatus !== "up") return;
    // Host-stale guard: videosList can take a moment; if the user switches
    // PS5 mid-list, OLD's clips must not appear under NEW's name.
    const probe = guard.capture();
    setLoading(true);
    setError(null);
    try {
      const r = await videosList(mgmtAddr(probe.host));
      if (probe.isStale()) return;
      setItems(r.items);
    } catch (e) {
      if (probe.isStale()) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [host, payloadStatus, guard]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // Clear rendered list + selection the instant the active console changes,
  // before the new console's videosList resolves — same reasoning as
  // Screenshots (stops A's rows/selection leaking into B).
  useEffect(() => {
    setItems(null);
    setSelected(new Set());
    setError(null);
  }, [host]);

  async function downloadOne(item: ScreenshotEntry) {
    if (!host?.trim()) return;
    const dest = await pickPath({
      mode: "folder",
      title: tr("videos_dest_pick", undefined, "Pick a folder to download into"),
    });
    if (!dest || typeof dest !== "string") return;
    setBusyPaths((s) => new Set(s).add(item.path));
    try {
      const jobId = await startTransferDownload(
        item.path,
        dest,
        `${host.trim()}:${PS5_PAYLOAD_PORT}`,
        "file",
      );
      await waitForJob(jobId);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusyPaths((s) => {
        const next = new Set(s);
        next.delete(item.path);
        return next;
      });
    }
  }

  async function downloadSelected() {
    if (!host?.trim() || selected.size === 0) return;
    const dest = await pickPath({
      mode: "folder",
      title: tr(
        "videos_bulk_dest",
        { n: selected.size },
        `Pick a folder for ${selected.size} clips`,
      ),
    });
    if (!dest || typeof dest !== "string") return;
    setBulkBusy(true);
    setError(null);
    try {
      // Per-item: one failed clip (transient drop) must not abort the batch.
      // Keep only the failed paths selected so a re-run retries just those.
      const failed: string[] = [];
      let okCount = 0;
      for (const path of selected) {
        try {
          const jobId = await startTransferDownload(
            path,
            dest,
            `${host.trim()}:${PS5_PAYLOAD_PORT}`,
            "file",
          );
          await waitForJob(jobId);
          okCount++;
        } catch (e) {
          failed.push(path);
          console.warn("[videos] download failed:", path, e);
        }
      }
      setSelected(new Set(failed));
      if (failed.length > 0) {
        setError(
          tr(
            "videos_bulk_partial",
            { ok: okCount, failed: failed.length },
            `Downloaded ${okCount}; ${failed.length} failed — failed ones stay selected to retry.`,
          ),
        );
      }
    } finally {
      setBulkBusy(false);
    }
  }

  // ─── Inline preview ─────────────────────────────────────────────────
  const [preview, setPreview] = useState<{
    item: ScreenshotEntry;
    url: string | null;
    tempDir: string | null;
    loading: boolean;
    error: string | null;
  } | null>(null);
  useScrollLock(!!preview);

  async function openPreview(item: ScreenshotEntry) {
    if (!host?.trim() || !canPreview) return;
    setPreview((prev) => {
      if (prev?.tempDir) void saveArchiveCleanupTemp(prev.tempDir).catch(() => {});
      return { item, url: null, tempDir: null, loading: true, error: null };
    });
    let tempDir: string | null = null;
    try {
      tempDir = await saveArchiveMakeTemp("clip-preview");
      const jobId = await startTransferDownload(
        item.path,
        tempDir,
        `${host.trim()}:${PS5_PAYLOAD_PORT}`,
        "file",
      );
      await waitForJob(jobId);
      const url = convertFileSrc(joinDir(tempDir, basename(item.path)));
      const created = tempDir;
      setPreview((p) => {
        if (p && p.item.path === item.path) {
          return { ...p, url, tempDir: created, loading: false };
        }
        void saveArchiveCleanupTemp(created).catch(() => {});
        return p;
      });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      if (tempDir) await saveArchiveCleanupTemp(tempDir).catch(() => {});
      setPreview((p) =>
        p && p.item.path === item.path
          ? { ...p, loading: false, error: msg, tempDir: null }
          : p,
      );
    }
  }

  function closePreview() {
    setPreview((p) => {
      if (p?.tempDir) void saveArchiveCleanupTemp(p.tempDir).catch(() => {});
      return null;
    });
  }

  // Esc closes the preview; clean the scratch dir if the user navigates away
  // with it open.
  useEffect(() => {
    if (!preview) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closePreview();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [preview]);
  useEffect(
    () => () => {
      // Unmount cleanup handled via closePreview on Esc/close; this catches
      // navigate-away with a preview still open.
      setPreview((p) => {
        if (p?.tempDir) void saveArchiveCleanupTemp(p.tempDir).catch(() => {});
        return p;
      });
    },
    [],
  );

  return (
    <div className="p-6">
      <PageHeader
        icon={VideoIcon}
        title={tr("videos_title", undefined, "Video clips")}
        count={items?.length}
        loading={loading}
        description={tr(
          "videos_description",
          undefined,
          "Gameplay video clips saved on the PS5 (Capture Gallery). Download to your computer, or delete via the File System tab. Clips download as-is — no conversion needed.",
        )}
        right={
          <div className="flex items-center gap-2">
            {selected.size > 0 && (
              <Button
                variant="primary"
                size="sm"
                leftIcon={
                  bulkBusy ? (
                    <Loader2 size={12} className="animate-spin" />
                  ) : (
                    <Download size={12} />
                  )
                }
                onClick={downloadSelected}
                disabled={bulkBusy}
              >
                {tr("videos_bulk_download", { n: selected.size }, `Download ${selected.size}`)}
              </Button>
            )}
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<RefreshCw size={12} />}
              onClick={refresh}
              disabled={loading || !host?.trim() || payloadStatus !== "up"}
            >
              {tr("refresh", undefined, "Refresh")}
            </Button>
          </div>
        }
      />

      {payloadStatus !== "up" && (
        <EmptyState
          fill
          icon={VideoIcon}
          message={tr("videos_no_payload", undefined, "Connect to your PS5 first.")}
        />
      )}

      {error && (
        <div className="mb-4">
          <ErrorCard
            title={tr("videos_error", undefined, "Couldn't list video clips")}
            detail={error}
          />
        </div>
      )}

      {items && items.length === 0 && payloadStatus === "up" && (
        <EmptyState
          icon={VideoIcon}
          message={tr(
            "videos_empty",
            undefined,
            "No clips found — record a gameplay video on the PS5 first.",
          )}
        />
      )}

      <div className="mx-auto max-w-4xl">
        {items && items.length > 0 && (
          <button
            type="button"
            onClick={toggleAll}
            className="mb-2 inline-flex items-center gap-1.5 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]"
          >
            {allSelected ? <CheckSquare size={11} /> : <Square size={11} />}
            {allSelected
              ? tr("videos_deselect_all", undefined, "Deselect all")
              : tr("videos_select_all", undefined, "Select all")}
          </button>
        )}
        <ul className="space-y-1">
          {items?.map((item) => {
            const isSelected = selected.has(item.path);
            const rowBusy = busyPaths.has(item.path);
            return (
              <li
                key={item.path}
                className={`flex items-center gap-3 rounded-md border p-2 text-xs ${
                  isSelected
                    ? "border-[var(--color-accent)] bg-[var(--color-surface-2)]"
                    : "border-[var(--color-border)] bg-[var(--color-surface-2)]"
                }`}
              >
                <button
                  type="button"
                  onClick={() => toggleOne(item.path)}
                  className="text-[var(--color-muted)] hover:text-[var(--color-text)]"
                  aria-label={tr("videos_select", undefined, "Toggle select")}
                >
                  {isSelected ? <CheckSquare size={12} /> : <Square size={12} />}
                </button>
                <VideoIcon
                  size={16}
                  className="shrink-0 text-[var(--color-muted)]"
                />
                <div className="min-w-0 flex-1">
                  <div className="truncate font-medium text-[var(--color-text)]">
                    {basename(item.path)}
                  </div>
                  <div className="truncate text-[var(--color-muted)]">
                    {formatBytes(item.size)}
                  </div>
                </div>
                {canPreview && (
                  <button
                    type="button"
                    onClick={() => openPreview(item)}
                    className="inline-flex items-center gap-1 rounded px-1.5 py-1 text-[var(--color-muted)] hover:text-[var(--color-text)]"
                    aria-label={tr("videos_preview", undefined, "Preview")}
                  >
                    <Eye size={13} />
                  </button>
                )}
                <button
                  type="button"
                  onClick={() => void downloadOne(item)}
                  disabled={rowBusy}
                  className="inline-flex items-center gap-1 rounded px-1.5 py-1 text-[var(--color-muted)] hover:text-[var(--color-text)] disabled:opacity-50"
                  aria-label={tr("videos_download", undefined, "Download")}
                >
                  {rowBusy ? (
                    <Loader2 size={13} className="animate-spin" />
                  ) : (
                    <Download size={13} />
                  )}
                </button>
              </li>
            );
          })}
        </ul>
      </div>

      {preview && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/80 p-6"
          onClick={closePreview}
        >
          <div
            className="relative max-h-full max-w-3xl overflow-hidden rounded-lg bg-[var(--color-surface)]"
            onClick={(e) => e.stopPropagation()}
          >
            <button
              type="button"
              onClick={closePreview}
              className="absolute right-2 top-2 z-10 rounded-full bg-black/50 p-1.5 text-white hover:bg-black/70"
              aria-label={tr("close", undefined, "Close")}
            >
              <X size={16} />
            </button>
            <div className="flex min-h-[240px] min-w-[320px] items-center justify-center p-2">
              {preview.loading && (
                <div className="flex items-center gap-2 text-sm text-[var(--color-muted)]">
                  <Loader2 size={16} className="animate-spin" />
                  {tr("videos_preview_loading", undefined, "Downloading clip…")}
                </div>
              )}
              {preview.error && (
                <div className="max-w-sm text-center text-sm text-[var(--color-danger)]">
                  {preview.error}
                </div>
              )}
              {preview.url && !preview.loading && (
                <video
                  src={preview.url}
                  controls
                  autoPlay
                  className="max-h-[70vh] max-w-full rounded"
                />
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
