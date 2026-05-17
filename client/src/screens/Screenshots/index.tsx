import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Image as ImageIcon,
  RefreshCw,
  Loader2,
  Download,
  CheckSquare,
  Square,
} from "lucide-react";
import {
  waitForJob,
  screenshotsList,
  startTransferDownload,
  type ScreenshotEntry,
} from "../../api/ps5";
import { useConnectionStore, PS5_PAYLOAD_PORT } from "../../state/connection";
import { mgmtAddr } from "../../lib/addr";
import { useStaleHostGuard } from "../../lib/staleHostGuard";
import { PageHeader, Button, EmptyState, ErrorCard } from "../../components";
import { useTr } from "../../state/lang";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { formatBytes } from "../../lib/format";

/**
 * Screenshot manager.
 *
 * Lists every .jpg under /user/av_contents/thumbnails/photo on the
 * connected PS5. No thumbnail rendering yet — just metadata + a
 * "download to local folder" path. Future iteration: pull each .jpg
 * via FS_READ for real thumbnails.
 */
export default function ScreenshotsScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const guard = useStaleHostGuard();
  const [items, setItems] = useState<ScreenshotEntry[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [bulkBusy, setBulkBusy] = useState(false);

  const allSelected = useMemo(() => {
    return !!items && items.length > 0 && selected.size === items.length;
  }, [items, selected]);

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

  async function downloadSelected() {
    if (!host?.trim() || selected.size === 0) return;
    const dest = await openDialog({
      directory: true,
      title: tr(
        "screenshots_bulk_dest",
        { n: selected.size },
        `Pick a folder for ${selected.size} screenshots`,
      ),
    });
    if (!dest || typeof dest !== "string") return;
    setBulkBusy(true);
    setError(null);
    try {
      for (const path of selected) {
        const jobId = await startTransferDownload(
          path,
          dest,
          `${host.trim()}:${PS5_PAYLOAD_PORT}`,
          "file",
        );
        await waitForJob(jobId);
      }
      setSelected(new Set());
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBulkBusy(false);
    }
  }

  const refresh = useCallback(async () => {
    if (!host?.trim() || payloadStatus !== "up") return;
    // Host-stale guard (2.12.0 migrated to canonical useStaleHostGuard).
    // screenshotsList can take seconds on a console with thousands of
    // shots; if the user switches PS5 mid-list, OLD's screenshots
    // would appear under NEW's name.
    const probe = guard.capture();
    setLoading(true);
    setError(null);
    try {
      const r = await screenshotsList(mgmtAddr(probe.host));
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

  async function downloadOne(item: ScreenshotEntry) {
    if (!host?.trim()) return;
    const dest = await openDialog({
      directory: true,
      title: tr(
        "screenshots_dest_pick",
        undefined,
        "Pick a folder to download into",
      ),
    });
    if (!dest || typeof dest !== "string") return;
    try {
      // startTransferDownload only enqueues an engine job; pre-fix
      // we stopped here and any later transfer failure (network
      // drop, permission denied, disk full) was invisible. Mirror
      // the bulk path: await waitForJob so failures surface.
      const jobId = await startTransferDownload(
        item.path,
        dest,
        `${host.trim()}:${PS5_PAYLOAD_PORT}`,
        "file",
      );
      await waitForJob(jobId);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  return (
    <div className="p-6">
      <PageHeader
        icon={ImageIcon}
        title={tr("screenshots_title", undefined, "Screenshots")}
        count={items?.length}
        loading={loading}
        description={tr(
          "screenshots_description",
          undefined,
          "Photos saved on the PS5 (Capture Gallery). Download to your computer or delete via the file system tab.",
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
                {tr(
                  "screenshots_bulk_download",
                  { n: selected.size },
                  `Download ${selected.size}`,
                )}
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
          icon={ImageIcon}
          message={tr(
            "screenshots_no_payload",
            undefined,
            "Connect to your PS5 first.",
          )}
        />
      )}

      {error && (
        <div className="mb-4">
          <ErrorCard
            title={tr("screenshots_error", undefined, "Couldn't list screenshots")}
            detail={error}
          />
        </div>
      )}

      {items && items.length === 0 && payloadStatus === "up" && (
        <EmptyState
          icon={ImageIcon}
          message={tr(
            "screenshots_empty",
            undefined,
            "No photos found — take a screenshot on the PS5 first.",
          )}
        />
      )}

      <div className="mx-auto max-w-4xl">
        {items && items.length > 0 && (
          <button
            type="button"
            onClick={toggleAll}
            className="mb-2 inline-flex items-center gap-1.5 text-[11px] text-[var(--color-muted)] hover:text-[var(--color-text)]"
          >
            {allSelected ? (
              <CheckSquare size={11} />
            ) : (
              <Square size={11} />
            )}
            {allSelected
              ? tr("screenshots_deselect_all", undefined, "Deselect all")
              : tr("screenshots_select_all", undefined, "Select all")}
          </button>
        )}
        <ul className="space-y-1">
          {items?.map((item) => {
            const isSelected = selected.has(item.path);
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
                  aria-label={tr("screenshots_select", undefined, "Toggle select")}
                >
                  {isSelected ? <CheckSquare size={12} /> : <Square size={12} />}
                </button>
                <ImageIcon size={14} className="text-[var(--color-muted)]" />
                <div className="min-w-0 flex-1">
                  <code className="block truncate text-[11px]">
                    {item.path.split("/").pop()}
                  </code>
                  <div className="text-[10px] text-[var(--color-muted)]">
                    {formatBytes(item.size)} ·{" "}
                    {new Date(item.mtime * 1000).toLocaleString()}
                  </div>
                </div>
                <Button
                  variant="ghost"
                  size="sm"
                  leftIcon={<Download size={11} />}
                  onClick={() => downloadOne(item)}
                >
                  {tr("screenshots_download", undefined, "Download")}
                </Button>
              </li>
            );
          })}
        </ul>
      </div>

      {loading && items === null && (
        <div className="mt-4 text-center text-xs text-[var(--color-muted)]">
          <Loader2 size={12} className="mr-2 inline animate-spin" />
          {tr("screenshots_loading", undefined, "Reading screenshots…")}
        </div>
      )}
    </div>
  );
}

// formatBytes moved to lib/format.ts (and corrected to IEC binary
// units — the prior local copy mislabelled 1024-step values as KB/MB).
