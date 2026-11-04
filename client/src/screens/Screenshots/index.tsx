import { consoleAddr } from "../../lib/addr";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  Image as ImageIcon,
  RefreshCw,
  Download,
  FileImage,
  CheckSquare,
  Square,
  Eye,
  Trash2,
} from "lucide-react";
import { convertFileSrc } from "@tauri-apps/api/core";
import {
  waitForJob,
  screenshotsList,
  startTransferDownload,
  convertScreenshot,
  saveArchiveMakeTemp,
  saveArchiveCleanupTemp,
  type ScreenshotEntry,
  type ScreenshotPngMode,
} from "../../api/ps5";
import { useConnectionStore } from "../../state/connection";
import { mgmtAddr } from "../../lib/addr";
import { useStaleHostGuard } from "../../lib/staleHostGuard";
import {
  Button,
  EmptyState,
  ErrorCard,
  Modal,
  Spinner,
  ConnectionGate,
} from "../../components";
import { CaptureThumb, useThumbCache } from "../Captures/CaptureThumb";
import { CaptureSortSelect } from "../Captures/CaptureSortSelect";
import { sortCaptures, type CaptureSort } from "../Captures/sortCaptures";
import { CaptureToolbar } from "../Captures/CaptureToolbar";
import { useCaptureDelete } from "../Captures/useCaptureDelete";
import { humanizePs5Error, humanizeCaught } from "../../lib/humanizeError";
import { useTr } from "../../state/lang";
import { pickPath } from "../../lib/pickPath";
import { formatBytes } from "../../lib/format";
import { basename } from "../../lib/uploadDest";
import {
  pngNameForJxr,
  rememberPngMode,
  storedPngMode,
  joinDir,
  isJxrScreenshot,
} from "../../lib/screenshotConvert";
import { shellErrorText } from "../../lib/shellError";
import { isMobile } from "../../lib/platform";
import { isTauriEnv } from "../../lib/tauriEnv";
import { formatDate } from "../../lib/formatDate";

/**
 * Screenshot manager.
 *
 * Lists each PS5 screenshot once (the payload walks the full-res
 * `/user/av_contents/photo` tree, then dedupes the matching
 * `/thumbnails/photo` entries). Shots are HDR JPEG XR (`.jxr`), which
 * normal apps can't open — so each row offers:
 *   • Download — pull the raw `.jxr` (for HDR-aware editors), and
 *   • Convert  — download + decode + HDR→SDR tone-map to a `.png`
 *     (desktop only; the JPEG XR codec isn't bundled on mobile).
 */
