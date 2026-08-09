import { useEffect, useMemo, useRef, useState } from "react";
import { useLocation, useNavigate } from "react-router";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  PackageOpen,
  Plus,
  Trash2,
  Download,
  RotateCcw,
  CheckCircle2,
  XCircle,
  AlertTriangle,
  Info,
  HardDrive,
  FolderOpen,
  Copy,
  FileText,
  Clock3,
  MapPin,
} from "lucide-react";

import { isAndroid } from "../../lib/platform";
import { pickPath } from "../../lib/pickPath";
import { isTauriEnv, safeUnlisten } from "../../lib/tauriEnv";
import {
  PageHeader,
  Button,
  EmptyState,
  WarningCard,
  ConsoleChip,
  GameIcon,
  OverflowMenu,
  PlatformBadge,
  Spinner,
  Badge,
  Checkbox,
  type OverflowMenuItem,
} from "../../components";
import { openInFileSystem } from "../../state/fsNavigation";
import { useConfirm } from "../../components/ConfirmDialog";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";
import {
  usePkgLibrary,
  isFinishedPkg,
  pkgRowInstalled,
  pkgInstallAllPlan,
  pkgAlternativeGroups,
  pkgEntryIdentity,
  loadPkgAlternativeSelections,
  recordPkgAlternativeSelection,
  type PkgEntry,
  type PkgAlternativeSelections,
} from "../../state/pkgLibrary";
import { useInstallSettingsStore } from "../../state/installSettings";
import { pkgCategoryLabel, isAddonCategory } from "../../lib/pkgStagingPath";
import {
  appsInstalled,
  pkgInstalledInventory,
  pkgScanExternal,
  pkgMetadataConsole,
  type ExternalPkg,
  type PkgConsoleMetadata,
  type InstalledPkgArtifact,
} from "../../api/ps5";
import { transferAddr, hostOf } from "../../lib/addr";
import { firmwareMajor } from "../../lib/ps5Firmware";
import { formatBytes } from "../../lib/format";
import { acceptPkgDrop } from "../../lib/pkgDropDedupe";

/* ─── Cover art ────────────────────────────────────────────────────────
 * Thin wrapper over the shared GameIcon (keyed by title id from the
 * ContentID), kept so the row markup reads `<Cover .../>`. Glyph fallback on
 * 404 — common for homebrew with no icon, or not-yet-installed pkgs. */
function Cover({ host, titleId }: { host: string; titleId?: string }) {
  return <GameIcon host={host} titleId={titleId} size={56} />;
}

