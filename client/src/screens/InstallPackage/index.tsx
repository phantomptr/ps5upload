import { useEffect, useMemo, useRef, useState } from "react";
import {
  failedStreamInstallIds,
  libraryInstallStates,
  useUploadQueueStore,
} from "../../state/uploadQueue";
import { isRemotePath } from "../../lib/remotePath";
import { PackagePanel } from "../../components/PackagePanel";
import { QueuePanel, queueItemsForHost } from "../Upload/QueuePanel";
import { volumeOfPkgPath } from "../../lib/pkgStorage";
import { useLocation, useNavigate } from "react-router";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  PackageOpen,
  Upload,
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
  ChevronDown,
} from "lucide-react";

import { isAndroid } from "../../lib/platform";
import { pickPath } from "../../lib/pickPath";
import { isTauriEnv, safeUnlisten } from "../../lib/tauriEnv";
import { engineIsOnThisDevice } from "../../state/engine";
import {
  PageHeader,
  Button,
  EmptyState,
  WarningCard,
  ConnectionGate,
  ConsoleChip,
  GameIcon,
  OverflowMenu,
  PlatformBadge,
  Spinner,
  Badge,
  type OverflowMenuItem,
  Toggle,
} from "../../components";
import { BrowseButton } from "../../components/BrowseButton";
import { FakeGameFirmwareNotice } from "../../components/FakeGameFirmwareNotice";
import { DOC_ANCHORS, faqLink, installErrorLink } from "../../lib/installErrorDoc";
import { openInFileSystem } from "../../state/fsNavigation";
import { useConfirm } from "../../components/ConfirmDialog";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";
import { pickLocalPath } from "../../state/localPicker";
import {
  usePkgLibrary,
  isFinishedPkg,
  pkgRowInstalled,
  pkgInstallAllPlan,
  pkgAlternativeGroups,
  pkgEntryIdentity,
  loadPkgAlternativeSelections,
  recordPkgAlternativeSelection,
  skipPkgAlternativeSelection,
  PKG_ALTERNATIVE_SKIP,
  type PkgEntry,
  type PkgAlternativeSelections,
} from "../../state/pkgLibrary";
import { NetworkFixActions } from "./NetworkFixActions";
import { useLinkInstallPrefs } from "../../state/linkInstallPrefs";
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
import { linkProbe, type LinkClass } from "../../api/links";
import { LinkDownloadCard } from "./LinkDownloadCard";
import { RarPackagesCard } from "./RarPackagesCard";
import { formatBytes, formatDuration } from "../../lib/format";
import { remainingSeconds } from "../../lib/rollingRate";
import { acceptPkgDrop, isInstallPackagePath } from "../../lib/pkgDropDedupe";
import { writeClipboard } from "../../lib/clipboard";
import { deleteBrowserPkgUpload, stageBrowserPkg } from "../../api/pkgUpload";

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
  onRetryStream,
  onDelete,
  onView,
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
  /** Re-run a refused package through Stream (shown only when the engine offers it). */
  onRetryStream?: () => void;
  onDelete: () => void;
  /** Open the package viewer on this row's original file (when known). */
  onView?: () => void;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const kernel = useConnectionStore((s) => s.runtimeByHost[hostOf(host)]?.ps5Kernel ?? null);
  // After the console refused this package from its own storage, Retry with Stream is offered
  // beside Install when the engine says that is safe for this package.
  const streamOffered = !!entry.lastResult?.retryWithStream && !!onRetryStream;
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
        void writeClipboard(
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
  // Speed + time remaining, the same readout the Upload queue shows. Both are
  // hidden once bytes reach the total: the PS5 is then committing the file,
  // the last rate no longer describes anything, and an ETA would sit at "0s".
  const uploadRate = uploading ? (entry.bytesPerSec ?? 0) : 0;
  const uploadEtaSec = uploading
    ? remainingSeconds(entry.bytes ?? 0, entry.totalBytes ?? 0, uploadRate)
    : null;
  const uploadFinalizing =
    uploading && !!entry.totalBytes && (entry.bytes ?? 0) >= entry.totalBytes;

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
              {entry.authenticity === "fake_debug" && (
                <Badge
                  tone="accent"
                  variant="soft"
                  title={tr(
                    "pkglib.auth.fake.title",
                    "Debug/fake-signed package, identified from the PS5 FIH envelope.",
                  )}
                >
                  {tr("pkglib.auth.fake", "fake/debug")}
                </Badge>
              )}
              {entry.authenticity === "retail" && (
                <Badge
                  tone="warn"
                  variant="soft"
                  title={tr(
                    "pkglib.auth.retail.title",
                    "Retail-signed package. Installing it does not grant a license; the console still needs a valid entitlement to launch it.",
                  )}
                >
                  {tr("pkglib.auth.retail", "retail")}
                </Badge>
              )}
              {/* Problems found while parsing the package itself. Shown here
                  so the user sees them BEFORE spending an upload and an
                  install on a package the console will refuse to run. */}
              {(entry.warnings?.length ?? 0) > 0 && (
                <Badge
                  tone="warn"
                  variant="soft"
                  title={entry.warnings?.join("\n")}
                >
                  {tr("pkglib.badge.pkgWarning", "check package")}
                </Badge>
              )}
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
              {volumeOfPkgPath(entry.path) && (
                <>
                  <span className="px-1 opacity-60">·</span>
                  {tr(
                    "pkglib.meta.drive",
                    { drive: volumeOfPkgPath(entry.path) ?? "" },
                    "on {drive}",
                  )}
                </>
              )}
              {!entry.sourcePath && onView && (
                <button
                  type="button"
                  onClick={onView}
                  className="ml-2 shrink-0 rounded px-1 text-[var(--color-accent)] hover:underline"
                >
                  {tr("viewer_open", undefined, "View details")}
                </button>
              )}
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
                  {onView && (
                    <button
                      type="button"
                      onClick={onView}
                      className="ml-1 shrink-0 rounded px-1 text-[var(--color-accent)] hover:underline"
                    >
                      {tr("viewer_open", undefined, "View details")}
                    </button>
                  )}
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
              <div
                className="flex min-w-0 items-center gap-1"
                title={entry.path}
              >
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
            {/* After a refusal from the PS5's own storage, Install tries that route again and
                Retry with Stream (when offered) serves the same copy from this engine. */}
            {streamOffered && (
                <Button
                  variant="primary"
                  size="sm"
                  leftIcon={<Download size={13} />}
                  onClick={onRetryStream}
                  disabled={installDisabled}
                  title={tr(
                    "pkglib.retry_stream_hint",
                    undefined,
                    "Send this package from the PS5's own storage through Stream instead. The PS5 refused it the other way (0x80b2116f); the package is not copied again.",
                  )}
                >
                  {tr("pkglib.retry_stream", undefined, "Retry with Stream")}
                </Button>
            )}
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
        <label className="flex cursor-pointer items-start gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-1)] px-2.5 py-2 text-xs">
          <input
            type="checkbox"
            checked={!!selectedForInstallAll}
            onChange={onSelectAlternative}
            disabled={installDisabled}
            className="mt-0.5 accent-[var(--color-accent)]"
          />
          <span className="min-w-0 leading-snug">
            <span
              className={
                selectedForInstallAll
                  ? "font-medium text-[var(--color-accent)]"
                  : "text-[var(--color-text)]"
              }
            >
              {selectedForInstallAll
                ? tr(
                    "pkglib.variant.selected",
                    undefined,
                    "Included in Install all (this PS5)",
                  )
                : tr(
                    "pkglib.variant.select",
                    undefined,
                    "Include in Install all (this PS5)",
                  )}
            </span>
            <span className="mt-0.5 block text-[var(--color-muted)]">
              {tr(
                "pkglib.variant.help",
                undefined,
                "When several packages are the same game update or DLC, pick exactly one for the bulk Install all button. Uncheck to leave it out. Single Install on the row always installs this package only.",
              )}
            </span>
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
              {!uploadFinalizing && uploadRate > 0 && (
                <>
                  {" · "}
                  {formatBytes(uploadRate)}/s
                  {uploadEtaSec !== null && (
                    <>
                      {" · "}
                      {tr(
                        "queue_eta",
                        { eta: formatDuration(uploadEtaSec) },
                        "ETA {eta}",
                      )}
                    </>
                  )}
                </>
              )}
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
          <span>
            {entry.lastResult.message}
            {(!entry.lastResult.ok || entry.lastResult.warn) && (
              <button
                type="button"
                className="ml-1.5 underline underline-offset-2"
                onClick={() =>
                  navigate(
                    entry.lastResult!.ok
                      ? faqLink(DOC_ANCHORS.wontLaunch)
                      : installErrorLink(entry.lastResult!.message),
                  )
                }
              >
                {entry.lastResult.ok
                  ? tr("install_help_wont_launch", undefined, "Game won't launch?")
                  : tr("install_help_what_means", undefined, "What does this mean?")}
              </button>
            )}
          </span>
        </div>
      )}
      {/* Windows: the console could not reach this computer, and the engine knows why. */}
      {!busy && !entry.lastResult?.ok && entry.lastResult?.netDiag && (
        <NetworkFixActions diag={entry.lastResult.netDiag} />
      )}
      {/* A fake PS5 GAME on firmware above 11.60 installs but cannot be
          played. Not shown for a retail-signed package, PS4, or homebrew. */}
      {!installed && entry.authenticity !== "retail" && (
        <FakeGameFirmwareNotice
          compact
          kernel={kernel}
          contentId={entry.contentId || entry.titleId}
        />
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
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  // Per-console store: every selector is scoped to THIS console's host, so the
  // Install Package view is fully isolated per PS5 (parallel installs).
  const entries = usePkgLibrary(host, (s) => s.entries);
  const loading = usePkgLibrary(host, (s) => s.loading);
  const error = usePkgLibrary(host, (s) => s.error);
  const installing = usePkgLibrary(host, (s) => s.installing);
  const installingAll = usePkgLibrary(host, (s) => s.installingAll);
  const busyNotice = usePkgLibrary(host, (s) => s.busyNotice);
  const downloadNotice = usePkgLibrary(host, (s) => s.downloadNotice);
  // Whether this console's queue is running something — its row then carries
  // the live status, and the page-level line would only repeat it.
  const queueRunningHere = useUploadQueueStore((s) =>
    queueItemsForHost(s.items, host).some((it) => it.status === "running"),
  );
  const refresh = usePkgLibrary(host, (s) => s.refresh);
  const addAndUpload = usePkgLibrary(host, (s) => s.addAndUpload);
  const install = usePkgLibrary(host, (s) => s.install);
  const installAll = usePkgLibrary(host, (s) => s.installAll);
  const installStream = usePkgLibrary(host, (s) => s.installStream);
  const retryWithStream = usePkgLibrary(host, (s) => s.retryWithStream);
  // How this console should fetch a link, remembered per host — the right
  // answer follows the link, and two consoles can sit behind different ones.
  const linkMode = useLinkInstallPrefs((s) => s.modeFor(host));
  const linkInsecure = useLinkInstallPrefs((s) => s.insecureFor(host));
  const setLinkMode = useLinkInstallPrefs((s) => s.setMode);
  const setLinkInsecure = useLinkInstallPrefs((s) => s.setInsecure);
  const installUrl = usePkgLibrary(host, (s) => s.installUrl);
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
  // Library rows' queue state, as a string key so this screen re-renders only
  // when a row's state changes, not on every progress tick.
  const libStateKey = useUploadQueueStore((s) =>
    [...libraryInstallStates(s.items, host)].map(([p, v]) => `${p}\u0000${v}`).join("\n"),
  );
  const libStates = useMemo(() => {
    const m = new Map<string, "queued" | "installing">();
    for (const line of libStateKey ? libStateKey.split("\n") : []) {
      const [p, v] = line.split("\u0000");
      m.set(p, v as "queued" | "installing");
    }
    return m;
  }, [libStateKey]);
  // The row whose original file is open in the package viewer.
  const [viewEntry, setViewEntry] = useState<PkgEntry | null>(null);
  const [picking, setPicking] = useState(false);
  const [remoteUrl, setRemoteUrl] = useState("");
  // A link that is a real file but not a package (R4, #368): offered as a download-only.
  const [linkDownload, setLinkDownload] = useState<{ url: string; info: LinkClass } | null>(null);
  const [checkingLink, setCheckingLink] = useState(false);
  const [dropActive, setDropActive] = useState(false);
  const browserPkgInputRef = useRef<HTMLInputElement>(null);
  const [alternativeSelections, setAlternativeSelections] =
    useState<PkgAlternativeSelections>(() =>
      loadPkgAlternativeSelections(host),
    );

  const hostReady = !!host?.trim();
  // There is no reliable firmware cutoff for Stream install, and no beta gate
  // either: it is the primary path. Hardware across FW 5.10 and FW 9.60 ran
  // 9/9 successful stream installs (16 MB to 3.56 GB) while the staged
  // Upload -> Install path failed 0/3 on the same consoles and packages. A
  // console can still reject a stream at Sony's HTTP proxy layer, so the real
  // network error is surfaced when that happens rather than pre-emptively
  // disabling capable consoles by version.
  useEffect(() => {
    setAlternativeSelections(loadPkgAlternativeSelections(host));
  }, [host]);
  // Stable ref so the drag-drop effect (subscribed once per host) always
  // calls the latest uploader without re-subscribing each render. Updated
  // in an effect (not during render) per the rules-of-hooks ref rule.
  const uploadRef = useRef<(p: string) => void>(() => {});
  const dropRef = useRef<(p: string) => void>(() => {});
  const recentPkgDrops = useRef(new Map<string, number>());
  useEffect(() => {
    // The window-level drop listener is registered outside JSX, so the
    // ConnectionGate cannot disarm it — an offline drop has to be refused here
    // or it starts a staging run against a console that isn't answering.
    uploadRef.current = (p: string) => {
      if (
        hostReady &&
        payloadStatus === "up" &&
        acceptPkgDrop(recentPkgDrops.current, p)
      ) {
        void addAndUpload(p, host);
      }
    };
    // A dropped package streams & installs — the preferred path: nothing is
    // copied to the PS5 first. Upload & install stays on its own button.
    dropRef.current = (p: string) => {
      if (
        hostReady &&
        payloadStatus === "up" &&
        acceptPkgDrop(recentPkgDrops.current, p)
      ) {
        void runStreamInstall(p, p.split(/[\\/]/).pop() ?? p);
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
      dropRef.current(dropped);
      window.history.replaceState({}, "");
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [location.key]);

  // Webview drag-drop — accept install packages, but keep mountable UFS images
  // (.ffpkg/.ffpfs) in the File System flow.
  useEffect(() => {
    if (!host) return;
    // A dropped file is a path on this device; a remote engine can't open it.
    if (!engineIsOnThisDevice()) return;
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
        const pkgPaths = paths.filter(isInstallPackagePath);
        if (paths.length > 0 && pkgPaths.length === 0) {
          setPickError(
            tr(
              "install.error.notPkg",
              "Only .pkg or .fpkg install packages can be installed here. .ffpkg / .ffpfs are mountable images — open them from the File System tab instead.",
            ),
          );
          return;
        }
        setPickError(null);
        for (const x of pkgPaths) dropRef.current(x);
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
            filters: [{ name: "PlayStation Package", extensions: ["pkg", "fpkg"] }],
          })
        : await openDialog({
            multiple: true,
            filters: [{ name: "PlayStation Package", extensions: ["pkg", "fpkg"] }],
          });
      const paths = Array.isArray(sel) ? sel : sel ? [sel] : [];
      for (const pth of paths) uploadRef.current(pth as string);
    } catch (e) {
      setPickError(`${e}`);
    } finally {
      setPicking(false);
    }
  }

  // Stream & install (#81): pick a single PC-side .pkg and install it
  // WITHOUT staging it on the PS5 first — the engine serves the file over
  // HTTP and the DPI daemon pulls it directly. Useful for a quick one-shot
  // install when you don't want to wait out the staging upload (or don't
  // have the disk space for it). Shares the `installing` lock with the
  // regular install flow.
  async function runStreamInstall(
    sourcePath: string,
    streamName: string,
    opts?: { ephemeral?: boolean },
  ) {
    const r = await installStream(sourcePath, host);
    // The outcome lives on the item's queue row — including its error and
    // Retry via upload. Only a browser upload's row is dropped on failure (its
    // staged copy is deleted, so it could never be retried): say it here.
    if (!r.ok && opts?.ephemeral) {
      const q = useUploadQueueStore.getState();
      for (const id of failedStreamInstallIds(q.items, sourcePath)) q.remove(id);
      setPickError(
        `${streamName}: ${
          r.message ||
          tr("install.stream.failed", undefined, "The install didn't complete.")
        }`,
      );
    }
  }

  async function handleBrowserStreamFile(file: File) {
    setPickError(null);
    let uploadId: string | null = null;
    try {
      const staged = await stageBrowserPkg(file);
      uploadId = staged.uploadId;
      await runStreamInstall(staged.path, staged.filename || file.name, { ephemeral: true });
    } catch (e) {
      setPickError(`${e}`);
    } finally {
      if (uploadId) {
        // Best effort: a stale-upload sweep also catches browser crashes.
        await deleteBrowserPkgUpload(uploadId).catch(() => {});
      }
    }
  }

  /** A package picked on a saved server: the engine streams it straight to the console. */
  async function streamFromServer(path: string) {
    setPickError(null);
    if (!host?.trim()) {
      setPickError(tr("install.error.noHost", "Set a PS5 host on the Connection tab first."));
      return;
    }
    if (!isInstallPackagePath(path)) {
      setPickError(tr("pkglib.stream.notPkg", undefined, "Pick a .pkg or .fpkg file."));
      return;
    }
    try {
      await runStreamInstall(path, path.split("/").pop() ?? path);
    } catch (e) {
      setPickError(`${e}`);
    }
  }

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
    if (!engineIsOnThisDevice()) {
      // The web UI — or the desktop app pointed at a remote engine — runs
      // against a server (often a NAS or Docker host) that already holds the
      // library. Browse that disk, exactly like Upload
      // does, so a 50 GB package is served in place instead of being
      // uploaded from the viewer's device first. "From this device" keeps
      // the old upload path.
      const picked = await pickLocalPath({
        mode: "file",
        title: tr(
          "pkglib.stream.pickServer",
          undefined,
          "Choose a package on the server",
        ),
      });
      if (!picked) return;
      if (!isInstallPackagePath(picked)) {
        setPickError(
          tr("pkglib.stream.notPkg", undefined, "Pick a .pkg or .fpkg file."),
        );
        return;
      }
      try {
        const name = picked.split(/[\\/]/).pop() ?? picked;
        await runStreamInstall(picked, name);
      } catch (e) {
        setPickError(`${e}`);
      }
      return;
    }
    // No beta gate. Stream is the RELIABLE path and is no longer hidden
    // behind a scary confirm.
    //
    // Measured across both test consoles (FW 9.60 and FW 5.10), packages from
    // 16 MB to 3.56 GB: Stream 9/9 succeeded; staged Upload -> Install 0/3,
    // and one failed staged install REMOVED the working game it was
    // replacing (Sony's installer clears the old title before writing the
    // new one, so a failure leaves nothing). The old dialog told users the
    // exact opposite — "if it fails, use Upload -> Install, that path is
    // reliable on all firmwares" — which pointed them at the destructive
    // route. Keeping a `destructive: true` confirm on the safe path while
    // the unsafe one was one click away was the wrong way round.
    try {
      const sel = isAndroid()
        ? await pickPath({
            mode: "file",
            filters: [{ name: "PlayStation Package", extensions: ["pkg", "fpkg"] }],
          })
        : await openDialog({
            multiple: false,
            filters: [{ name: "PlayStation Package", extensions: ["pkg", "fpkg"] }],
          });
      const p = Array.isArray(sel) ? sel[0] : sel;
      if (!p) return;
      const sourcePath = String(p);
      const streamName = sourcePath.split(/[\\/]/).pop() ?? sourcePath;
      await runStreamInstall(sourcePath, streamName);
    } catch (e) {
      setPickError(`${e}`);
    }
  }

  async function handleUrlInstall() {
    setPickError(null);
    setLinkDownload(null);
    const link = remoteUrl.trim();
    // Decide by what the link serves, after its redirects: a package installs, another real
    // file is download-only, and a page / error / login / empty body is refused with the
    // reason. The URL's spelling decides nothing (R4, #368). A probe that cannot be made
    // (offline from here, an old engine) falls through: the install reports its own errors.
    setCheckingLink(true);
    const info: LinkClass | null = await linkProbe(link, linkInsecure)
      .catch(() => null)
      .finally(() => setCheckingLink(false));
    if (info?.kind === "refused") {
      setPickError(
        info.message ??
          tr("linkdl.refused", undefined, "That link is not a file download."),
      );
      return;
    }
    if (info?.kind === "file") {
      setLinkDownload({ url: link, info });
      return;
    }
    const approved = await confirm({
      title: tr("pkglib.url.confirmTitle", "Start experimental link install?"),
      message: tr("pkglib.url.confirmBody", "This computer downloads the package from the link and feeds it to the PS5, so it must stay awake and connected until the install finishes. Reinstalling over an existing title may remove it if Sony's installer fails. Use only a trusted package URL you are authorized to install."),
      confirmLabel: tr("pkglib.url.confirm", "Start install"),
      cancelLabel: tr("pkglib.stream.fallback.cancel", "Not now"),
    });
    if (!approved) return;
    const startedAt = Date.now();
    try {
      const result = await installUrl(link, host, {
        mode: linkMode,
        // The name the link ended up with (a redirect or Content-Disposition), for the row.
        displayName: info?.filename,
      });
      // A link that reached the queue (as itself, or as the file a
      // download-first produced) reports on its row. One that never got there
      // — a malformed link, a failed download — has only this line.
      const hasRow = queueItemsForHost(useUploadQueueStore.getState().items, host).some(
        (it) =>
          it.sourceKind === "install" &&
          it.status === "failed" &&
          (it.completedAt ?? 0) >= startedAt,
      );
      if (!result.ok && !hasRow) {
        setPickError(
          result.message ??
            tr("pkglib.url.failed", "The link install didn't complete."),
        );
      }
    } catch (e) {
      setPickError(`${e}`);
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
    } else if (entry.category === "gp" && entry.titleId) {
      // The base IS installed — but if it did not come from a base package we
      // can account for, the console may not be able to match this update to
      // it. Sony's installer then reports success and copies nothing, leaving
      // the game on its old version with no error. Hardware-confirmed: the
      // same update did nothing over a pre-existing base and applied in about
      // 150 s once the base was re-installed from its matching package.
      //
      // We warn rather than block: we can only tell that we cannot CONFIRM a
      // matching base, not that the update will definitely fail.
      const baseAccountedFor = entries.some(
        (x) =>
          x.titleId === entry.titleId &&
          (x.category === "gd" || x.category === undefined) &&
          x.installedHere,
      );
      if (!baseAccountedFor) {
        const ok = await confirm({
          title: tr(
            "pkglib.updateBaseUnknown.title",
            undefined,
            "This update might not apply",
          ),
          message: tr(
            "pkglib.updateBaseUnknown.body",
            { id: entry.titleId },
            `${entry.titleId} is installed, but not from a base package ps5upload installed — so the PS5 may not be able to match this update to it. When that happens the console reports success and silently changes nothing. If the game stays on its old version afterwards, install the matching base package through ps5upload (choose Override), then apply this update again.`,
          ),
          confirmLabel: tr(
            "pkglib.updateBaseUnknown.installAnyway",
            undefined,
            "Install anyway",
          ),
        });
        if (!ok) return;
      }
    }
    // Manually installing an alternative is an explicit choice. Remember it
    // for this console so a later Install all never replaces it with a sibling
    // merely because that sibling appears later in the list.
    chooseAlternative(entry);
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
      // An explicit checkbox opt-out must win over auto-detection of the
      // currently installed artifact. Without a sentinel, deleting the saved
      // choice simply selected the same row again on the next render.
      if (alternativeSelections[group.key] === PKG_ALTERNATIVE_SKIP) {
        delete next[group.key];
        continue;
      }
      const identities = new Set(
        group.entries.map((entry) => pkgEntryIdentity(entry)),
      );
      if (next[group.key] && identities.has(next[group.key])) continue;
      const active = group.entries.filter((entry) =>
        pkgRowInstalled(
          entry,
          installedIds,
          entry.titleId ? installedArtifacts.get(entry.titleId) : undefined,
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

  function chooseAlternative(entry: PkgEntry) {
    const key = alternativeKeyByPath.get(entry.path);
    if (!key) return;
    const identity = pkgEntryIdentity(entry);
    recordPkgAlternativeSelection(host, key, identity);
    setAlternativeSelections((current) => ({
      ...current,
      [key]: identity,
    }));
  }

  function toggleAlternative(entry: PkgEntry) {
    const key = alternativeKeyByPath.get(entry.path);
    if (!key) return;
    const identity = pkgEntryIdentity(entry);
    const already =
      effectiveAlternativeSelections[key] === identity ||
      alternativeSelections[key] === identity;
    if (already) {
      // Checkbox toggle-off: leave the group unselected so Install all
      // skips the conflict until the user picks again.
      skipPkgAlternativeSelection(host, key);
      setAlternativeSelections((current) => {
        return { ...current, [key]: PKG_ALTERNATIVE_SKIP };
      });
      return;
    }
    chooseAlternative(entry);
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
            entry.titleId ? installedArtifacts.get(entry.titleId) : undefined,
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
          title: entry.title || entry.titleId || entry.contentId || entry.name,
          titleId: entry.titleId,
          entries: [entry],
        });
      }
    }
    return [...grouped.values()];
  }, [entries]);
  // Installs go into the console's queue, which runs them one at a time, so a
  // busy console never blocks the Install button — the click just queues.
  const installBlocked = false;

  const renderPkgRow = (entry: PkgEntry) => {
    // A row whose install is waiting or running in the console queue shows it
    // (and can't be deleted from under the queued install).
    const queued = libStates.get(entry.path);
    if (queued && entry.status === "idle") entry = { ...entry, status: queued };
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
        deleteDisabled={installing || !!queued}
        alternativeKey={alternativeKey}
        selectedForInstallAll={
          !!alternativeKey &&
          effectiveAlternativeSelections[alternativeKey] ===
            pkgEntryIdentity(entry)
        }
        onSelectAlternative={
          alternativeKey ? () => toggleAlternative(entry) : undefined
        }
        onInstall={() => void handleInstall(entry)}
        onRetryStream={() => void retryWithStream(entry.path, host)}
        onDelete={() => void handleDelete(entry)}
        onView={() => setViewEntry(entry)}
      />
    );
  };

  return (
    <div className="app-page">
      <PackagePanel
        path={
          viewEntry
            ? // The original on this computer reads fastest; otherwise the staged copy on the
              // console, read over its FTP server.
              viewEntry.sourcePath && !isRemotePath(viewEntry.sourcePath)
              ? viewEntry.sourcePath
              : `ps5://${hostOf(host)}${viewEntry.path}`
            : null
        }
        host={host}
        onClose={() => setViewEntry(null)}
        actions={
          viewEntry
            ? [
                {
                  label: tr("pkglib.install", undefined, "Install"),
                  primary: true,
                  onClick: () => {
                    const e = viewEntry;
                    setViewEntry(null);
                    void handleInstall(e);
                  },
                },
              ]
            : []
        }
      />
      <PageHeader
        icon={PackageOpen}
        title={tr("install.title", "Install Package")}
        count={entries.length || undefined}
        loading={loading}
        description={tr(
          "install.description.stream",
          "Stream & install sends a .pkg or .fpkg straight from this computer to your PS5 — nothing is copied first, and it's the most reliable way to install. Upload & install copies it to the PS5 first, for when the console can't reach this computer; those copies stay listed here so you can reinstall any time.",
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
                disabled={!hostReady || installingAll}
                title={
                  !hostReady
                    ? tr(
                        "install.add.disabledHint",
                        "Set a PS5 host on the Connection tab first",
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

            {!engineIsOnThisDevice() && (
              <input
                ref={browserPkgInputRef}
                type="file"
                accept=".pkg,.fpkg"
                className="hidden"
                onChange={(event) => {
                  const file = event.currentTarget.files?.[0];
                  // Selecting the same file again must fire another change.
                  event.currentTarget.value = "";
                  if (file) void handleBrowserStreamFile(file);
                }}
              />
            )}
            {!engineIsOnThisDevice() && (
              <Button
                variant="ghost"
                size="sm"
                onClick={() => browserPkgInputRef.current?.click()}
                disabled={!hostReady}
                title={tr(
                  "pkglib.stream.fromDevice.hint",
                  undefined,
                  "Upload a package from the device this browser is running on, then stream it.",
                )}
              >
                {tr("pkglib.stream.fromDevice", undefined, "From this device")}
              </Button>
            )}
            <BrowseButton
              mode="file"
              remote
              primary
              icon={<Download size={14} />}
              filters={[{ name: "PlayStation Package", extensions: ["pkg", "fpkg"] }]}
              label={tr("pkglib.streamInstall", undefined, "Stream & install")}
              // Installs queue per console, so a running install never
              // blocks picking the next one.
              disabled={!hostReady}
              tooltip={
                !hostReady
                  ? tr(
                      "install.add.disabledHint",
                      "Set a PS5 host on the Connection tab first",
                    )
                  : tr(
                      "pkglib.stream.hint",
                      "Install a .pkg straight from this PC over HTTP — no staging upload. The most reliable path.",
                    )
              }
              onMainClick={() => void handleStreamPick()}
              onPick={(p) => void streamFromServer(p)}
            />
            {isTauriEnv() && (
              <Button
                variant="secondary"
                size="sm"
                leftIcon={<Upload size={14} />}
                onClick={handlePick}
                loading={picking}
                disabled={!hostReady}
                title={
                  !hostReady
                    ? tr(
                        "install.add.disabledHint",
                        "Set a PS5 host on the Connection tab first",
                      )
                    : tr(
                        "install.uploadInstall.hint",
                        "Copy the package to the PS5 first, then install it — for when the console can't reach this computer.",
                      )
                }
              >
                {tr("install.uploadInstall", "Upload & install")}
              </Button>
            )}
          </div>
        }
      />

      {/* This console's queue first: what is installing or waiting is the
          thing you came back to this screen to see. */}
      {hostReady && <QueuePanel host={host} />}

      <ConnectionGate require="payload">
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

        {/* A queued install shows its own progress on its queue row, so this
            line appears only for work the queue doesn't represent. */}
        {busyNotice && !queueRunningHere && (
          <div className="mb-4 flex items-center justify-between gap-3 rounded-lg border border-[var(--color-accent)] bg-[var(--color-accent-soft)] px-4 py-3 text-sm">
            <div className="flex items-start gap-2">
              <Spinner size={14} tone="accent" className="mt-0.5 shrink-0" />
              <span>{busyNotice}</span>
            </div>
          </div>
        )}
        {/* "Download through this computer" runs before its install joins
            the queue, so it reports here, next to the queue it will join. */}
        {downloadNotice && (
          <div className="mb-4 flex items-start gap-2 rounded-lg border border-[var(--color-accent)] bg-[var(--color-accent-soft)] px-4 py-3 text-sm">
            <Spinner size={14} tone="accent" className="mt-0.5 shrink-0" />
            <span>{downloadNotice}</span>
          </div>
        )}

        <div className="mb-4 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3">
          <label htmlFor="pkg-remote-url" className="block text-sm font-medium text-[var(--color-text)]">
            {tr("pkglib.url.title", "Install from HTTP(S) link")}
          </label>
          <p className="my-1 text-xs text-[var(--color-muted)]">
            {tr("pkglib.url.help", "This computer downloads the package over several connections at once and feeds it to the PS5 on your network, so the transfer runs at your line speed rather than the console's slower single stream. Nothing is staged on either machine, so a 100 GB game needs no spare space. The link must be a direct download that supports byte ranges. Keep this computer awake until the install finishes.")}
          </p>
          <fieldset className="my-2 space-y-1.5">
            <legend className="sr-only">
              {tr("pkglib.url.title", "Install from HTTP(S) link")}
            </legend>
            {(["direct", "stream", "download"] as const).map((m) => (
              <label key={m} className="flex items-start gap-2 text-xs">
                <input
                  type="radio"
                  name="link-install-mode"
                  className="mt-0.5"
                  checked={linkMode === m}
                  onChange={() => setLinkMode(host, m)}
                />
                <span>
                  <span className="text-[var(--color-text)]">
                    {tr(`pkglib.url.mode.${m}`)}
                  </span>
                  <span className="block text-[var(--color-muted)]">
                    {tr(`pkglib.url.mode.${m}_hint`)}
                  </span>
                </span>
              </label>
            ))}
            <label className="flex items-start gap-2 text-xs">
              <input
                type="checkbox"
                className="mt-0.5"
                // Only meaningful when THIS computer does the downloading: in
                // direct mode the console runs its own TLS handshake and we
                // have no say in what it accepts. Disabled rather than hidden
                // so the reason can be read.
                checked={linkInsecure && linkMode !== "direct"}
                disabled={linkMode === "direct"}
                onChange={(e) => setLinkInsecure(host, e.currentTarget.checked)}
              />
              <span className={linkMode === "direct" ? "opacity-60" : undefined}>
                <span className="text-[var(--color-text)]">
                  {tr("pkglib.url.insecure")}
                </span>
                <span className="block text-[var(--color-muted)]">
                  {tr("pkglib.url.insecure_hint")}
                </span>
              </span>
            </label>
          </fieldset>
          <div className="flex flex-wrap gap-2">
            <input
              id="pkg-remote-url"
              type="url"
              value={remoteUrl}
              onChange={(event) => setRemoteUrl(event.currentTarget.value)}
              placeholder="https://example.com/game.pkg"
              aria-label={tr("pkglib.url.label", "Direct package URL")}
              className="min-w-52 flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-1)] px-2 py-1.5 text-sm text-[var(--color-text)]"
            />
            <Button variant="secondary" size="sm" onClick={handleUrlInstall}
              disabled={!hostReady || !remoteUrl.trim() || checkingLink}>
              {checkingLink
                ? tr("linkdl.checking", undefined, "Checking link…")
                : tr("pkglib.url.install", "Install link")}
            </Button>
          </div>
        </div>
        {linkDownload && (
          <LinkDownloadCard
            host={host}
            url={linkDownload.url}
            info={linkDownload.info}
            insecureTls={linkInsecure}
            onClose={() => setLinkDownload(null)}
          />
        )}
        {hostReady && <RarPackagesCard host={host} />}
        {hostReady && <ExternalPackages host={host} />}
        {/* Workflow options, grouped near the top where they're set before
            adding a package (not buried under the library list). Both govern the
            hands-off "add → installed → cleaned up" flow, so they read together. */}
        <div className="mb-4 flex flex-col gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3">
          <span className="text-xs font-medium uppercase tracking-wide text-[var(--color-muted)]">
            {tr("pkglib.options.heading", "Options")}
          </span>
          <Toggle
            checked={autoInstall}
            onChange={setAutoInstall}
            label={tr(
              "pkglib.autoInstall",
              undefined,
              "Install automatically once the upload finishes",
            )}
          />
          <Toggle
            checked={autoRemove}
            onChange={setAutoRemove}
            label={tr(
              "pkglib.autoRemove",
              undefined,
              "Auto-delete each package from the PS5 after it installs",
            )}
          />
        </div>

        {/* Reference notes, below the controls they explain: read once,
            then out of the way. */}
        <div className="mb-4 flex items-start gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 text-[12px] text-[var(--color-muted)]">
          <Info size={13} className="mt-0.5 shrink-0" />
          <div>
            <span className="font-medium text-[var(--color-text)]">
              {tr("pkglib.installnote.title", "How installing works")}
            </span>
            {" — "}
            {tr(
              "pkglib.installnote.body",
              "ps5upload hands the package to its own installer on the PS5 and only reports it installed once the console has read the whole package; a patch must also raise the game's version. If an install fails, the package is kept so you can retry. FW 12+ and system packages may still require the PS5's own Settings → System → Debug Settings → Game → Package Installer.",
            )}
          </div>
        </div>

        {/* Stated, not detected. A user installing an FPKG already knows it is
            one, and there is no header marker that separates a fake package from
            a retail one — every field that looked like a candidate is present on
            genuine Sony packages too. So the note names the requirement and lets
            the reader decide whether it applies to them. */}
        {/* A disclosure, not a standing callout: it matters only to someone
            installing a fake package, and they recognise the question. */}
        <details className="group mb-4 rounded-md border border-[var(--color-warn)]/40 bg-[var(--color-warn)]/5 text-[12px] text-[var(--color-muted)]">
          <summary className="flex cursor-pointer list-none items-center gap-2 px-3 py-2 font-medium text-[var(--color-warn)] [&::-webkit-details-marker]:hidden">
            <AlertTriangle size={13} className="shrink-0" />
            <span className="flex-1">
              {tr(
                "pkglib.fpkgsupport.title",
                "Installing a fake package (FPKG)?",
              )}
            </span>
            <ChevronDown
              size={14}
              className="shrink-0 transition-transform group-open:rotate-180"
            />
          </summary>
          <div className="px-3 pb-3">
            <div className="flex flex-col gap-1.5">
              <div>
                {tr(
                  "pkglib.fpkgsupport.lead",
                  "A PS5 fake package only installs when fake-package support is already loaded on the console. Load all three, in this order, before installing:",
                )}
              </div>
              <ol className="ml-4 flex list-decimal flex-col gap-1">
                <li>
                  <strong className="text-[var(--color-text)]">kstuff</strong>{" "}
                  {tr(
                    "pkglib.fpkgsupport.kstuff",
                    "— the build with PS5 fake-package support",
                  )}
                </li>
                <li>
                  <strong className="text-[var(--color-text)]">
                    a53_ppr_install_fast.elf
                  </strong>{" "}
                  {tr(
                    "pkglib.fpkgsupport.ppr",
                    "— applies the PPR plaintext / no-auth patch",
                  )}
                </li>
                <li>
                  <strong className="text-[var(--color-text)]">
                    shadowmountplus.elf
                  </strong>{" "}
                  {tr(
                    "pkglib.fpkgsupport.smp",
                    "— the mount layer that registers the installed title",
                  )}
                </li>
              </ol>
              <div>
                {tr(
                  "pkglib.fpkgsupport.tail",
                  "Without them the console refuses the install, or takes it and then fails to mount the game. Retail and debug packages need none of this — if that is what you are installing, ignore this note.",
                )}
              </div>
            </div>
          </div>
        </details>

        {alternativeGroups.length > 0 && (
          <div className="mb-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] px-4 py-3 text-sm leading-relaxed text-[var(--color-muted)]">
            <strong className="text-[var(--color-text)]">
              {tr("pkglib.installAll.what", undefined, "What is Install all?")}
            </strong>{" "}
            {tr(
              "pkglib.installAll.whatBody",
              undefined,
              "Install all runs every ready package in order: base game → updates → DLC. When you have two updates of the same version (or two DLC packs that conflict), use the checkbox on each row to choose which one this console should install — only one per group. Uncheck to leave that group out. The Install button on a single row always installs just that package.",
            )}
          </div>
        )}


        {hostReady && entries.length === 0 && !loading ? (
          <EmptyState
            icon={dropActive ? HardDrive : PackageOpen}
            size="hero"
            title={
              dropActive
                ? tr("pkglib.empty.drop.stream", "Drop to stream & install")
                : tr("pkglib.empty.title.stream", "Install a package")
            }
            message={tr(
              "pkglib.empty.body.stream",
              "Use Stream & install to install a .pkg or .fpkg straight from this computer, or drop one onto the window. Packages you copy over with Upload & install are listed here so you can reinstall them.",
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
      </ConnectionGate>
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
          "Plug a USB stick or external drive with .pkg or .fpkg install packages into the PS5 and they show up here — no upload needed. Installing copies the file onto the console first (your drive's copy is left untouched), then installs it. Use Scan after connecting a drive.",
        )}
      </div>
      <Toggle
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
            "Auto-scan is off. Connect a USB or external drive with .pkg or .fpkg install packages, then click Scan.",
          )}
        </div>
      ) : pkgs.length === 0 ? (
        // Empty state — informative, and the Scan button above stays put.
        <div className="rounded-md border border-dashed border-[var(--color-border)] px-3 py-4 text-xs text-[var(--color-muted)]">
          {tr(
            "pkglib.external.empty",
            "No .pkg or .fpkg install packages found on connected USB or external drives. Connect a drive that has packages on it, then click Scan.",
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
                  {/* Install result on its own line with an icon: success,
                      a may-not-launch caution, or a failure. */}
                  {r && (
                    <div
                      className="mt-1 flex items-center gap-1.5 text-xs font-medium"
                      style={{
                        color: r.ok
                          ? r.mayNotLaunch
                            ? "var(--color-warn)"
                            : "var(--color-good)"
                          : "var(--color-bad)",
                      }}
                    >
                      {r.ok ? (
                        r.mayNotLaunch ? (
                          <AlertTriangle size={13} className="shrink-0" />
                        ) : (
                          <CheckCircle2 size={13} className="shrink-0" />
                        )
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
                  // Each click queues; only this package's own install locks it.
                  disabled={installingPath === p.path}
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