export default function ScreenshotsScreen({ tabs }: { tabs?: ReactNode } = {}) {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const guard = useStaleHostGuard();
  const [items, setItems] = useState<ScreenshotEntry[] | null>(null);
  const [sort, setSort] = useState<CaptureSort>("newest");
  const sortedItems = useMemo(() => sortCaptures(items ?? [], sort), [items, sort]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  // How a .jxr is written as PNG (#448), remembered per viewer.
  const [pngMode, setPngMode] = useState<ScreenshotPngMode>(() => storedPngMode());
  const choosePngMode = (m: ScreenshotPngMode) => {
    rememberPngMode(m);
    setPngMode(m);
  };
  const [bulkBusy, setBulkBusy] = useState(false);
  const {
    deleting,
    deleteSelected,
    dialog: deleteDialog,
  } = useCaptureDelete({
    host,
    kind: "screenshots",
    selected,
    setSelected,
    setItems,
    setError,
  });
  // Per-row in-flight set so a single download shows a spinner and can't be
  // double-fired (previously the row button had no busy feedback at all).
  const [busyPaths, setBusyPaths] = useState<Set<string>>(new Set());
  // Separate in-flight set for the Convert action (download + decode +
  // tone-map), so its spinner is independent of a plain Download.
  const [convertingPaths, setConvertingPaths] = useState<Set<string>>(
    new Set(),
  );
  // Convert (JPEG XR decode) is a desktop-only feature — the codec isn't
  // bundled on mobile (Android/iOS) — so the button is hidden there. Also
  // gated on isTauriEnv(): thumbnails/previews stage a local temp dir + use
  // convertFileSrc, neither of which exists in a browser session.
  const canConvert = !isMobile() && isTauriEnv();

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
    const dest = await pickPath({
      mode: "folder",
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
      // Per-item: one failed screenshot (transient network drop) must not
      // abort the rest of the batch and leave the user with no idea which
      // transferred. Continue past failures, then keep only the failed
      // paths selected so a re-run retries just those.
      const failed: string[] = [];
      let okCount = 0;
      for (const path of selected) {
        try {
          const jobId = await startTransferDownload(
            path,
            dest,
            consoleAddr(host.trim()),
            "file",
          );
          await waitForJob(jobId);
          okCount++;
        } catch (e) {
          failed.push(path);
          console.warn("[screenshots] download failed:", path, e);
        }
      }
      setSelected(new Set(failed));
      if (failed.length > 0) {
        setError(
          tr(
            "screenshots_bulk_partial",
            { ok: okCount, failed: failed.length },
            `Downloaded ${okCount}; ${failed.length} failed — failed ones stay selected to retry.`,
          ),
        );
      }
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
      setError(humanizePs5Error(e instanceof Error ? e.message : String(e)));
    } finally {
      setLoading(false);
    }
  }, [host, payloadStatus, guard]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // Clear the rendered list + selection the instant the active console changes,
  // before the new console's screenshotsList resolves. The stale guard in
  // refresh() only stops a late result from overwriting state — it does NOT
  // remove console A's already-rendered (and clickable) rows, and a selection
  // made on A would otherwise leak into B so "Download N" targets A's paths
  // against the now-current host. Mirrors Saves/Hardware/Dashboard.
  useEffect(() => {
    setItems(null);
    setSelected(new Set());
    setError(null);
  }, [host]);

  async function downloadOne(item: ScreenshotEntry) {
    if (!host?.trim()) return;
    const dest = await pickPath({
      mode: "folder",
      title: tr(
        "screenshots_dest_pick",
        undefined,
        "Pick a folder to download into",
      ),
    });
    if (!dest || typeof dest !== "string") return;
    setBusyPaths((s) => new Set(s).add(item.path));
    try {
      // startTransferDownload only enqueues an engine job; pre-fix
      // we stopped here and any later transfer failure (network
      // drop, permission denied, disk full) was invisible. Mirror
      // the bulk path: await waitForJob so failures surface.
      const jobId = await startTransferDownload(
        item.path,
        dest,
        consoleAddr(host.trim()),
        "file",
      );
      await waitForJob(jobId);
    } catch (e) {
      setError(humanizeCaught(e));
    } finally {
      setBusyPaths((s) => {
        const next = new Set(s);
        next.delete(item.path);
        return next;
      });
    }
  }

  // Download the raw .jxr into the chosen folder, then decode + HDR→SDR
  // tone-map it to a .png alongside, removing the intermediate .jxr. The
  // .png is what most people actually want (the .jxr won't open in normal
  // viewers); anyone who needs the HDR original uses Download instead.
  async function convertOne(item: ScreenshotEntry) {
    const mode = pngMode;
    if (!host?.trim()) return;
    const dest = await pickPath({
      mode: "folder",
      title: tr(
        "screenshots_convert_dest",
        undefined,
        "Pick a folder for the converted PNG",
      ),
    });
    if (!dest || typeof dest !== "string") return;
    const name = basename(item.path);
    const localJxr = joinDir(dest, name);
    const localPng = joinDir(dest, pngNameForJxr(name));
    setConvertingPaths((s) => new Set(s).add(item.path));
    setError(null);
    try {
      const jobId = await startTransferDownload(
        item.path,
        dest,
        consoleAddr(host.trim()),
        "file",
      );
      await waitForJob(jobId);
      // Only .jxr needs the JPEG-XR decode + HDR→SDR tone-map
      // (deleteSource=true leaves only the viewable .png behind). An SDR
      // .jpg/.png download is already viewable as-is, so leave it.
      if (isJxrScreenshot(name)) {
        await convertScreenshot(localJxr, localPng, true, mode);
      }
    } catch (e) {
      setError(humanizePs5Error(shellErrorText(e instanceof Error ? e.message : String(e), tr)));
    } finally {
      setConvertingPaths((s) => {
        const next = new Set(s);
        next.delete(item.path);
        return next;
      });
    }
  }

  // ─── Preview lightbox ───────────────────────────────────────────────
  // PS5 shots are HDR .jxr the WebView can't render, so a preview means:
  // download to a scratch dir → tone-map to PNG → load that PNG via
  // convertFileSrc. Desktop-only (same JPEG-XR codec gate as Convert). The
  // scratch dir is cleaned when the lightbox closes.
  const [preview, setPreview] = useState<{
    item: ScreenshotEntry;
    url: string | null;
    tempDir: string | null;
    loading: boolean;
    error: string | null;
  } | null>(null);

  async function openPreview(item: ScreenshotEntry) {
    if (!host?.trim() || !canConvert) return;
    // Replacing an open preview: clean the outgoing scratch dir so opening
    // several previews in a row doesn't leak one temp dir each.
    setPreview((prev) => {
      if (prev?.tempDir) void saveArchiveCleanupTemp(prev.tempDir).catch(() => {});
      return { item, url: null, tempDir: null, loading: true, error: null };
    });
    let tempDir: string | null = null;
    try {
      tempDir = await saveArchiveMakeTemp("ss-preview");
      const jobId = await startTransferDownload(
        item.path,
        tempDir,
        consoleAddr(host.trim()),
        "file",
      );
      await waitForJob(jobId);
      const name = basename(item.path);
      const localFile = joinDir(tempDir, name);
      // Only HDR .jxr captures need the JPEG-XR decode + tone-map. SDR
      // .jpg/.png shots are already WebView-renderable — converting them
      // would fail ("not a JPEG XR file"), which is exactly the error a
      // user with .jpg screenshots hit on Preview.
      let displayLocal = localFile;
      if (isJxrScreenshot(name)) {
        const localPng = joinDir(tempDir, pngNameForJxr(name));
        await convertScreenshot(localFile, localPng, true);
        displayLocal = localPng;
      }
      const url = convertFileSrc(displayLocal);
      // Race guard: only commit if this item is still the one showing. If the
      // user switched to a different preview (or closed) mid-convert, our
      // scratch dir is now orphaned — clean it instead of leaking it.
      const created = tempDir;
      setPreview((p) => {
        if (p && p.item.path === item.path) {
          return { ...p, url, tempDir: created, loading: false };
        }
        void saveArchiveCleanupTemp(created).catch(() => {});
        return p;
      });
    } catch (e) {
      const msg = shellErrorText(e instanceof Error ? e.message : String(e), tr);
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

  // Mirror the open preview's temp dir into a ref so the unmount cleanup
  // (navigating away with the lightbox open) can reach it without re-running.
  const previewTempRef = useRef<string | null>(null);
  useEffect(() => {
    previewTempRef.current = preview?.tempDir ?? null;
  }, [preview]);
  useEffect(
    () => () => {
      if (previewTempRef.current)
        void saveArchiveCleanupTemp(previewTempRef.current).catch(() => {});
    },
    [],
  );

  const thumbCache = useThumbCache();

  return (
    <div className="app-page">
      {/* The Captures screen puts its Screenshots / Video clips switch here. */}
      {tabs}
      <CaptureToolbar
        count={items?.length}
        loading={loading}
        description={
          isTauriEnv()
            ? tr(
                "screenshots_description_v3",
                undefined,
                "Screenshots saved on the PS5 (its Capture Gallery). Preview them, download them to this computer, or select some to delete them.",
              )
            : tr(
                "screenshots_description_browser_v2",
                undefined,
                "Screenshots saved on the PS5 (its Capture Gallery). Download them to a folder on the computer running ps5upload, or select some to delete them.",
              )
        }
        right={
          <>
            <CaptureSortSelect value={sort} onChange={setSort} />
            {isTauriEnv() && !isMobile() && (
              <label className="flex min-w-0 max-w-full items-center gap-1.5 text-xs text-[var(--color-muted)]">
                <span className="shrink-0">{tr("screenshots_png_mode", undefined, "Convert as")}</span>
                <select
                  value={pngMode}
                  onChange={(e) => choosePngMode(e.target.value as ScreenshotPngMode)}
                  className="select w-auto! min-w-0 max-w-full text-xs"
                  aria-label={tr("screenshots_png_mode", undefined, "Convert as")}
                  title={tr(
                    "screenshots_png_mode_hint_v2",
                    undefined,
                    "SDR PNG opens everywhere (the HDR capture is tone-mapped). HDR PNG keeps the capture's brightness and colour as 16-bit BT.2020 PQ with the capture's own ICC profile and a cICP tag, which HDR-aware viewers and browsers show as captured; pick the one without the cICP tag for apps that refuse it.",
                  )}
                  data-testid="screenshots-png-mode"
                >
                  <option value="sdr">{tr("screenshots_png_sdr", undefined, "SDR PNG")}</option>
                  <option value="hdr">{tr("screenshots_png_hdr", undefined, "HDR PNG (cICP)")}</option>
                  <option value="hdr_plain">{tr("screenshots_png_hdr_plain", undefined, "HDR PNG, no cICP tag")}</option>
                </select>
              </label>
            )}
            <div className="flex max-w-full flex-wrap items-center gap-2">
              {selected.size > 0 && (
                <Button
                  variant="primary"
                  size="sm"
                  leftIcon={
                    bulkBusy ? (
                      <Spinner size={12} tone="inherit" />
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
              {selected.size > 0 && (
                <Button
                  variant="danger"
                  size="sm"
                  loading={deleting}
                  leftIcon={<Trash2 size={12} />}
                  onClick={() => void deleteSelected()}
                  disabled={deleting || bulkBusy}
                >
                  {tr("captures_bulk_delete", { n: selected.size }, `Delete ${selected.size}`)}
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
          </>
        }
      />

      <ConnectionGate require="payload">
        {error && (
          <div className="mb-4">
            <ErrorCard
              title={tr("screenshots_error", undefined, "Couldn't list screenshots")}
              detail={error}
              onDismiss={() => setError(null)}
              action={
                <Button
                  size="sm"
                  leftIcon={<RefreshCw size={12} />}
                  onClick={() => void refresh()}
                  disabled={loading}
                >
                  {tr("retry", undefined, "Retry")}
                </Button>
              }
            />
          </div>
        )}

        {items && items.length === 0 && (
          <EmptyState
            icon={ImageIcon}
            message={tr(
              "screenshots_empty",
              undefined,
              "No photos found — take a screenshot on the PS5 first.",
            )}
          />
        )}

        <div>
          {items && items.length > 0 && (
            <button
              type="button"
              onClick={toggleAll}
              className="mb-2 inline-flex items-center gap-1.5 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]"
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
          <ul className="space-y-2">
            {sortedItems.map((item) => {
              const isSelected = selected.has(item.path);
              return (
                <li
                  key={item.path}
                  className={`flex flex-wrap items-center gap-x-3 gap-y-2 rounded-[var(--radius-card)] border px-3 py-2.5 text-xs shadow-[var(--edge-highlight)] transition-colors ${
                    isSelected
                      ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)]"
                      : "border-[var(--glass-edge)] bg-[var(--color-surface-raised)]"
                  }`}
                >
                  <button
                    type="button"
                    role="checkbox"
                    aria-checked={isSelected}
                    onClick={() => toggleOne(item.path)}
                    className="grid h-8 w-8 shrink-0 place-items-center rounded-full text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
                    aria-label={tr("screenshots_select", undefined, "Toggle select")}
                  >
                    {isSelected ? <CheckSquare size={14} /> : <Square size={14} />}
                  </button>
                  <CaptureThumb
                    item={item}
                    host={host ?? ""}
                    enabled={canConvert}
                    cache={thumbCache}
                  />
                  <div className="min-w-0 flex-1 basis-[10rem]">
                    <code className="block truncate text-xs">
                      {item.path.split("/").pop()}
                    </code>
                    <div className="text-xs text-[var(--color-muted)]">
                      {formatBytes(item.size)} ·{" "}
                      {formatDate(item.mtime * 1000)}
                    </div>
                  </div>
                  {canConvert && (
                    <Button
                      variant="ghost"
                      size="sm"
                      leftIcon={<Eye size={11} />}
                      onClick={() => void openPreview(item)}
                      disabled={
                        convertingPaths.has(item.path) || busyPaths.has(item.path)
                      }
                      title={tr(
                        "screenshots_preview_hint",
                        undefined,
                        "Preview this screenshot",
                      )}
                    >
                      {tr("screenshots_preview", undefined, "Preview")}
                    </Button>
                  )}
                  {canConvert && (
                    <Button
                      variant="ghost"
                      size="sm"
                      leftIcon={
                        convertingPaths.has(item.path) ? (
                          <Spinner size={12} tone="inherit" />
                        ) : (
                          <FileImage size={11} />
                        )
                      }
                      onClick={() => convertOne(item)}
                      disabled={
                        convertingPaths.has(item.path) || busyPaths.has(item.path)
                      }
                      title={tr(
                        "screenshots_convert_hint",
                        undefined,
                        "Download and convert this HDR screenshot to a viewable PNG",
                      )}
                    >
                      {tr("screenshots_convert", undefined, "Convert to PNG")}
                    </Button>
                  )}
                  {/* The browser build downloads too: the engine writes the file into a
                      folder picked on its own machine (pickPath browses it in-app). */}
                  <Button
                    variant="ghost"
                    size="sm"
                    leftIcon={
                      busyPaths.has(item.path) ? (
                        <Spinner size={12} tone="inherit" />
                      ) : (
                        <Download size={11} />
                      )
                    }
                    onClick={() => downloadOne(item)}
                    disabled={
                      busyPaths.has(item.path) || convertingPaths.has(item.path)
                    }
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
            <Spinner size={12} className="me-2 inline" />
            {tr("screenshots_loading", undefined, "Reading screenshots…")}
          </div>
        )}

        {/* Preview: the shared modal (Esc or the backdrop closes it). */}
        {preview && (
          <Modal
            open
            onClose={closePreview}
            title={
              <span className="truncate font-mono text-sm">
                {preview.item.path.split("/").pop()}
              </span>
            }
            size="full"
            panelClassName="sm:max-w-[min(92vw,80rem)]"
            bodyClassName="p-4"
            footer={
              isTauriEnv() ? (
                <Button
                  variant="secondary"
                  size="sm"
                  loading={busyPaths.has(preview.item.path)}
                  leftIcon={<Download size={12} />}
                  onClick={() => void downloadOne(preview.item)}
                >
                  {tr("screenshots_download", undefined, "Download")}
                </Button>
              ) : undefined
            }
          >
            <div className="flex min-h-[40vh] items-center justify-center overflow-hidden rounded-[var(--radius-card)] bg-[var(--color-surface-3)]">
              {preview.loading ? (
                <div className="flex flex-col items-center gap-2 p-10 text-xs text-[var(--color-muted)]">
                  <Spinner size={20} />
                  {tr(
                    "screenshots_preview_loading",
                    undefined,
                    "Downloading + converting (HDR screenshots take a few seconds)…",
                  )}
                </div>
              ) : preview.error ? (
                <div className="max-w-md p-8 text-center text-xs text-[var(--color-bad)]">
                  {preview.error}
                </div>
              ) : preview.url ? (
                <img
                  src={preview.url}
                  alt=""
                  className="max-h-[72vh] max-w-full object-contain"
                />
              ) : null}
            </div>
          </Modal>
        )}
        {deleteDialog}
      </ConnectionGate>
    </div>
  );
}

// formatBytes moved to lib/format.ts (and corrected to IEC binary
// units — the prior local copy mislabelled 1024-step values as KB/MB).
