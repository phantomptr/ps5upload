import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Image as ImageIcon,
  RefreshCw,
  Loader2,
  Download,
  FileImage,
  CheckSquare,
  Square,
  Eye,
  X,
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
import {
  pngNameForJxr,
  joinDir,
  isJxrScreenshot,
} from "../../lib/screenshotConvert";
import { isMobile } from "../../lib/platform";
import { isTauriEnv } from "../../lib/tauriEnv";

/** Map a full-res shot path to its (smaller) PS5 thumbnail path. The console
 *  stores `/user/av_contents/photo/.../X.jxr` and a thumbnail at
 *  `/user/av_contents/thumbnails/photo/.../X.jxr.jxr` (see payload runtime.c).
 *  Decoding the thumbnail is far cheaper than the ~1 MiB full-res. Returns null
 *  when the path isn't a recognised capture path (then we fall back to it). */
function thumbnailPathFor(fullPath: string): string | null {
  const marker = "/av_contents/photo/";
  if (!fullPath.includes(marker)) return null;
  return fullPath.replace(marker, "/av_contents/thumbnails/photo/") + ".jxr";
}

/** Shared, per-mount thumbnail cache + scratch dir, threaded into each row's
 *  <ScreenshotThumb> so a decoded thumbnail survives re-renders and re-scrolls
 *  (decoded once) and the scratch dir is cleaned on unmount. */
interface ThumbCache {
  urls: Map<string, string>;
  inflight: Map<string, Promise<string | null>>;
  dir: { current: string | null };
}

/** Download a shot's (thumbnail, else full-res) `.jxr` and decode it to a
 *  WebView-renderable PNG — the same pipeline the Preview lightbox uses, just
 *  cached for inline thumbnails. SDR `.jpg/.png` shots are shown as-is. */
async function buildThumb(
  item: ScreenshotEntry,
  host: string,
  cache: ThumbCache,
): Promise<string | null> {
  if (cache.urls.has(item.path)) return cache.urls.get(item.path) ?? null;
  const existing = cache.inflight.get(item.path);
  if (existing) return existing;
  const run = (async (): Promise<string | null> => {
    if (!cache.dir.current) cache.dir.current = await saveArchiveMakeTemp("ss-thumb");
    const dir = cache.dir.current;
    const fetchDecode = async (srcPath: string): Promise<string> => {
      const name = basename(srcPath);
      const jobId = await startTransferDownload(
        srcPath,
        dir,
        `${host.trim()}:${PS5_PAYLOAD_PORT}`,
        "file",
      );
      await waitForJob(jobId);
      const local = joinDir(dir, name);
      if (isJxrScreenshot(name)) {
        const png = joinDir(dir, pngNameForJxr(name));
        await convertScreenshot(local, png, true);
        return convertFileSrc(png);
      }
      return convertFileSrc(local);
    };
    const thumb = thumbnailPathFor(item.path);
    let url: string;
    try {
      url = thumb ? await fetchDecode(thumb) : await fetchDecode(item.path);
    } catch {
      // Thumbnail missing/odd path → fall back to the full-res shot.
      url = await fetchDecode(item.path);
    }
    cache.urls.set(item.path, url);
    return url;
  })();
  cache.inflight.set(item.path, run);
  try {
    return await run;
  } finally {
    cache.inflight.delete(item.path);
  }
}

/** Inline lazy thumbnail: a generic glyph until the row scrolls near view, then
 *  the decoded shot. Desktop-only (`enabled`, gated on the JPEG XR codec); on
 *  mobile or decode failure it stays the glyph. */