/* ─── One package row ─────────────────────────────────────────────────── */
function PkgRow({
  entry,
  host,
  installed,
  installDisabled,
  deleteDisabled,
  alternativeKey,
  selectedForInstallAll,
  onSelectAlternative,
  onInstall,
  onDelete,
}: {
  entry: PkgEntry;
  host: string;
  installed: boolean;
  installDisabled: boolean;
  deleteDisabled: boolean;
  alternativeKey?: string;
  selectedForInstallAll?: boolean;
  onSelectAlternative?: () => void;
  onInstall: () => void;
  onDelete: () => void;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const uploading = entry.status === "uploading";
  const installingThis = entry.status === "installing";
  const queued = entry.status === "queued";
  const busy = uploading || installingThis || queued;
  const categoryLabel = pkgCategoryLabel(entry.category);
  const legacyVariantSuffix =
    alternativeKey && entry.fingerprint
      ? ` · ${entry.fingerprint.slice(0, 8)}`
      : "";
  const rowLabel = isAddonCategory(entry.category)
    ? entry.originalName ||
      `${categoryLabel || "Add-on"}${entry.appVer ? ` v${entry.appVer}` : ""}${legacyVariantSuffix}`
    : entry.title || entry.originalName || entry.contentId || entry.name;
  // Right-click/⋯ context actions for the package (Open Folder, Copy Details).
  const dir = entry.path.slice(0, entry.path.lastIndexOf("/")) || "/";
  const menuItems: OverflowMenuItem[] = [
    {
      label: tr("pkglib.menu.openFolder", "Open folder"),
      icon: <FolderOpen size={12} />,
      onSelect: () => openInFileSystem(navigate, dir),
    },
    {
      label: tr("pkglib.menu.copyDetails", "Copy details"),
      icon: <Copy size={12} />,
      onSelect: () =>
        void navigator.clipboard?.writeText(
          [
            entry.title,
            entry.appVer ? `v${entry.appVer}` : "",
            entry.originalName,
            entry.sourcePath,
            entry.uploadedAt
              ? new Date(entry.uploadedAt).toLocaleString()
              : undefined,
            entry.contentId,
            entry.titleId,
            entry.path,
          ]
            .filter(Boolean)
            .join("\n"),
        ),
    },
  ];
  const pct =
    uploading && entry.totalBytes
      ? Math.min(100, Math.round(((entry.bytes ?? 0) / entry.totalBytes) * 100))
      : 0;

  return (
    <li className="flex flex-col gap-2 rounded-xl border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center">
        <div className="flex min-w-0 items-center gap-3 sm:flex-1">
          <Cover host={host} titleId={entry.titleId} />
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-2">
            <span
              className="min-w-0 basis-full truncate text-sm font-medium sm:basis-auto sm:flex-1"
              title={rowLabel}
            >
              {rowLabel}
            </span>
            {/* PS4 / PS5 platform badge — derived from the header magic
                (\x7FFIH = PS5) and the title-id prefix (CUSA = PS4, PPSA =
                PS5). Helps users tell at a glance which console a pkg targets. */}
            <PlatformBadge platform={entry.platform} />
            {installed && !busy && (
              <Badge tone="good" variant="soft">
                {tr("pkglib.badge.installed", "installed")}
              </Badge>
            )}
            {/* Update / DLC badge — a base game and its update share a
                ContentID, so without this they look identical. */}
            {pkgCategoryLabel(entry.category) &&
              pkgCategoryLabel(entry.category) !== "Base" && (
                <Badge tone="accent" variant="soft">
                  {pkgCategoryLabel(entry.category) === "Update"
                    ? tr("pkglib.badge.update", "update")
                    : tr("pkglib.badge.dlc", "DLC")}
                </Badge>
              )}
            {/* Authoritative PARAM.SFO version — the definitive "which update
                is this" (updates share a ContentID and a title). */}
            {entry.appVer && (
              <span
                className="inline-flex shrink-0 items-center rounded-full border border-[var(--color-border)] px-1.5 py-0.5 font-mono text-xs font-medium tabular-nums text-[var(--color-muted)]"
                title={tr(
                  "pkglib.version.title",
                  "Package version (PARAM.SFO APP_VER)",
                )}
              >
                v{entry.appVer}
              </span>
            )}
            </div>
            <div className="mt-0.5 truncate font-mono text-xs text-[var(--color-muted)]">
              {entry.contentId || entry.name}
              <span className="px-1 opacity-60">·</span>
              <span className="tabular-nums">{formatBytes(entry.size)}</span>
            </div>
            {/* Upload provenance. Computer-side details remain in this app's
                local cache; older/other-computer rows still show the PS5 path
                and use the staged file mtime as their best-known upload time. */}
            <div className="mt-1.5 grid min-w-0 gap-x-4 gap-y-1 text-[11px] text-[var(--color-muted)] sm:grid-cols-2">
              {entry.originalName && (
                <div
                  className="flex min-w-0 items-center gap-1"
                  title={entry.originalName}
                >
                  <FileText size={11} className="shrink-0 opacity-70" />
                  <span className="shrink-0 font-medium">
                    {tr("pkglib.meta.file", undefined, "File:")}
                  </span>
                  <span className="truncate">{entry.originalName}</span>
                </div>
              )}
              {entry.sourcePath && (
                <div
                  className="flex min-w-0 items-center gap-1"
                  title={entry.sourcePath}
                >
                  <MapPin size={11} className="shrink-0 opacity-70" />
                  <span className="shrink-0 font-medium">
                    {tr("pkglib.meta.source", undefined, "Source:")}
                  </span>
                  <span className="truncate font-mono">{entry.sourcePath}</span>
                </div>
              )}
              {entry.uploadedAt && (
                <div
                  className="flex min-w-0 items-center gap-1"
                  title={new Date(entry.uploadedAt).toISOString()}
                >
                  <Clock3 size={11} className="shrink-0 opacity-70" />
                  <span className="shrink-0 font-medium">
                    {tr("pkglib.meta.uploaded", undefined, "Uploaded:")}
                  </span>
                  <span className="truncate tabular-nums">
                    {new Date(entry.uploadedAt).toLocaleString()}
                  </span>
                </div>
              )}
              <div className="flex min-w-0 items-center gap-1" title={entry.path}>
                <FolderOpen size={11} className="shrink-0 opacity-70" />
                <span className="shrink-0 font-medium">
                  {tr("pkglib.meta.onPs5", undefined, "On PS5:")}
                </span>
                <span className="truncate font-mono">{entry.path}</span>
              </div>
            </div>
          </div>
        </div>

        {/* Actions */}
        {!busy && (
          <div className="flex shrink-0 flex-wrap items-center justify-end gap-1.5">
            <Button
              variant={installed ? "secondary" : "primary"}
              size="sm"
              leftIcon={
                installed ? <RotateCcw size={13} /> : <Download size={13} />
              }
              onClick={onInstall}
              disabled={installDisabled}
              title={
                installDisabled
                  ? tr(
                      "pkglib.install.busyHint",
                      "Installing replaces the PS5 payload, which would interrupt an active upload. Wait for the current upload (or install) to finish first.",
                    )
                  : undefined
              }
            >
              {installed
                ? tr("pkglib.reinstall", "Reinstall")
                : tr("pkglib.install", "Install")}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              leftIcon={<Trash2 size={13} />}
              onClick={onDelete}
              disabled={deleteDisabled}
              title={tr("pkglib.delete", "Delete")}
            >
              {tr("pkglib.delete", "Delete")}
            </Button>
            <OverflowMenu items={menuItems} />
          </div>
        )}
        {installingThis && (
          <div className="flex shrink-0 items-center gap-2 text-xs text-[var(--color-accent)]">
            <Spinner size={14} tone="inherit" />
            {tr("pkglib.installing", "Installing…")}
          </div>
        )}
        {queued && (
          <div className="flex shrink-0 items-center gap-2 text-xs text-[var(--color-muted)]">
            <Spinner size={14} />
            {tr(
              "pkglib.queued",
              undefined,
              "Queued — waiting for the current transfer",
            )}
          </div>
        )}
      </div>

      {alternativeKey && onSelectAlternative && !busy && (
        <label className="flex cursor-pointer items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-1)] px-2.5 py-2 text-xs">
          <input
            type="radio"
            name={`pkg-variant-${alternativeKey}`}
            checked={!!selectedForInstallAll}
            onChange={onSelectAlternative}
            disabled={installDisabled}
            className="accent-[var(--color-accent)]"
          />
          <span
            className={
              selectedForInstallAll
                ? "font-medium text-[var(--color-accent)]"
                : "text-[var(--color-muted)]"
            }
          >
            {selectedForInstallAll
              ? tr(
                  "pkglib.variant.selected",
                  undefined,
                  "Selected for Install all on this PS5",
                )
              : tr(
                  "pkglib.variant.select",
                  undefined,
                  "Use this variant for Install all on this PS5",
                )}
          </span>
        </label>
      )}

      {/* Upload progress */}
      {uploading && (
        <div className="flex flex-col gap-1">
          <div className="h-1.5 overflow-hidden rounded-full bg-[var(--color-surface-3)]">
            <div
              className="h-full rounded-full bg-[var(--color-accent)] transition-[width] duration-300"
              style={{ width: `${pct}%` }}
            />
          </div>
          <div className="flex items-center justify-between text-xs text-[var(--color-muted)]">
            <span>{tr("pkglib.uploading", "Uploading to PS5…")}</span>
            <span className="tabular-nums">
              {formatBytes(entry.bytes ?? 0)}
              {entry.totalBytes
                ? ` / ${formatBytes(entry.totalBytes)}`
                : ""} · {pct}%
            </span>
          </div>
        </div>
      )}

      {/* Amber covers both a may-not-launch success and an accepted request
          whose asynchronous completion could not be verified. */}
      {!busy && entry.lastResult && (
        <div
          className={`flex items-start gap-1.5 text-xs ${
            entry.lastResult.warn
              ? "text-[var(--color-warn)]"
              : entry.lastResult.ok
                ? "text-[var(--color-good)]"
                : "text-[var(--color-bad)]"
          }`}
        >
          {entry.lastResult.warn ? (
            <AlertTriangle size={13} className="mt-px shrink-0" />
          ) : entry.lastResult.ok ? (
            <CheckCircle2 size={13} className="mt-px shrink-0" />
          ) : (
            <XCircle size={13} className="mt-px shrink-0" />
          )}
          <span>{entry.lastResult.message}</span>
        </div>
      )}
    </li>
  );
}

/** Last-known installed-title set per console (port-stripped host). Seeds the
 *  Install/Reinstall labels on (re)mount so the screen doesn't flash "Install"
 *  on every row for the split second before the async `appsInstalled` fetch
 *  returns. Best-effort cache; refreshed on every fetch. */
const installedIdsCache = new Map<string, Set<string>>();

/* ─── Screen ──────────────────────────────────────────────────────────── */
export default function InstallPackageScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  // This console's runtime (kernel string → firmware major for the Stream
  // FW-11 guard). Scoped to the active host like every other selector.
  const runtime = useConnectionStore((s) => s.runtimeByHost[hostOf(host)]);
  // Per-console store: every selector is scoped to THIS console's host, so the
  // Install Package view is fully isolated per PS5 (parallel installs).
  const entries = usePkgLibrary(host, (s) => s.entries);
  const loading = usePkgLibrary(host, (s) => s.loading);
  const error = usePkgLibrary(host, (s) => s.error);
  const installing = usePkgLibrary(host, (s) => s.installing);
  const installingAll = usePkgLibrary(host, (s) => s.installingAll);
  const busyNotice = usePkgLibrary(host, (s) => s.busyNotice);
  const installPending = usePkgLibrary(host, (s) => s.installPending);
  const refresh = usePkgLibrary(host, (s) => s.refresh);
  const addAndUpload = usePkgLibrary(host, (s) => s.addAndUpload);
  const install = usePkgLibrary(host, (s) => s.install);
  const installAll = usePkgLibrary(host, (s) => s.installAll);
  const installStream = usePkgLibrary(host, (s) => s.installStream);
  const cancelPendingInstall = usePkgLibrary(
    host,
    (s) => s.cancelPendingInstall,
  );
  const remove = usePkgLibrary(host, (s) => s.remove);
  const clearFinished = usePkgLibrary(host, (s) => s.clearFinished);
  const clearAll = usePkgLibrary(host, (s) => s.clearAll);
  const autoRemove = useInstallSettingsStore((s) => s.autoRemoveAfterInstall);
  const setAutoRemove = useInstallSettingsStore(
    (s) => s.setAutoRemoveAfterInstall,
  );
  const autoInstall = useInstallSettingsStore((s) => s.autoInstallAfterUpload);
  const setAutoInstall = useInstallSettingsStore(
    (s) => s.setAutoInstallAfterUpload,
  );
  const { confirm, dialog } = useConfirm();

  const [installedIds, setInstalledIds] = useState<Set<string>>(
    () => installedIdsCache.get(hostOf(host)) ?? new Set(),
  );
  const [installedArtifacts, setInstalledArtifacts] = useState<
    Map<string, InstalledPkgArtifact[]>
  >(() => new Map());
  const [pickError, setPickError] = useState<string | null>(null);
  const [picking, setPicking] = useState(false);
  const [streaming, setStreaming] = useState(false);
  const [dropActive, setDropActive] = useState(false);
  const [alternativeSelections, setAlternativeSelections] =
    useState<PkgAlternativeSelections>(() =>
      loadPkgAlternativeSelections(host),
    );

  const hostReady = !!host?.trim();
  // There is no reliable firmware cutoff for Stream install. Hardware proves
  // it works on one FW 5.10 console while a FW 9.60 console can reject the
  // same request at Sony's HTTP proxy layer before fetching a byte. Keep it a
  // clearly-labelled beta and surface the real network error instead of
  // disabling capable older consoles based on version alone.
  const fwMajor = firmwareMajor(runtime?.ps5Kernel);
  useEffect(() => {
    setAlternativeSelections(loadPkgAlternativeSelections(host));
  }, [host]);
  // Stable ref so the drag-drop effect (subscribed once per host) always
  // calls the latest uploader without re-subscribing each render. Updated
  // in an effect (not during render) per the rules-of-hooks ref rule.
  const uploadRef = useRef<(p: string) => void>(() => {});
  const recentPkgDrops = useRef(new Map<string, number>());
  useEffect(() => {
    uploadRef.current = (p: string) => {
      if (hostReady && acceptPkgDrop(recentPkgDrops.current, p)) {
        void addAndUpload(p, host);
      }
    };
  });

  // Initial load + whenever the host changes.
  useEffect(() => {
    if (hostReady) void refresh(host);
  }, [host, hostReady, refresh]);

  // Installed-title set for the "installed" badge — refetched whenever the
  // library set changes (after an install/upload) so badges stay accurate.
  useEffect(() => {
    if (!hostReady) return;
    let cancelled = false;
    appsInstalled(transferAddr(host))
      .then((res) => {
        const ids = new Set(res.titles.map((t) => t.titleId));
        installedIdsCache.set(hostOf(host), ids);
        if (!cancelled) setInstalledIds(ids);
      })
      .catch(() => {
        /* badge is best-effort */
      });
    return () => {
      cancelled = true;
    };
  }, [host, hostReady, entries.length, installing]);

  const stagedTitleIds = useMemo(
    () =>
      [
        ...new Set(entries.map((e) => e.titleId).filter(Boolean) as string[]),
      ].sort(),
    [entries],
  );
  const stagedTitleIdsKey = stagedTitleIds.join(",");

  // Title-level app_list cannot prove a patch or DLC. Read the actual
  // category-specific package artifacts (app.pkg / patch.pkg / addcont) and
  // compare their sampled fingerprints to each staged row. Re-run after an
  // install settles so only the variant that really landed reads Reinstall.
  useEffect(() => {
    if (!hostReady || installing) return;
    let cancelled = false;
    // Never carry an exact artifact verdict across a console switch while the
    // new console's inventory request is still in flight.
    setInstalledArtifacts(new Map());
    const ids = stagedTitleIdsKey ? stagedTitleIdsKey.split(",") : [];
    void Promise.all(
      ids.map(async (titleId) => {
        try {
          return [
            titleId,
            await pkgInstalledInventory(transferAddr(host), titleId),
          ] as const;
        } catch {
          return [titleId, undefined] as const;
        }
      }),
    ).then((rows) => {
      if (cancelled) return;
      const next = new Map<string, InstalledPkgArtifact[]>();
      for (const [titleId, artifacts] of rows) {
        // Undefined means the live query failed; omit that key so the legacy
        // installedHere hint can remain visible instead of asserting absence.
        if (artifacts) next.set(titleId, artifacts);
      }
      setInstalledArtifacts(next);
    });
    return () => {
      cancelled = true;
    };
  }, [host, hostReady, installing, stagedTitleIdsKey]);

  // App-wide drag-drop hand-off: AppShell routes a dropped .pkg here via
  // location state.
  const location = useLocation();
  useEffect(() => {
    const dropped = (location.state as { droppedPath?: string } | null)
      ?.droppedPath;
    if (dropped) {
      uploadRef.current(dropped);
      window.history.replaceState({}, "");
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [location.key]);

  // Webview drag-drop — filter to .pkg, reject .ffpkg/.ffpfs with a clear msg.
  useEffect(() => {
    if (!host) return;
    if (!isTauriEnv()) return;
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    const p = getCurrentWebview().onDragDropEvent((e) => {
      if (cancelled) return;
      if (e.payload.type === "enter" || e.payload.type === "over") {
        setDropActive(true);
      } else if (e.payload.type === "leave") {
        setDropActive(false);
      } else if (e.payload.type === "drop") {
        setDropActive(false);
        const paths = e.payload.paths ?? [];
        const pkgPaths = paths.filter((x) => /\.pkg$/i.test(x));
        if (paths.length > 0 && pkgPaths.length === 0) {
          setPickError(
            tr(
              "install.error.notPkg",
              "Only .pkg files can be installed. .ffpkg / .ffpfs are mountable images — open them from the File System tab instead.",
            ),
          );
          return;
        }
        setPickError(null);
        for (const x of pkgPaths) uploadRef.current(x);
      }
    });
    p.then((fn) => {
      if (cancelled) safeUnlisten(fn);
      else unlisten = fn;
    }).catch(() => {});
    return () => {
      cancelled = true;
      if (unlisten) safeUnlisten(unlisten);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host]);

  async function handlePick() {
    setPickError(null);
    if (!hostReady) {
      setPickError(
        tr(
          "install.error.noHost",
          "Set a PS5 host on the Connection tab first.",
        ),
      );
      return;
    }
    setPicking(true);
    try {
      const sel = isAndroid()
        ? await pickPath({
            mode: "file",
            filters: [{ name: "PS5 Package", extensions: ["pkg"] }],
          })
        : await openDialog({
            multiple: true,
            filters: [{ name: "PS5 Package", extensions: ["pkg"] }],
          });
      const paths = Array.isArray(sel) ? sel : sel ? [sel] : [];
      for (const pth of paths) uploadRef.current(pth as string);
    } catch (e) {
      setPickError(`${e}`);
    } finally {
      setPicking(false);
    }
  }

  // Stream-install (beta, #81): pick a single PC-side .pkg and install it
  // WITHOUT staging it on the PS5 first — the engine serves the file over
  // HTTP and the DPI daemon pulls it directly. Useful for a quick one-shot
  // install when you don't want to wait out the staging upload (or don't
  // have the disk space for it). Shares the `installing` lock with the
  // regular install flow.
  async function handleStreamPick() {
    setPickError(null);
    if (!host?.trim()) {
      setPickError(
        tr(
          "install.error.noHost",
          "Set a PS5 host on the Connection tab first.",
        ),
      );
      return;
    }
    const firmwareLabel =
      fwMajor === null ? "this firmware" : `firmware ${fwMajor}.x`;
    const proceed = await confirm({
      title: tr(
        "pkglib.stream.beta.title",
        undefined,
        "Stream install is beta",
      ),
      message: tr(
        "pkglib.stream.beta.body",
        { fw: firmwareLabel },
        `On ${firmwareLabel}, Stream uses the PS5's own HTTP installer to pull the package from this computer. Firmware version alone does not determine support, and console proxy/network settings can block it. If it fails, use Upload → Install instead. Continue with Stream?`,
      ),
      confirmLabel: tr(
        "pkglib.stream.beta.confirm",
        undefined,
        "Stream anyway",
      ),
      cancelLabel: tr("cancel", undefined, "Cancel"),
      destructive: true,
    });
    if (!proceed) return;
    setStreaming(true);
    try {
      const sel = isAndroid()
        ? await pickPath({
            mode: "file",
            filters: [{ name: "PS5 Package", extensions: ["pkg"] }],
          })
        : await openDialog({
            multiple: false,
            filters: [{ name: "PS5 Package", extensions: ["pkg"] }],
          });
      const p = Array.isArray(sel) ? sel[0] : sel;
      if (!p) return;
      const r = await installStream(p as string, host);
      if (!r.ok && r.stagedFallbackRecommended) {
        const fallback = await confirm({
          title: tr(
            "pkglib.stream.fallback.title",
            undefined,
            "Stream is blocked on this PS5",
          ),
          message: `${r.message || "The PS5 couldn't complete the HTTP install."}\n\nUpload the same package to PS5 staging and install it now? This uses the PS5-local file path and does not depend on Sony's HTTP proxy.`,
          confirmLabel: tr(
            "pkglib.stream.fallback.confirm",
            undefined,
            "Upload & install",
          ),
          cancelLabel: tr(
            "pkglib.stream.fallback.cancel",
            undefined,
            "Not now",
          ),
        });
        if (fallback) {
          setPickError(null);
          await addAndUpload(p as string, host, {
            installAfterUpload: true,
            selectVariant: true,
          });
          // addAndUpload records an explicit variant choice when needed.
          setAlternativeSelections(loadPkgAlternativeSelections(host));
        } else if (r.message) {
          setPickError(r.message);
        }
      } else if (!r.ok && r.message) {
        setPickError(r.message);
      }
    } catch (e) {
      setPickError(`${e}`);
    } finally {
      setStreaming(false);
    }
  }

  // Install-order guard. A base game (CATEGORY "gd") and its update ("gp") or
  // DLC ("ac") share a ContentID AND a title_id — they never overwrite each
  // other (the library stages them to separate sub-dirs; on the PS5 the base
  // lands in /user/app/<id>/app.pkg and the update in /user/patch/<id>/
  // patch.pkg). The one thing that goes wrong is ORDER: a patch/DLC needs its
  // base installed FIRST. Install it without the base and Sony's installer
  // accepts the request (err_code 0) but nothing actually lands — a confusing
  // "it said done but the game's missing". So before installing an add-on
  // whose base isn't on the console, warn (advisory, not blocking).
  async function handleInstall(entry: PkgEntry) {
    const baseMissing =
      isAddonCategory(entry.category) &&
      !!entry.titleId &&
      !installedIds.has(entry.titleId);
    if (baseMissing) {
      const kind =
        entry.category === "ac"
          ? tr("pkglib.addon.dlc", undefined, "DLC")
          : tr("pkglib.addon.update", undefined, "update");
      // Is the base game (a "gd"/root-level entry with the same title_id)
      // already staged in the library? If so, point the user at it.
      const baseInLib = entries.some(
        (x) =>
          x.path !== entry.path &&
          x.titleId === entry.titleId &&
          (x.category === "gd" || x.category === undefined),
      );
      const ok = await confirm({
        title: tr(
          "pkglib.baseMissing.title",
          undefined,
          "Base game isn't installed",
        ),
        message: baseInLib
          ? tr(
              "pkglib.baseMissing.bodyInLib",
              { kind, id: entry.titleId ?? "" },
              `This ${kind} is for ${entry.titleId}, whose base game isn't installed on the PS5 yet — but it's here in your library. Install the base game first, then this ${kind}. Installing the ${kind} now will be accepted but won't actually apply.`,
            )
          : tr(
              "pkglib.baseMissing.body",
              { kind, id: entry.titleId ?? "" },
              `This ${kind} is for ${entry.titleId}, but its base game isn't installed on the PS5. Sony's installer will accept it, but nothing installs until the base game is on the console — install the base first.`,
            ),
        confirmLabel: tr(
          "pkglib.baseMissing.installAnyway",
          undefined,
          "Install anyway",
        ),
      });
      if (!ok) return;
    }
    // Manually installing an alternative is an explicit choice. Remember it
    // for this console so a later Install all never replaces it with a sibling
    // merely because that sibling appears later in the list.
    selectAlternative(entry);
    void install(entry.path, host);
  }

  async function handleDelete(entry: PkgEntry) {
    // `||` so a headerless pkg (empty contentId) shows its filename rather
    // than a blank "Delete ?" dialog.
    const name = entry.title || entry.contentId || entry.name;
    const ok = await confirm({
      title: tr("pkglib.delete.confirmTitle", { name }, `Delete ${name}?`),
      message: tr(
        "pkglib.delete.confirmBody",
        { size: formatBytes(entry.size) },
        `This permanently removes the uploaded .pkg (${formatBytes(
          entry.size,
        )}) from your PS5. Any already-installed copy of the game stays installed; you'd just need to re-upload the .pkg to install it again.`,
      ),
      confirmLabel: tr("pkglib.delete", "Delete"),
      destructive: true,
    });
    if (ok) void remove(entry.path, host);
  }

  const alternativeGroups = useMemo(
    () => pkgAlternativeGroups(entries),
    [entries],
  );
  const alternativeKeyByPath = useMemo(() => {
    const byPath = new Map<string, string>();
    for (const group of alternativeGroups) {
      for (const entry of group.entries) byPath.set(entry.path, group.key);
    }
    return byPath;
  }, [alternativeGroups]);
  // If the console inventory proves one exact artifact is active, use it as
  // the safe default unless the user made an explicit per-console choice.
  const effectiveAlternativeSelections = useMemo(() => {
    const next = { ...alternativeSelections };
    for (const group of alternativeGroups) {
      const identities = new Set(
        group.entries.map((entry) => pkgEntryIdentity(entry)),
      );
      if (next[group.key] && identities.has(next[group.key])) continue;
      const active = group.entries.filter((entry) =>
        pkgRowInstalled(
          entry,
          installedIds,
          entry.titleId
            ? installedArtifacts.get(entry.titleId)
            : undefined,
        ),
      );
      if (active.length === 1) {
        next[group.key] = pkgEntryIdentity(active[0]);
      } else {
        delete next[group.key];
      }
    }
    return next;
  }, [
    alternativeGroups,
    alternativeSelections,
    installedArtifacts,
    installedIds,
  ]);

  function selectAlternative(entry: PkgEntry) {
    const key = alternativeKeyByPath.get(entry.path);
    if (!key) return;
    const identity = pkgEntryIdentity(entry);
    recordPkgAlternativeSelection(host, key, identity);
    setAlternativeSelections((current) => ({
      ...current,
      [key]: identity,
    }));
  }

  const totalSize = useMemo(
    () => entries.reduce((a, e) => a + (e.size || 0), 0),
    [entries],
  );
  // Count of spent (installed) packages — drives the "Clear finished (N)"
  // button so the user can wipe just the clutter in one tap.
  const finishedCount = useMemo(
    () => entries.filter(isFinishedPkg).length,
    [entries],
  );
  // Count of not-yet-installed, idle rows — drives the "Install all (N)"
  // button. Mirrors installAll's own target filter in the store so the count
  // and the action agree. >1 so the button only appears when it saves taps.
  const installedPathsForPlan = useMemo(
    () =>
      entries
        .filter((entry) =>
          pkgRowInstalled(
            entry,
            installedIds,
            entry.titleId
              ? installedArtifacts.get(entry.titleId)
              : undefined,
          ),
        )
        .map((entry) => entry.path),
    [entries, installedArtifacts, installedIds],
  );
  const installPlan = useMemo(() => {
    const installed = new Set(installedPathsForPlan);
    return pkgInstallAllPlan(
      entries.map((entry) => ({
        ...entry,
        installedHere: installed.has(entry.path),
      })),
      effectiveAlternativeSelections,
    );
  }, [entries, effectiveAlternativeSelections, installedPathsForPlan]);
  const installableCount = installPlan.targets.length;
  const titleGroups = useMemo(() => {
    const grouped = new Map<
      string,
      { key: string; title: string; titleId?: string; entries: PkgEntry[] }
    >();
    for (const entry of entries) {
      const key = entry.titleId
        ? `title:${entry.titleId}`
        : entry.contentId
          ? `content:${entry.contentId}`
          : `path:${entry.path}`;
      const existing = grouped.get(key);
      if (existing) {
        existing.entries.push(entry);
        if (entry.title) existing.title = entry.title;
      } else {
        grouped.set(key, {
          key,
          title:
            entry.title || entry.titleId || entry.contentId || entry.name,
          titleId: entry.titleId,
          entries: [entry],
        });
      }
    }
    return [...grouped.values()];
  }, [entries]);
  // Installing swaps the payload out (it owns the transfer port :9113), so it
  // can't run concurrently with an upload. Rather than block the button, the
  // store QUEUES an install behind any active transfer and surfaces a notice
  // (see `busyNotice` / `install()` in pkgLibrary). So the only thing that
  // disables the Install button here is another install already in progress
  // (or queued) — clicking during an upload is allowed and just waits.
  const installBlocked = installing;

  const renderPkgRow = (entry: PkgEntry) => {
    const installed = pkgRowInstalled(
      entry,
      installedIds,
      entry.titleId ? installedArtifacts.get(entry.titleId) : undefined,
    );
    const alternativeKey = alternativeKeyByPath.get(entry.path);
    return (
      <PkgRow
        key={entry.path}
        entry={entry}
        host={host}
        installed={installed}
        installDisabled={installBlocked}
        deleteDisabled={installing}
        alternativeKey={alternativeKey}
        selectedForInstallAll={
          !!alternativeKey &&
          effectiveAlternativeSelections[alternativeKey] ===
            pkgEntryIdentity(entry)
        }
        onSelectAlternative={
          alternativeKey ? () => selectAlternative(entry) : undefined
        }
        onInstall={() => void handleInstall(entry)}
        onDelete={() => void handleDelete(entry)}
      />
    );
  };

  return (
    <div className="p-6">
      <PageHeader
        icon={PackageOpen}
        title={tr("install.title", "Install Package")}
        count={entries.length || undefined}
        loading={loading}
        description={tr(
          "install.description",
          "Upload .pkg files to your PS5, then install them with one tap. Packages stay on the PS5 until you delete them, so you can reinstall any time.",
        )}
        right={
          <div className="flex items-center gap-2">
            {/* Which console this library + install targets. Auto-hides for
                single-PS5 users; color-matches the console's tab so it's
                unambiguous with multiple consoles. */}
            <ConsoleChip addr={host} />
            {/* Install every staged, not-yet-installed package in one tap,
                base → update → DLC order. Only shown when it saves taps
                (>1 installable row — a single row has its own Install button).
                Disabled while any install runs; the store queues behind an
                active upload rather than failing. */}
            {installableCount > 1 && (
              <Button
                variant="secondary"
                size="sm"
                leftIcon={<Download size={14} />}
                onClick={() =>
                  void installAll(
                    host,
                    effectiveAlternativeSelections,
                    installedPathsForPlan,
                  )
                }
                loading={installingAll}
                disabled={!hostReady || installing || installingAll}
                title={
                  !hostReady
                    ? tr(
                        "install.add.disabledHint",
                        "Set a PS5 host on the Connection tab first",
                      )
                    : installing
                      ? tr(
                          "pkglib.add.installingHint",
                          "Wait for the current install to finish",
                        )
                      : tr(
                          "pkglib.installAll.hint",
                          "Install every staged package, base games before updates and DLC",
                        )
                }
              >
                {tr("pkglib.installAll", undefined, "Install all")} (
                {installableCount})
              </Button>
            )}
            {isTauriEnv() && (
              <>
                <Button
                  variant="primary"
                  size="sm"
                  leftIcon={<Plus size={14} />}
                  onClick={handlePick}
                  loading={picking}
                  disabled={!hostReady || installing || installingAll}
                  title={
                    !hostReady
                      ? tr(
                          "install.add.disabledHint",
                          "Set a PS5 host on the Connection tab first",
                        )
                      : installing
                        ? tr(
                            "pkglib.add.installingHint",
                            "Wait for the current install to finish",
                          )
                        : undefined
                  }
                >
                  {tr("install.add", "Add .pkg")}
                </Button>
                <Button
                  variant="secondary"
                  size="sm"
                  leftIcon={
                    streaming ? (
                      <Spinner size={14} tone="inherit" />
                    ) : (
                      <Download size={14} />
                    )
                  }
                  onClick={handleStreamPick}
                  loading={streaming}
                  disabled={!hostReady || installing || installingAll}
                  title={
                    !hostReady
                      ? tr(
                          "install.add.disabledHint",
                          "Set a PS5 host on the Connection tab first",
                        )
                      : tr(
                          "pkglib.stream.hint",
                          "Install a .pkg straight from this PC over HTTP — no staging upload (beta)",
                        )
                  }
                >
                  {tr("pkglib.stream", undefined, "Stream (beta)")}
                </Button>
              </>
            )}
          </div>
        }
      />

      <div className="mb-4 flex items-start gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 text-[12px] text-[var(--color-muted)]">
        <Info size={13} className="mt-0.5 shrink-0" />
        <div>
          <span className="font-medium text-[var(--color-text)]">
            {tr("pkglib.installnote.title", "How installing works")}
          </span>
          {" — "}
          {tr(
            "pkglib.installnote.body",
            "ps5upload installs the package on the PS5 for you. It briefly takes over the payload to run the install (falling back to the DPI loader if needed) and restores it when done, so the connection may blip for a few seconds. On FW 12+ the screen can go black for a moment — that's normal. Game pkgs work best; some system (NPXS) pkgs may still need the PS5's own Settings → Package Installer.",
          )}
        </div>
      </div>

      {/* Workflow options, grouped near the top where they're set before
          adding a package (not buried under the library list). Both govern the
          hands-off "add → installed → cleaned up" flow, so they read together. */}
      <div className="mb-4 flex flex-col gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3">
        <span className="text-xs font-medium uppercase tracking-wide text-[var(--color-muted)]">
          {tr("pkglib.options.heading", "Options")}
        </span>
        <Checkbox
          checked={autoInstall}
          onChange={setAutoInstall}
          label={tr(
            "pkglib.autoInstall",
            undefined,
            "Install automatically once the upload finishes",
          )}
        />
        <Checkbox
          checked={autoRemove}
          onChange={setAutoRemove}
          label={tr(
            "pkglib.autoRemove",
            undefined,
            "Auto-delete each package from the PS5 after it installs",
          )}
        />
      </div>

      {!hostReady && (
        <div className="mb-4">
          <WarningCard
            title={tr(
              "install.noTarget",
              "No PS5 host set — open the Connection tab to set one.",
            )}
          />
        </div>
      )}

      {pickError && (
        <div className="mb-4">
          <WarningCard
            title={tr("install.pickError", "Could not add file")}
            detail={pickError}
          />
        </div>
      )}
      {error && (
        <div className="mb-4">
          <WarningCard
            title={tr("pkglib.error", "Something went wrong")}
            detail={error}
          />
        </div>
      )}

      {busyNotice && (
        <div className="mb-4 flex items-center justify-between gap-3 rounded-lg border border-[var(--color-accent)] bg-[var(--color-accent-soft)] px-4 py-3 text-sm">
          <div className="flex items-start gap-2">
            <Spinner size={14} tone="accent" className="mt-0.5 shrink-0" />
            <span>{busyNotice}</span>
          </div>
          {/* Only offer Cancel while the install is still WAITING its turn.
              During the real install (FW 12.x keeps busyNotice set for the
              "screen may go black" notice) cancelling would tear the payload
              out mid-swap, so the button is hidden then. */}
          {installPending && (
            <Button
              variant="secondary"
              size="sm"
              onClick={cancelPendingInstall}
            >
              {tr("cancel", undefined, "Cancel")}
            </Button>
          )}
        </div>
      )}

      {hostReady && <ExternalPackages host={host} />}

      {hostReady && entries.length === 0 && !loading ? (
        <EmptyState
          icon={dropActive ? HardDrive : PackageOpen}
          size="hero"
          title={
            dropActive
              ? tr("pkglib.empty.drop", "Drop to upload")
              : tr("pkglib.empty.title", "No packages uploaded yet")
          }
          message={tr(
            "pkglib.empty.body",
            "Add a .pkg to upload it to your PS5 — then install it from here. You can also drag .pkg files onto the window.",
          )}
        />
      ) : (
        <>
          <div className="grid gap-4">
            {titleGroups.map((group) => {
              const sections = [
                {
                  key: "base",
                  label: tr("pkglib.section.base", undefined, "Base game"),
                  rows: group.entries.filter(
                    (entry) =>
                      entry.category !== "gp" && entry.category !== "ac",
                  ),
                },
                {
                  key: "updates",
                  label: tr("pkglib.section.updates", undefined, "Updates"),
                  rows: group.entries.filter(
                    (entry) => entry.category === "gp",
                  ),
                },
                {
                  key: "dlc",
                  label: tr("pkglib.section.dlc", undefined, "DLC"),
                  rows: group.entries.filter(
                    (entry) => entry.category === "ac",
                  ),
                },
              ].filter((section) => section.rows.length > 0);
              const groupPaths = new Set(
                group.entries.map((entry) => entry.path),
              );
              const alternatives = alternativeGroups.filter((alternative) =>
                alternative.entries.some((entry) => groupPaths.has(entry.path)),
              );
              const unresolved = alternatives.filter(
                (alternative) =>
                  !effectiveAlternativeSelections[alternative.key],
              ).length;

              return (
                <section
                  key={group.key}
                  className="rounded-xl border border-[var(--color-border)] bg-[var(--color-surface-1)] p-3"
                >
                  <div className="mb-3 flex flex-wrap items-center justify-between gap-2 border-b border-[var(--color-border)] pb-3">
                    <div className="min-w-0">
                      <h2 className="truncate text-sm font-semibold">
                        {group.title}
                      </h2>
                      <div className="mt-0.5 flex flex-wrap items-center gap-2 text-xs text-[var(--color-muted)]">
                        {group.titleId && (
                          <span className="font-mono">{group.titleId}</span>
                        )}
                        <span>
                          {tr(
                            "pkglib.group.count",
                            { n: group.entries.length },
                            `${group.entries.length} package${group.entries.length === 1 ? "" : "s"}`,
                          )}
                        </span>
                      </div>
                    </div>
                    {alternatives.length > 0 && (
                      <Badge
                        tone={unresolved > 0 ? "warn" : "accent"}
                        variant="soft"
                      >
                        {unresolved > 0
                          ? `${unresolved} choice${unresolved === 1 ? "" : "s"} needed`
                          : `${alternatives.length} variant choice${alternatives.length === 1 ? "" : "s"} set`}
                      </Badge>
                    )}
                  </div>

                  {alternatives.length > 0 && (
                    <div className="mb-3 flex items-start gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-2.5 text-xs text-[var(--color-muted)]">
                      <Info size={13} className="mt-0.5 shrink-0" />
                      <span>
                        {tr(
                          "pkglib.variant.help",
                          undefined,
                          "Every update/DLC variant is preserved below. Choose one same-version alternative for this PS5; Install all uses the selected row and leaves its siblings staged.",
                        )}
                      </span>
                    </div>
                  )}

                  <div className="grid gap-4">
                    {sections.map((section) => (
                      <div key={section.key}>
                        <div className="mb-2 flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
                          <span>{section.label}</span>
                          <span className="rounded-full bg-[var(--color-surface-3)] px-1.5 py-0.5 font-mono text-[10px] tabular-nums">
                            {section.rows.length}
                          </span>
                        </div>
                        <ul className="grid gap-2">
                          {section.rows.map(renderPkgRow)}
                        </ul>
                      </div>
                    ))}
                  </div>
                </section>
              );
            })}
          </div>
          {entries.length > 0 && (
            <div className="mt-3 flex flex-wrap items-center justify-between gap-2 border-t border-[var(--color-border)] pt-3 text-xs text-[var(--color-muted)]">
              <span>
                {tr(
                  "pkglib.footer.count",
                  { n: entries.length },
                  `${entries.length} package${entries.length === 1 ? "" : "s"}`,
                )}
              </span>
              <div className="flex items-center gap-2">
                {finishedCount > 0 && (
                  <Button
                    variant="secondary"
                    size="sm"
                    disabled={installing}
                    onClick={() => void clearFinished(host)}
                  >
                    {tr(
                      "pkglib.clearFinished",
                      { n: finishedCount },
                      `Clear finished (${finishedCount})`,
                    )}
                  </Button>
                )}
                <Button
                  variant="secondary"
                  size="sm"
                  disabled={installing}
                  onClick={async () => {
                    const ok = await confirm({
                      title: tr(
                        "pkglib.clearAll.confirmTitle",
                        undefined,
                        "Delete all staged packages?",
                      ),
                      message: tr(
                        "pkglib.clearAll.confirmBody",
                        { n: entries.length },
                        `This permanently deletes all ${entries.length} staged .pkg file(s) from the PS5. Installed games are not affected.`,
                      ),
                      confirmLabel: tr(
                        "pkglib.clearAll",
                        undefined,
                        "Clear all",
                      ),
                      destructive: true,
                    });
                    if (ok) void clearAll(host);
                  }}
                >
                  {tr("pkglib.clearAll", undefined, "Clear all")}
                </Button>
                <span className="tabular-nums">
                  {tr(
                    "pkglib.footer.size",
                    { size: formatBytes(totalSize) },
                    `${formatBytes(totalSize)} on PS5`,
                  )}
                </span>
              </div>
            </div>
          )}
        </>
      )}
      {dialog}
    </div>
  );
}

/**
 * Packages found on connected USB / external drives (`/mnt/usb*`, `/mnt/ext*`).
 * Installing one copies it to internal storage on-console first — Sony's
 * installer can't read the exfat USB mount directly (hardware-confirmed) — then
 * runs the normal install cascade. This is ADDITIVE to the upload-then-install
 * flow above; nothing here changes that path.
 */
function ExternalPackages({ host }: { host: string }) {
  const tr = useTr();
  const installExternal = usePkgLibrary(host, (s) => s.installExternal);
  const installing = usePkgLibrary(host, (s) => s.installing);
  const autoScan = useInstallSettingsStore((s) => s.autoScanExternal);
  const setAutoScan = useInstallSettingsStore((s) => s.setAutoScanExternal);
  const [pkgs, setPkgs] = useState<ExternalPkg[]>([]);
  const [scanning, setScanning] = useState(false);
  const [scanned, setScanned] = useState(false);
  const [installingPath, setInstallingPath] = useState<string | null>(null);
  const [results, setResults] = useState<
    Record<
      string,
      {
        ok: boolean;
        message?: string;
        mayNotLaunch?: boolean;
        acceptedUnverified?: boolean;
      }
    >
  >({});
  // Lazily-fetched authoritative metadata (title, version, category), keyed by
  // path. The bulk scan is filename-fast and skips these; we fill them in per
  // row in the background so the list still appears instantly. `enrichedRef`
  // dedupes so a re-render / rescan doesn't re-fetch what we already have.
  const [meta, setMeta] = useState<Record<string, PkgConsoleMetadata>>({});
  const enrichedRef = useRef<Set<string>>(new Set());

  async function scan() {
    setScanning(true);
    try {
      setPkgs(await pkgScanExternal(transferAddr(host)));
      setScanned(true);
    } catch {
      // Best-effort: no drives / scan failure just shows an empty section.
      setPkgs([]);
      setScanned(true);
    } finally {
      setScanning(false);
    }
  }

  // Scan once when the host becomes ready — but only if auto-scan is on. With
  // it off, nothing is scanned until the user clicks Scan (the manual button is
  // always available). The pkg stays on the drive after install, so the list
  // itself doesn't change; the per-row result line reflects the outcome.
  useEffect(() => {
    if (autoScan) void scan();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host, autoScan]);

  // Background enrichment: walk the scanned packages one at a time (gentle on
  // the console's FS RPC) and pull each one's real title / version / category.
  // Rows render immediately with scan data and upgrade in place as this fills.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      for (const p of pkgs) {
        if (cancelled) return;
        if (enrichedRef.current.has(p.path)) continue;
        enrichedRef.current.add(p.path);
        const m = await pkgMetadataConsole(transferAddr(host), p.path);
        if (cancelled) return;
        if (m && (m.title || m.appVer || m.category)) {
          setMeta((prev) => ({ ...prev, [p.path]: m }));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [pkgs, host]);

  async function onInstall(pkg: ExternalPkg) {
    setInstallingPath(pkg.path);
    try {
      const r = await installExternal(pkg, host);
      setResults((prev) => ({ ...prev, [pkg.path]: r }));
    } finally {
      setInstallingPath(null);
    }
  }

  // Always render once a console is connected — previously the whole section
  // (Rescan button included) vanished whenever a scan found nothing, so it both
  // "popped in" when results arrived and left no way to refresh when empty.
  // Now it's a stable panel with explicit scanning / empty / list states.
  const firstScan = scanning && !scanned;

  return (
    <div className="mb-4 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3">
      <div className="mb-2 flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
          <HardDrive
            size={14}
            className="shrink-0 text-[var(--color-accent)]"
          />
          <span className="text-sm font-semibold">
            {tr("pkglib.external.title", "Install from USB / external drive")}
          </span>
          {pkgs.length > 0 && (
            <span className="text-xs text-[var(--color-muted)]">
              {tr(
                "pkglib.external.count",
                { n: pkgs.length },
                `${pkgs.length} found`,
              )}
            </span>
          )}
        </div>
        <Button
          variant="secondary"
          size="sm"
          leftIcon={
            scanning ? (
              <Spinner size={14} tone="inherit" />
            ) : (
              <RotateCcw size={13} />
            )
          }
          disabled={scanning || installing}
          onClick={() => void scan()}
        >
          {scanning
            ? tr("pkglib.external.scanning", "Scanning…")
            : scanned
              ? tr("pkglib.external.rescan", "Refresh")
              : tr("pkglib.external.scan", "Scan")}
        </Button>
      </div>
      <div className="mb-2 text-xs leading-relaxed text-[var(--color-muted)]">
        {tr(
          "pkglib.external.hint",
          "Plug a USB stick or external drive with .pkg files into the PS5 and they show up here — no upload needed. Installing copies the file onto the console first (your drive's copy is left untouched), then installs it. Use Scan after connecting a drive.",
        )}
      </div>
      <Checkbox
        className="mb-2"
        checked={autoScan}
        onChange={setAutoScan}
        label={tr(
          "pkglib.external.autoScan",
          "Automatically scan USB / external drives when this tab opens",
        )}
      />

      {firstScan ? (
        // Stable scanning state — no more "suddenly appears" pop-in.
        <div className="flex items-center gap-2 rounded-md border border-dashed border-[var(--color-border)] px-3 py-4 text-xs text-[var(--color-muted)]">
          <Spinner size={14} />
          {tr("pkglib.external.scanningDrives", "Scanning connected drives…")}
        </div>
      ) : !scanned ? (
        // Auto-scan is off and we haven't scanned yet — prompt rather than
        // claiming "nothing found" (we haven't looked).
        <div className="rounded-md border border-dashed border-[var(--color-border)] px-3 py-4 text-xs text-[var(--color-muted)]">
          {tr(
            "pkglib.external.notScanned",
            "Auto-scan is off. Connect a USB or external drive with .pkg files, then click Scan.",
          )}
        </div>
      ) : pkgs.length === 0 ? (
        // Empty state — informative, and the Scan button above stays put.
        <div className="rounded-md border border-dashed border-[var(--color-border)] px-3 py-4 text-xs text-[var(--color-muted)]">
          {tr(
            "pkglib.external.empty",
            "No .pkg files found on connected USB or external drives. Connect a drive that has .pkg files on it, then click Scan.",
          )}
        </div>
      ) : (
        <ul className="grid gap-2">
          {pkgs.map((p) => {
            const r = results[p.path];
            const m = meta[p.path];
            // Prefer the authoritative title; fall back to the title id, then
            // the filename. The filename is shown on its own line below.
            const heading = m?.title || p.titleId || p.name;
            const platform = m?.platform || p.platform;
            const titleId = m?.titleId || p.titleId;
            const catLabel = pkgCategoryLabel(m?.category);
            return (
              <li
                key={p.path}
                className="flex items-center gap-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2"
              >
                <GameIcon host={host} titleId={titleId} size={44} />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <PlatformBadge platform={platform} />
                    <span
                      className="truncate text-sm font-medium"
                      title={p.path}
                    >
                      {heading}
                    </span>
                    {catLabel && catLabel !== "Base" && (
                      <Badge tone="accent" variant="outline">
                        {catLabel === "Update"
                          ? tr("pkglib.badge.update", "update")
                          : tr("pkglib.badge.dlc", "DLC")}
                      </Badge>
                    )}
                    {m?.appVer && (
                      <Badge
                        tone="neutral"
                        variant="outline"
                        className="font-mono"
                      >
                        v{m.appVer}
                      </Badge>
                    )}
                  </div>
                  {/* Filename — often the clearest identifier (carries the
                      game name + version), and distinct from the heading. */}
                  {p.name && p.name !== heading && (
                    <div
                      className="mt-0.5 flex items-center gap-1 truncate text-xs text-[var(--color-muted)]"
                      title={p.name}
                    >
                      <FileText size={11} className="shrink-0 opacity-70" />
                      <span className="truncate">{p.name}</span>
                    </div>
                  )}
                  <div className="mt-0.5 truncate font-mono text-xs text-[var(--color-muted)]">
                    {p.drive}
                    <span className="px-1 opacity-60">·</span>
                    <span className="tabular-nums">{formatBytes(p.size)}</span>
                  </div>
                  {/* Install result on its own line with an icon, including
                      accepted-but-unverified outcomes. Mirrors library rows. */}
                  {r && (
                    <div
                      className="mt-1 flex items-center gap-1.5 text-xs font-medium"
                      style={{
                        color: r.ok
                          ? r.mayNotLaunch
                            ? "var(--color-warn)"
                            : "var(--color-good)"
                          : r.acceptedUnverified
                            ? "var(--color-warn)"
                            : "var(--color-bad)",
                      }}
                    >
                      {r.ok ? (
                        r.mayNotLaunch ? (
                          <AlertTriangle size={13} className="shrink-0" />
                        ) : (
                          <CheckCircle2 size={13} className="shrink-0" />
                        )
                      ) : r.acceptedUnverified ? (
                        <AlertTriangle size={13} className="shrink-0" />
                      ) : (
                        <XCircle size={13} className="shrink-0" />
                      )}
                      <span className="min-w-0">
                        {r.ok
                          ? r.mayNotLaunch
                            ? tr(
                                "pkglib.external.installedWarn",
                                "installed (may not launch)",
                              )
                            : tr("pkglib.external.installed", "installed")
                          : r.message ||
                            tr("pkglib.external.failed", "install failed")}
                      </span>
                    </div>
                  )}
                </div>
                <Button
                  variant="primary"
                  size="sm"
                  leftIcon={
                    installingPath === p.path ? (
                      <Spinner size={14} tone="inherit" />
                    ) : (
                      <Download size={13} />
                    )
                  }
                  disabled={installing}
                  onClick={() => void onInstall(p)}
                >
                  {installingPath === p.path
                    ? tr("pkglib.external.installingThis", "Installing…")
                    : tr("pkglib.external.install", "Install")}
                </Button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
