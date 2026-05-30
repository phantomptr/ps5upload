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
  Gamepad2,
  Info,
  HardDrive,
} from "lucide-react";

import { isAndroid } from "../../lib/platform";
import { pickPath } from "../../lib/pickPath";
import { isTauriEnv, safeUnlisten } from "../../lib/tauriEnv";
import { PageHeader, Button, EmptyState, WarningCard } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";
import { usePkgLibrary, type PkgEntry } from "../../state/pkgLibrary";
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
  const busy = uploading || installingThis;
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
              title={entry.title ?? entry.contentId}
            >
              {entry.title ?? entry.contentId ?? entry.name}
            </span>
            {installed && !busy && (
              <span className="inline-flex shrink-0 items-center gap-1 rounded-full border border-[var(--color-good)] bg-[var(--color-good-soft)] px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-[var(--color-good)]">
                {tr("pkglib.badge.installed", "installed")}
              </span>
            )}
          </div>
          <div className="mt-0.5 truncate font-mono text-[11px] text-[var(--color-muted)]">
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
                      "Wait for the current upload/install to finish",
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
          <div className="flex items-center justify-between text-[11px] text-[var(--color-muted)]">
            <span>{tr("pkglib.uploading", "Uploading to PS5…")}</span>
            <span className="tabular-nums">
              {formatBytes(entry.bytes ?? 0)}
              {entry.totalBytes ? ` / ${formatBytes(entry.totalBytes)}` : ""} ·{" "}
              {pct}%
            </span>
          </div>
        </div>
      )}

      {/* Last install result */}
      {!busy && entry.lastResult && (
        <div
          className={`flex items-start gap-1.5 text-[11px] ${
            entry.lastResult.ok
              ? "text-[var(--color-good)]"
              : "text-[var(--color-bad)]"
          }`}
        >
          {entry.lastResult.ok ? (
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
  const entries = usePkgLibrary((s) => s.entries);
  const loading = usePkgLibrary((s) => s.loading);
  const error = usePkgLibrary((s) => s.error);
  const installing = usePkgLibrary((s) => s.installing);
  const refresh = usePkgLibrary((s) => s.refresh);
  const addAndUpload = usePkgLibrary((s) => s.addAndUpload);
  const install = usePkgLibrary((s) => s.install);
  const remove = usePkgLibrary((s) => s.remove);
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
    const ok = await confirm({
      title: tr(
        "pkglib.delete.confirmTitle",
        { name: entry.title ?? entry.contentId },
        `Delete ${entry.title ?? entry.contentId}?`,
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
  const sorted = entries; // store already sorts by title
  // Installing swaps the main payload for the DPI loader, which owns the
  // transfer port — so an install MUST NOT run while a package is still
  // uploading (it would tear down the in-flight transfer), and Add is
  // blocked while installing for the same reason. Uploads themselves can
  // run concurrently.
  const anyUploading = entries.some((e) => e.status === "uploading");
  const installBlocked = installing || anyUploading;

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
        }
      />

      <div className="mb-4 flex items-start gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 text-[12px] text-[var(--color-muted)]">
        <Info size={13} className="mt-0.5 shrink-0" />
        <div>
          <span className="font-medium text-[var(--color-text)]">
            {tr("pkglib.note.title", "Installs run through the DPI daemon")}
          </span>
          {" — "}
          {tr(
            "pkglib.note.body",
            "the cleanest path for game pkgs. Installing briefly swaps the ps5upload payload for the DPI loader and restores it when done, so the connection may blip for a few seconds. Game pkgs work best; some system (NPXS) pkgs may still need the PS5's own Settings → Package Installer.",
          )}
        </div>
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
            <div className="mt-3 flex items-center justify-between border-t border-[var(--color-border)] pt-3 text-[11px] text-[var(--color-muted)]">
              <span>
                {tr(
                  "pkglib.footer.count",
                  { n: entries.length },
                  `${entries.length} package${entries.length === 1 ? "" : "s"}`,
                )}
              </span>
              <span className="tabular-nums">
                {tr(
                  "pkglib.footer.size",
                  { size: formatBytes(totalSize) },
                  `${formatBytes(totalSize)} on PS5`,
                )}
              </span>
            </div>
          )}
        </>
      )}
      {dialog}
    </div>
  );
}