function ScreenshotThumb({
  item,
  host,
  enabled,
  cache,
}: {
  item: ScreenshotEntry;
  host: string;
  enabled: boolean;
  cache: ThumbCache;
}) {
  const [url, setUrl] = useState<string | null>(
    () => cache.urls.get(item.path) ?? null,
  );
  const [near, setNear] = useState(false);
  const boxRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (url || near || !boxRef.current) return;
    const el = boxRef.current;
    const obs = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          setNear(true);
          obs.disconnect();
        }
      },
      { rootMargin: "300px" },
    );
    obs.observe(el);
    return () => obs.disconnect();
  }, [url, near]);

  useEffect(() => {
    if (!near || url || !enabled || !host?.trim()) return;
    let cancelled = false;
    buildThumb(item, host, cache)
      .then((u) => {
        if (!cancelled) setUrl(u);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [near, url, enabled, host, item, cache]);

  return (
    <div
      ref={boxRef}
      className="grid h-10 w-10 shrink-0 place-items-center overflow-hidden rounded bg-[var(--color-surface-3)]"
    >
      {url ? (
        <img src={url} alt="" className="h-full w-full object-cover" loading="lazy" />
      ) : (
        <ImageIcon size={14} className="text-[var(--color-muted)]" />
      )}
    </div>
  );
}

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
            `${host.trim()}:${PS5_PAYLOAD_PORT}`,
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
      setError(e instanceof Error ? e.message : String(e));
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

  // Download the raw .jxr into the chosen folder, then decode + HDR→SDR
  // tone-map it to a .png alongside, removing the intermediate .jxr. The
  // .png is what most people actually want (the .jxr won't open in normal
  // viewers); anyone who needs the HDR original uses Download instead.
  async function convertOne(item: ScreenshotEntry) {
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
        `${host.trim()}:${PS5_PAYLOAD_PORT}`,
        "file",
      );
      await waitForJob(jobId);
      // Only .jxr needs the JPEG-XR decode + HDR→SDR tone-map
      // (deleteSource=true leaves only the viewable .png behind). An SDR
      // .jpg/.png download is already viewable as-is, so leave it.
      if (isJxrScreenshot(name)) {
        await convertScreenshot(localJxr, localPng, true);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
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
  // Lock background scroll while the (inline) screenshot preview is open.
  useScrollLock(!!preview);

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
        `${host.trim()}:${PS5_PAYLOAD_PORT}`,
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

  // Lazy inline-thumbnail cache: decoded URLs + a scratch dir, created once and
  // stable across renders so a thumbnail decodes once. Held in state (not a ref)
  // so passing it to rows isn't a render-time ref read. Scratch dir cleaned on
  // unmount.
  const [thumbCache] = useState<ThumbCache>(() => ({
    urls: new Map(),
    inflight: new Map(),
    dir: { current: null },
  }));
  useEffect(
    () => () => {
      const d = thumbCache.dir.current;
      if (d) void saveArchiveCleanupTemp(d).catch(() => {});
    },
    [thumbCache],
  );

  // Esc closes the lightbox.
  useEffect(() => {
    if (!preview) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closePreview();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [preview]);

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
            {selected.size > 0 && isTauriEnv() && (
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
          fill
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
                <ScreenshotThumb
                  item={item}
                  host={host ?? ""}
                  enabled={canConvert}
                  cache={thumbCache}
                />
                <div className="min-w-0 flex-1">
                  <code className="block truncate text-xs">
                    {item.path.split("/").pop()}
                  </code>
                  <div className="text-xs text-[var(--color-muted)]">
                    {formatBytes(item.size)} ·{" "}
                    {new Date(item.mtime * 1000).toLocaleString()}
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
                        <Loader2 size={11} className="animate-spin" />
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
                {isTauriEnv() && (
                  <Button
                    variant="ghost"
                    size="sm"
                    leftIcon={
                      busyPaths.has(item.path) ? (
                        <Loader2 size={11} className="animate-spin" />
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
                )}
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

      {/* Preview lightbox — click the backdrop or press Esc to close. */}
      {preview && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-[var(--overlay-scrim)] p-6"
          onClick={closePreview}
          role="dialog"
          aria-modal="true"
        >
          <div
            className="relative flex max-h-full max-w-full flex-col items-center gap-3"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex w-full items-center justify-between gap-3 text-sm text-white">
              <span className="min-w-0 truncate font-mono text-xs">
                {preview.item.path.split("/").pop()}
              </span>
              <div className="flex shrink-0 items-center gap-2">
                {isTauriEnv() && (
                <Button
                  variant="secondary"
                  size="sm"
                  leftIcon={
                    busyPaths.has(preview.item.path) ? (
                      <Loader2 size={12} className="animate-spin" />
                    ) : (
                      <Download size={12} />
                    )
                  }
                  disabled={busyPaths.has(preview.item.path)}
                  onClick={() => void downloadOne(preview.item)}
                >
                  {tr("screenshots_download", undefined, "Download")}
                </Button>
                )}
                <button
                  type="button"
                  onClick={closePreview}
                  aria-label={tr("screenshots_preview_close", undefined, "Close")}
                  className="rounded p-1 text-white/80 hover:text-white"
                >
                  <X size={18} />
                </button>
              </div>
            </div>
            <div className="flex min-h-[40vh] min-w-[40vw] items-center justify-center overflow-hidden rounded-md bg-[var(--color-surface-2)]">
              {preview.loading ? (
                <div className="flex flex-col items-center gap-2 p-10 text-xs text-[var(--color-muted)]">
                  <Loader2 size={20} className="animate-spin" />
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
                  className="max-h-[80vh] max-w-[88vw] object-contain"
                />
              ) : null}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

// formatBytes moved to lib/format.ts (and corrected to IEC binary
// units — the prior local copy mislabelled 1024-step values as KB/MB).
