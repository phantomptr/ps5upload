import { useEffect, useMemo, useRef, useState } from "react";
import { useLocation } from "react-router-dom";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  PackageOpen,
  Plus,
  Trash2,
  Download,
  RotateCcw,
  Loader2,
  CheckCircle2,
  XCircle,
  AlertTriangle,
  Gamepad2,
  Info,
  HardDrive,
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
} from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";
import {
  usePkgLibrary,
  isFinishedPkg,
  type PkgEntry,
} from "../../state/pkgLibrary";
import { useInstallSettingsStore } from "../../state/installSettings";
import { pkgCategoryLabel } from "../../lib/pkgStagingPath";
import { appsInstalled, appIconUrl } from "../../api/ps5";
import { transferAddr } from "../../lib/addr";
import { formatBytes } from "../../lib/format";

/* ─── Cover art ────────────────────────────────────────────────────────
 * Keyed by title id (derived from the ContentID). Falls back to a glyph on
 * 404 — common for homebrew with no icon, or pkgs not yet installed whose
 * appmeta icon doesn't exist. Same render+fallback trick as the Library /
 * Installed Apps screens. */
function Cover({ host, titleId }: { host: string; titleId?: string }) {
  const [failed, setFailed] = useState(false);
  const show = !failed && !!titleId && !!host.trim();
  return (
    <div className="flex aspect-square w-14 shrink-0 items-center justify-center overflow-hidden rounded-md bg-[var(--color-surface-3)]">
      {show ? (
        <img
          src={appIconUrl(transferAddr(host), titleId!)}
          alt=""
          className="h-full w-full object-cover"
          loading="lazy"
          onError={() => setFailed(true)}
        />
      ) : (
        <Gamepad2 size={20} className="text-[var(--color-muted)]" />
      )}
    </div>
  );
}

/* ─── One package row ─────────────────────────────────────────────────── */
function PkgRow({
  entry,
  host,
  installed,
  installDisabled,
  deleteDisabled,
  onInstall,
  onDelete,
}: {
  entry: PkgEntry;
  host: string;
  installed: boolean;
  installDisabled: boolean;
  deleteDisabled: boolean;
  onInstall: () => void;
  onDelete: () => void;
}) {
  const tr = useTr();
  const uploading = entry.status === "uploading";
  const installingThis = entry.status === "installing";
  const queued = entry.status === "queued";
  const busy = uploading || installingThis || queued;
  const pct =
    uploading && entry.totalBytes
      ? Math.min(100, Math.round(((entry.bytes ?? 0) / entry.totalBytes) * 100))
      : 0;

  return (
    <li className="flex flex-col gap-2 rounded-xl border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3">
      <div className="flex items-center gap-3">
        <Cover host={host} titleId={entry.titleId} />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span
              className="truncate text-sm font-medium"
              title={entry.title || entry.contentId || entry.name}
            >
              {entry.title || entry.contentId || entry.name}
            </span>
            {installed && !busy && (
              <span className="inline-flex shrink-0 items-center gap-1 rounded-full border border-[var(--color-good)] bg-[var(--color-good-soft)] px-1.5 py-0.5 text-xs font-medium uppercase tracking-wide text-[var(--color-good)]">
                {tr("pkglib.badge.installed", "installed")}
              </span>
            )}
            {/* Update / DLC badge — a base game and its update share a
                ContentID, so without this they look identical. */}
            {pkgCategoryLabel(entry.category) &&
              pkgCategoryLabel(entry.category) !== "Base" && (
                <span className="inline-flex shrink-0 items-center rounded-full border border-[var(--color-accent)] bg-[var(--color-accent-soft,transparent)] px-1.5 py-0.5 text-xs font-medium uppercase tracking-wide text-[var(--color-accent)]">
                  {pkgCategoryLabel(entry.category) === "Update"
                    ? tr("pkglib.badge.update", "update")
                    : tr("pkglib.badge.dlc", "DLC")}
                </span>
              )}
          </div>
          <div className="mt-0.5 truncate font-mono text-xs text-[var(--color-muted)]">
            {entry.contentId || entry.name}
            <span className="px-1 opacity-60">·</span>
            <span className="tabular-nums">{formatBytes(entry.size)}</span>
          </div>
        </div>

        {/* Actions */}
        {!busy && (
          <div className="flex shrink-0 items-center gap-1.5">
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
          </div>
        )}
        {installingThis && (
          <div className="flex shrink-0 items-center gap-2 text-xs text-[var(--color-accent)]">
            <Loader2 size={14} className="animate-spin" />
            {tr("pkglib.installing", "Installing…")}
          </div>
        )}
        {queued && (
          <div className="flex shrink-0 items-center gap-2 text-xs text-[var(--color-muted)]">
            <Loader2 size={14} className="animate-spin" />
            {tr(
              "pkglib.queued",
              undefined,
              "Queued — waiting for the current transfer",
            )}
          </div>
        )}
      </div>

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
              {entry.totalBytes ? ` / ${formatBytes(entry.totalBytes)}` : ""} ·{" "}
              {pct}%
            </span>
          </div>
        </div>
      )}

      {/* Last install result. `warn` is a soft-success: installed, but via
          the unlaunchable last-resort path — amber, with a triangle, so the
          user knows it may not boot (the FW-12 "can't start the game" case). */}
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

/* ─── Screen ──────────────────────────────────────────────────────────── */
export default function InstallPackageScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  // Per-console store: every selector is scoped to THIS console's host, so the
  // Install Package view is fully isolated per PS5 (parallel installs).
  const entries = usePkgLibrary(host, (s) => s.entries);
  const loading = usePkgLibrary(host, (s) => s.loading);
  const error = usePkgLibrary(host, (s) => s.error);
  const installing = usePkgLibrary(host, (s) => s.installing);
  const busyNotice = usePkgLibrary(host, (s) => s.busyNotice);
  const refresh = usePkgLibrary(host, (s) => s.refresh);
  const addAndUpload = usePkgLibrary(host, (s) => s.addAndUpload);
  const install = usePkgLibrary(host, (s) => s.install);
  const cancelPendingInstall = usePkgLibrary(host, (s) => s.cancelPendingInstall);
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

  const [installedIds, setInstalledIds] = useState<Set<string>>(new Set());
  const [pickError, setPickError] = useState<string | null>(null);
  const [picking, setPicking] = useState(false);
  const [dropActive, setDropActive] = useState(false);

  const hostReady = !!host?.trim();
  // Stable ref so the drag-drop effect (subscribed once per host) always
  // calls the latest uploader without re-subscribing each render. Updated
  // in an effect (not during render) per the rules-of-hooks ref rule.
  const uploadRef = useRef<(p: string) => void>(() => {});
  useEffect(() => {
    uploadRef.current = (p: string) => {
      if (hostReady) void addAndUpload(p, host);
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
        if (!cancelled) {
          setInstalledIds(new Set(res.titles.map((t) => t.titleId)));
        }
      })
      .catch(() => {
        /* badge is best-effort */
      });
    return () => {
      cancelled = true;
    };
  }, [host, hostReady, entries.length, installing]);

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
        tr("install.error.noHost", "Set a PS5 host on the Connection tab first."),
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

  async function handleDelete(entry: PkgEntry) {
    // `||` so a headerless pkg (empty contentId) shows its filename rather
    // than a blank "Delete ?" dialog.
    const name = entry.title || entry.contentId || entry.name;
    const ok = await confirm({
      title: tr(
        "pkglib.delete.confirmTitle",
        { name },
        `Delete ${name}?`,
      ),
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
  const sorted = entries; // store already sorts by title
  // Installing swaps the payload out (it owns the transfer port :9113), so it
  // can't run concurrently with an upload. Rather than block the button, the
  // store QUEUES an install behind any active transfer and surfaces a notice
  // (see `busyNotice` / `install()` in pkgLibrary). So the only thing that
  // disables the Install button here is another install already in progress
  // (or queued) — clicking during an upload is allowed and just waits.
  const installBlocked = installing;

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
            <Button
              variant="primary"
              size="sm"
              leftIcon={<Plus size={14} />}
              onClick={handlePick}
              loading={picking}
              disabled={!hostReady || installing}
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
        <span className="text-[11px] font-medium uppercase tracking-wide text-[var(--color-muted)]">
          {tr("pkglib.options.heading", "Options")}
        </span>
        <label className="flex cursor-pointer items-center gap-2 text-xs text-[var(--color-text)]">
          <input
            type="checkbox"
            className="h-3.5 w-3.5"
            checked={autoInstall}
            onChange={(e) => setAutoInstall(e.target.checked)}
          />
          {tr(
            "pkglib.autoInstall",
            undefined,
            "Install automatically once the upload finishes",
          )}
        </label>
        <label className="flex cursor-pointer items-center gap-2 text-xs text-[var(--color-text)]">
          <input
            type="checkbox"
            className="h-3.5 w-3.5"
            checked={autoRemove}
            onChange={(e) => setAutoRemove(e.target.checked)}
          />
          {tr(
            "pkglib.autoRemove",
            undefined,
            "Auto-delete each package from the PS5 after it installs",
          )}
        </label>
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
          <WarningCard title={tr("pkglib.error", "Something went wrong")} detail={error} />
        </div>
      )}

      {busyNotice && (
        <div className="mb-4 flex items-center justify-between gap-3 rounded-lg border border-[var(--color-accent)] bg-[var(--color-accent-soft)] px-4 py-3 text-sm">
          <div className="flex items-start gap-2">
            <Loader2
              size={14}
              className="mt-0.5 shrink-0 animate-spin text-[var(--color-accent)]"
            />
            <span>{busyNotice}</span>
          </div>
          <Button variant="secondary" size="sm" onClick={cancelPendingInstall}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
        </div>
      )}

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
          <ul className="grid gap-2">
            {sorted.map((e) => {
              const installed = !!e.titleId && installedIds.has(e.titleId);
              return (
                <PkgRow
                  key={e.path}
                  entry={e}
                  host={host}
                  installed={installed}
                  installDisabled={installBlocked}
                  deleteDisabled={installing}
                  onInstall={() => void install(e.path, host)}
                  onDelete={() => void handleDelete(e)}
                />
              );
            })}
          </ul>
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
                      confirmLabel: tr("pkglib.clearAll", undefined, "Clear all"),
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
