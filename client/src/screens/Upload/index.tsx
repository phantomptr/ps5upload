import { useEffect, useMemo, useState } from "react";
import {
  Upload as UploadIcon,
  FolderOpen,
  FileIcon,
  HardDrive,
  Gamepad2,
  FileArchive,
  Loader2,
  Info,
  X,
  Plus,
  type LucideIcon,
} from "lucide-react";
import clsx from "clsx";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { isTauriEnv, safeUnlisten } from "../../lib/tauriEnv";
import { convertFileSrc } from "@tauri-apps/api/core";

import {
  useUploadStore,
  type ExcludeMode,
  type PickedSource,
  type SourceKind,
} from "../../state/upload";
import {
  fetchVolumes,
  pathKind,
  probeDestination,
  type PlannedFile,
  type Volume,
  type ZipInspect,
} from "../../api/ps5";
import {
  useTransferStore,
  type TransferPhase,
  type UploadStrategy,
} from "../../state/transfer";
import { useConnectionStore, PS5_PAYLOAD_PORT } from "../../state/connection";
import { pushNotification } from "../../state/notifications";
import { useRosterStore } from "../../state/roster";
import { useNavigate } from "react-router-dom";
import { PageHeader, WarningCard, Button } from "../../components";
import FfpkgInspectorPanel from "./FfpkgInspectorPanel";
import FolderDiffPanel from "./FolderDiffPanel";
import { useUploadSettingsStore } from "../../state/uploadSettings";
import { useUploadQueueStore } from "../../state/uploadQueue";
import { useRecentHostMetricsStore } from "../../state/recentHostMetrics";
import {
  computeUploadEta,
  formatEtaSeconds,
  pickBannerMode,
} from "../../lib/uploadEta";
import { resolveUploadDest } from "../../lib/uploadDest";
import { QueuePanel } from "./QueuePanel";
import { humanizePs5Error } from "../../lib/humanizeError";
import { formatBytes } from "../../lib/format";
import { useTr } from "../../state/lang";

// formatBytes moved to lib/format.ts.

/** Map discriminated SourceKind to its one-line "Detected: ..." string. */
function detectedLabel(source: PickedSource): { icon: LucideIcon; label: string } {
  switch (source.kind) {
    case "file":
      return { icon: FileIcon, label: "Plain file" };
    case "image": {
      // Show the actual extension in the label so users know whether
      // it's exFAT (.exfat) or UFS2 (.ffpkg) — both are "disk images"
      // to us but require different fstype args to nmount.
      const ext = source.path.toLowerCase().endsWith(".ffpkg")
        ? ".ffpkg"
        : ".exfat";
      return { icon: FileArchive, label: `Disk image (${ext})` };
    }
    case "folder":
      return { icon: FolderOpen, label: "Folder" };
    case "game-folder":
      return { icon: Gamepad2, label: "Game folder" };
    case "archive":
      return { icon: FileArchive, label: "Compressed dump (.zip)" };
  }
}

export default function UploadScreen() {
  const tr = useTr();
  const store = useUploadStore();
  const {
    source,
    detecting,
    detectError,
    zipInspectEntries,
    mountAfterUpload,
    mountReadOnly,
    destinationVolume,
    destinationSubpath,
    excludeMode,
    excludes,
    pickFile,
    pickFolder,
    reset,
    setMountAfterUpload,
    setMountReadOnly,
    setDestination,
    setExcludeMode,
    toggleExclude,
    addExclude,
    removeExclude,
  } = store;

  const [dropActive, setDropActive] = useState(false);

  // Subscribe to the webview's drag-drop events. Tauri delivers the paths
  // already resolved to the host filesystem, so we only need to stat one
  // to decide file-vs-folder and route to the right picker.
  //
  // Cleanup covers two races:
  //   • `onDragDropEvent` returns a promise; if the component unmounts
  //     before it resolves, `unlisten` would still be null when the
  //     cleanup fn runs and the listener would leak. The `cancelled`
  //     flag lets the resolution chain unregister immediately when the
  //     promise finally fires post-unmount.
  //   • The event callback does `await pathKind(...)`; if the user
  //     navigates away mid-await and Upload remounts later, the stale
  //     callback could touch the store for the wrong screen. Short-
  //     circuit on `cancelled` at entry.
  useEffect(() => {
    if (!isTauriEnv()) return; // browser dev/test contexts skip Tauri-only APIs
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    const p = getCurrentWebview().onDragDropEvent(async (e) => {
      if (cancelled) return;
      if (e.payload.type === "enter" || e.payload.type === "over") {
        setDropActive(true);
      } else if (e.payload.type === "leave") {
        setDropActive(false);
      } else if (e.payload.type === "drop") {
        setDropActive(false);
        const first = e.payload.paths?.[0];
        if (!first) return;
        const kind = await pathKind(first);
        if (cancelled) return;
        if (kind === "folder") await pickFolder(first);
        else if (kind === "file") await pickFile(first);
      }
    });
    p.then((fn) => {
      if (cancelled) safeUnlisten(fn);
      else unlisten = fn;
    }).catch(() => {
      // onDragDropEvent's Promise can reject if the webview is gone
      // before subscription completes. Silently ignore — there's no
      // listener to unregister.
    });
    return () => {
      cancelled = true;
      if (unlisten) safeUnlisten(unlisten);
    };
  }, [pickFile, pickFolder]);

  const handleChooseFile = async () => {
    const selected = await openDialog({ directory: false, multiple: false });
    if (typeof selected === "string") await pickFile(selected);
  };

  const handleChooseFolder = async () => {
    const selected = await openDialog({ directory: true, multiple: false });
    if (typeof selected === "string") await pickFolder(selected);
  };

  const host = useConnectionStore((s) => s.host);
  const transferPhase = useTransferStore((s) => s.phase);
  const startTransfer = useTransferStore((s) => s.start);
  const resetTransfer = useTransferStore((s) => s.reset);
  const alwaysOverwrite = useUploadSettingsStore((s) => s.alwaysOverwrite);
  const reconcileMode = useUploadSettingsStore((s) => s.reconcileMode);
  const queueAdd = useUploadQueueStore((s) => s.add);
  const activeExcludes =
    excludeMode === "rules"
      ? excludes.filter((rule) => rule.enabled).map((rule) => rule.pattern)
      : [];

  /** Snapshot the current source + destination + options into a queue
   *  item. Captured at click time, so subsequent edits to the form
   *  don't bleed into the queued item. */
  const handleAddToQueue = (strategy: "overwrite" | "resume") => {
    if (!source || !host?.trim()) return;
    const { dest } = resolveUploadDest(
      destinationVolume,
      destinationSubpath,
      source.path,
      source.kind === "archive",
    );
    const addr = `${host}:${PS5_PAYLOAD_PORT}`;
    const displayName =
      source.path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? source.path;
    queueAdd({
      sourceKind: source.kind,
      sourcePath: source.path,
      displayName,
      resolvedDest: dest,
      addr,
      strategy,
      reconcileMode,
      excludes: activeExcludes,
      mountAfterUpload: source.kind === "image" && mountAfterUpload,
      mountReadOnly,
    });
  };

  const [pending, setPending] = useState<null | {
    dest: string;
    addr: string;
    entryCount: number;
    isFolder: boolean;
  }>(null);
  // Separate from transferPhase — covers the pre-flight window (probe
  // in flight, dialog not yet open) where phase is still "idle" but
  // the button shouldn't accept a second click.
  const [preflightBusy, setPreflightBusy] = useState(false);
  // Preflight probe can fail (engine down, PS5 unreachable, port refused);
  // before the catch was added the button silently de-busied and nothing
  // happened. Surface the error inline so the user knows why the upload
  // dialog never appeared. We clear on source change (useEffect below)
  // because otherwise a "preflight failed for /old/path" banner sticks
  // around after the user picks a fresh source — misleading.
  const [preflightError, setPreflightError] = useState<string | null>(null);

  // Live list of writable PS5 volumes for the destination dropdown.
  // Refreshed when the host changes; previously the dropdown was a
  // hardcoded `/data /mnt/ext0 /mnt/usb0` list which hid every other
  // mount point (e.g. `/mnt/ext1`, `/mnt/usbN` for N>0, and any
  // ps5upload-mounted images).
  const [availableVolumes, setAvailableVolumes] = useState<Volume[]>([]);
  // Clear preflight error whenever the source changes (new file/folder
  // pick, or source cleared). The error is tied to the OLD source's
  // destination probe and would otherwise stick around as a misleading
  // banner over an entirely different upload.
  useEffect(() => {
    setPreflightError(null);
  }, [source?.path, source?.kind]);
  useEffect(() => {
    if (!host?.trim()) {
      setAvailableVolumes([]);
      return;
    }
    let cancelled = false;
    const addr = `${host}:${PS5_PAYLOAD_PORT}`;
    fetchVolumes(addr)
      .then((vols) => {
        if (cancelled) return;
        // Only writable, non-placeholder volumes are valid upload
        // targets. Placeholders are mount-points the payload reports
        // even when nothing is mounted there.
        setAvailableVolumes(
          vols.filter((v) => v.writable && !v.is_placeholder),
        );
      })
      .catch(() => {
        if (!cancelled) setAvailableVolumes([]);
      });
    return () => {
      cancelled = true;
    };
  }, [host]);

  const beginUpload = async (strategy: UploadStrategy) => {
    if (!source) return;
    if (!pending) return;
    setPending(null);
    await startTransfer({
      sourceKind: source.kind,
      srcPath: source.path,
      dest: pending.dest,
      addr: pending.addr,
      strategy,
      reconcileMode,
      excludes: activeExcludes,
      mountAfterUpload: source.kind === "image" && mountAfterUpload,
      mountReadOnly,
    });
  };

  const handleUpload = async () => {
    // detectError set = source couldn't be inspected (corrupt/unreadable
    // zip, failed folder walk). Don't ship a half-detected source — for an
    // archive that's `zipInfo: null`, which would fail deep in the flow.
    if (!source || detecting || preflightBusy || detectError) return;
    if (!host?.trim()) {
      useTransferStore.setState({
        phase: {
          kind: "failed",
          error: "Set your PS5's IP on the Connection tab first.",
        },
      });
      return;
    }
    // Archives extract into a directory, so the pre-flight probe treats them
    // as a folder destination (check the named subdir, not a same-named file).
    const isFolder =
      source.kind === "folder" ||
      source.kind === "game-folder" ||
      source.kind === "archive";
    const { dest } = resolveUploadDest(
      destinationVolume,
      destinationSubpath,
      source.path,
      source.kind === "archive",
    );
    const addr = `${host}:${PS5_PAYLOAD_PORT}`;

    // Pre-flight: does the destination already have content? If no,
    // just go. If yes, the user needs to pick Override / Resume / Cancel
    // (unless they've set "always overwrite" in Settings).
    if (alwaysOverwrite) {
      await startTransfer({
        sourceKind: source.kind,
        srcPath: source.path,
        dest,
        addr,
        strategy: "overwrite",
        excludes: activeExcludes,
        mountAfterUpload: source.kind === "image" && mountAfterUpload,
      mountReadOnly,
      });
      return;
    }
    setPreflightBusy(true);
    setPreflightError(null);
    try {
      // Always probe the FINAL destination path — for folders that means
      // listing `.../<folder-name>` to see if the named subdir exists and
      // how full it is; for files it means checking the parent for a
      // same-named entry. Either way we want the dialog to reflect what
      // the upload will actually land on, not the parent dir.
      const probe = await probeDestination(addr, dest, isFolder);
      setPending({
        dest,
        addr,
        entryCount: probe.exists ? probe.entryCount : 0,
        isFolder,
      });
    } catch (e) {
      // Network/engine failure on probe — surface inline. Without this
      // catch the try/finally swallowed the throw and the button just
      // de-busied with no UI feedback, so the user clicked Upload and
      // nothing visible happened.
      setPreflightError(e instanceof Error ? e.message : String(e));
    } finally {
      setPreflightBusy(false);
    }
  };

  return (
    <div className="p-6">
      {pending && (
        <ExistingDestinationDialog
          entryCount={pending.entryCount}
          dest={pending.dest}
          isFolder={pending.isFolder}
          onOverride={() => beginUpload("overwrite")}
          onResume={() => beginUpload("resume")}
          onCancel={() => setPending(null)}
        />
      )}
      <PageHeader
        icon={UploadIcon}
        title={tr("upload", undefined, "Upload")}
        description={tr(
          "upload_description",
          undefined,
          "Drag a game folder, an .exfat image, or any file onto this window — or use the buttons below. ps5upload figures out what to do based on what you drop.",
        )}
      />

      <PayloadReadinessBanner />

      <Step1Picker
        active={!source}
        dropActive={dropActive}
        onFile={handleChooseFile}
        onFolder={handleChooseFolder}
      />

      {source && (
        <Step2Options
          source={source}
          detecting={detecting}
          detectError={detectError}
          zipInspectEntries={zipInspectEntries}
          mountAfterUpload={mountAfterUpload}
          mountReadOnly={mountReadOnly}
          destinationVolume={destinationVolume}
          destinationSubpath={destinationSubpath}
          availableVolumes={availableVolumes}
          excludeMode={excludeMode}
          excludes={excludes}
          transferPhase={transferPhase}
          preflightBusy={preflightBusy}
          preflightError={preflightError}
          onClear={() => {
            resetTransfer();
            reset();
            // Also dismiss the Override/Resume dialog if it's open —
            // it refers to a source the user just cleared, and leaving
            // it visible would let them click Override on a gone source
            // (beginUpload silently returns when source is null).
            setPending(null);
          }}
          onUseWrappedHint={(p) => pickFolder(p)}
          onSetMountAfterUpload={setMountAfterUpload}
          onSetMountReadOnly={setMountReadOnly}
          onSetDestination={setDestination}
          onSetExcludeMode={setExcludeMode}
          onToggleExclude={toggleExclude}
          onAddExclude={addExclude}
          onRemoveExclude={removeExclude}
          onUpload={handleUpload}
          onAddToQueue={handleAddToQueue}
        />
      )}

      <QueuePanel />
    </div>
  );
}

// ─── Step 1: picker ────────────────────────────────────────────────────────

function Step1Picker({
  active,
  dropActive,
  onFile,
  onFolder,
}: {
  active: boolean;
  dropActive: boolean;
  onFile: () => void;
  onFolder: () => void;
}) {
  const tr = useTr();
  return (
    <section
      className={clsx(
        "mb-6 rounded-lg border-2 border-dashed p-8 text-center transition-colors",
        dropActive
          ? "border-[var(--color-accent)] bg-[var(--color-surface-3)]"
          : "border-[var(--color-border)] bg-[var(--color-surface-2)]",
        !active && "opacity-70"
      )}
    >
      <div className="mb-3 flex items-center justify-center gap-3 text-[var(--color-muted)]">
        <FileIcon size={22} />
        <span className="text-xs">{tr("upload_or", undefined, "or")}</span>
        <FolderOpen size={22} />
      </div>
      <div className="text-sm">
        {tr("upload_drop_here", undefined, "Drop a file or folder here")}
      </div>
      <div className="mt-4 flex items-center justify-center gap-2">
        <button
          type="button"
          onClick={onFile}
          className="rounded-md border border-[var(--color-border)] px-3 py-1.5 text-xs hover:bg-[var(--color-surface-3)]"
        >
          {tr("upload_choose_file", undefined, "Choose file")}
        </button>
        <button
          type="button"
          onClick={onFolder}
          className="rounded-md border border-[var(--color-border)] px-3 py-1.5 text-xs hover:bg-[var(--color-surface-3)]"
        >
          {tr("upload_choose_folder", undefined, "Choose folder")}
        </button>
      </div>
      <p className="mx-auto mt-3 max-w-md text-xs text-[var(--color-muted)]">
        {tr(
          "upload_picker_hint",
          undefined,
          "Files: any file — .exfat images unlock a mount-after-upload option. Folders: game folders are auto-detected from sce_sys/param.sfo.",
        )}
      </p>
    </section>
  );
}

// ─── Step 2: options (branching by kind) ───────────────────────────────────

function Step2Options(props: {
  source: PickedSource;
  detecting: boolean;
  detectError: string | null;
  zipInspectEntries: number | null;
  mountAfterUpload: boolean;
  mountReadOnly: boolean;
  destinationVolume: string | null;
  destinationSubpath: string;
  availableVolumes: Volume[];
  excludeMode: ExcludeMode;
  excludes: { pattern: string; enabled: boolean }[];
  transferPhase: TransferPhase;
  preflightBusy: boolean;
  preflightError: string | null;
  onClear: () => void;
  onUseWrappedHint: (path: string) => void;
  onSetMountAfterUpload: (on: boolean) => void;
  onSetMountReadOnly: (on: boolean) => void;
  onSetDestination: (v: string | null, s?: string) => void;
  onSetExcludeMode: (m: ExcludeMode) => void;
  onToggleExclude: (p: string) => void;
  onAddExclude: (p: string) => void;
  onRemoveExclude: (p: string) => void;
  onUpload: () => void;
  onAddToQueue: (strategy: "overwrite" | "resume") => void;
}) {
  const {
    source,
    detecting,
    detectError,
    zipInspectEntries,
    mountAfterUpload,
    mountReadOnly,
    destinationVolume,
    destinationSubpath,
    availableVolumes,
    excludeMode,
    excludes,
    transferPhase,
    preflightBusy,
    preflightError,
    onClear,
    onUseWrappedHint,
    onSetMountAfterUpload,
    onSetMountReadOnly,
    onSetDestination,
    onSetExcludeMode,
    onToggleExclude,
    onAddExclude,
    onRemoveExclude,
    onUpload,
    onAddToQueue,
  } = props;
  const tr = useTr();

  const { icon: KindIcon, label: kindLabel } = detectedLabel(source);
  const showExcludes =
    source.kind === "folder" ||
    source.kind === "game-folder" ||
    source.kind === "archive";
  const showMountToggle = source.kind === "image";
  const inFlight =
    transferPhase.kind === "starting" || transferPhase.kind === "running";
  // (2.11.0) Mutual-exclusion with QueuePanel. The PS5 payload's
  // transfer port is single-client — concurrent FTX2 connections
  // serialize at the socket, but the UI would say both are
  // "running" while one silently waits. Worse: a user clicking
  // "Upload" with a queue already running can stack work the user
  // doesn't realise will take twice as long. Disable both Upload
  // buttons while the queue runs; QueuePanel does the symmetric
  // disable of its Start button while a one-shot is in flight.
  const queueRunning = useUploadQueueStore((s) => s.running);
  const uploadDisabled =
    detecting || inFlight || preflightBusy || queueRunning || !!detectError;

  return (
    <>
      <section className="mb-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5">
        <div className="flex items-start gap-3">
          <div className="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-[var(--color-surface-3)]">
            <KindIcon size={18} />
          </div>
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <span className="rounded-full bg-[var(--color-surface-3)] px-2 py-0.5 text-xs font-medium">
                {tr("upload_detected_label", "Detected:")} {kindLabel}
              </span>
              {detecting && (
                <span className="inline-flex items-center gap-1 text-xs text-[var(--color-muted)]">
                  <Loader2 size={12} className="animate-spin" />
                  {tr("upload_inspecting", "Inspecting…")}
                </span>
              )}
            </div>
            <div className="mt-1 truncate font-mono text-xs text-[var(--color-muted)]">
              {source.path}
            </div>
            {detecting && (
              // Prominent scanning banner. The small inline spinner up
              // top is easy to miss for users staring at the disabled
              // Upload / Add-to-Queue buttons further down the page —
              // a 200k-file game folder walks for ~30 sec on an
              // external HDD with no visible feedback explaining why
              // those buttons aren't clickable. The banner lives
              // inside the source card so it's the second thing the
              // user reads after the path, and a hint line tells them
              // it's normal and what to expect.
              //
              // For .zip dumps the engine streams a live entry-count
              // via the inspect SSE channel; we surface it inline so
              // the user can see the central-directory walk is
              // actually moving (especially valuable when the
              // archive is on a spun-down USB HDD).
              <div className="mt-2 flex items-start gap-2 rounded-md border border-[var(--color-accent)]/40 bg-[var(--color-accent)]/5 p-2 text-xs">
                <Loader2
                  size={14}
                  className="mt-0.5 shrink-0 animate-spin text-[var(--color-accent)]"
                />
                <div>
                  <div className="font-medium text-[var(--color-text)]">
                    {source.kind === "archive"
                      ? zipInspectEntries !== null
                        ? tr(
                            "upload_scanning_archive_title_with_count",
                            "Scanning archive… {count} entries",
                          ).replace(
                            "{count}",
                            zipInspectEntries.toLocaleString(),
                          )
                        : tr("upload_scanning_archive_title", "Scanning archive…")
                      : tr("upload_scanning_title", "Scanning game folder…")}
                  </div>
                  <div className="text-[var(--color-muted)]">
                    {source.kind === "archive"
                      ? tr(
                          "upload_scanning_archive_hint",
                          "Reading the .zip central directory and parsing embedded game metadata. Upload buttons will enable when this finishes.",
                        )
                      : tr(
                          "upload_scanning_hint",
                          "Counting files and building the upload plan. Large folders (100k+ files) take 30 seconds or so. Upload buttons will enable when this finishes.",
                        )}
                  </div>
                </div>
              </div>
            )}
            {detectError && (
              <div className="mt-2 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface-3)] p-2 text-xs text-[var(--color-bad)]">
                {detectError}
              </div>
            )}
          </div>
          <button
            type="button"
            onClick={onClear}
            title={tr("upload_choose_diff_source", "Choose a different source")}
            className="rounded-md p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
          >
            <X size={16} />
          </button>
        </div>

        {/* Local UFS2 inspector — only meaningful for .ffpkg images.
            Lets the user see "what's in this image" before uploading
            multiple GB. Read-only, no PS5 needed. */}
        {source.kind === "image" &&
          source.path.toLowerCase().endsWith(".ffpkg") && (
            <FfpkgInspectorPanel path={source.path} />
          )}

        {source.wrappedHint && (
          <WrappedHintChip
            hint={source.wrappedHint}
            onUse={() => onUseWrappedHint(source.wrappedHint!.path)}
          />
        )}
      </section>

      {source.kind === "game-folder" && source.meta && (
        <GameMetaCard meta={source.meta} />
      )}

      {source.kind === "folder" && source.meta && (
        <FolderStatsCard
          totalBytes={source.meta.total_size}
          fileCount={source.meta.file_count}
        />
      )}

      {(source.kind === "folder" || source.kind === "game-folder") &&
        source.meta && (
          <PreflightEtaBanner
            fileCount={source.meta.file_count}
            totalBytes={source.meta.total_size}
          />
        )}

      {source.kind === "archive" && source.zipInfo && (
        <ZipArchiveCard info={source.zipInfo} />
      )}

      <FolderDiffSlot
        source={source}
        destinationVolume={destinationVolume}
        destinationSubpath={destinationSubpath}
        excludes={excludes}
      />

      {showMountToggle && (
        <MountAfterUploadCard
          checked={mountAfterUpload}
          onChange={onSetMountAfterUpload}
          readOnly={mountReadOnly}
          onChangeReadOnly={onSetMountReadOnly}
        />
      )}

      <DestinationCard
        volume={destinationVolume}
        subpath={destinationSubpath}
        onChange={onSetDestination}
        availableVolumes={availableVolumes}
        resolvedDest={
          resolveUploadDest(
            destinationVolume,
            destinationSubpath,
            source.path,
            source.kind === "archive",
          ).dest
        }
      />

      <BandwidthCard />

      {showExcludes && (
        <ExcludesCard
          mode={excludeMode}
          excludes={excludes}
          onSetMode={onSetExcludeMode}
          onToggle={onToggleExclude}
          onAdd={onAddExclude}
          onRemove={onRemoveExclude}
        />
      )}

      <TransferStatus phase={transferPhase} />

      <div className="flex flex-wrap items-center justify-end gap-2">
        <button
          type="button"
          onClick={onClear}
          className="rounded-md border border-[var(--color-border)] px-4 py-2 text-sm hover:bg-[var(--color-surface-3)]"
        >
          {tr("upload_cancel", "Cancel")}
        </button>
        {/* Add-to-queue: capture the current source + options into the
            persisted upload queue without starting the transfer. The
            Resume variant only makes sense for folders (single files
            don't reconcile), so it's hidden for image/file sources. */}
        <button
          type="button"
          onClick={() => onAddToQueue("overwrite")}
          disabled={detecting || preflightBusy || !!detectError}
          className="rounded-md border border-[var(--color-border)] px-4 py-2 text-sm hover:bg-[var(--color-surface-3)] disabled:opacity-50"
          title={
            detecting
              ? tr(
                  "upload_disabled_scanning",
                  undefined,
                  "Scanning game folder — wait for the scan to finish, then this button enables.",
                )
              : tr(
                  "upload_add_to_queue_tooltip",
                  "Add this upload to the queue without starting it",
                )
          }
        >
          {tr("upload_add_to_queue", "Add to queue")}
        </button>
        <button
          type="button"
          onClick={onUpload}
          disabled={uploadDisabled}
          title={
            detecting
              ? tr(
                  "upload_disabled_scanning",
                  undefined,
                  "Scanning game folder — wait for the scan to finish, then this button enables.",
                )
              : queueRunning
              ? tr(
                  "upload_disabled_queue_running",
                  undefined,
                  "Upload queue is running — pause the queue to start a one-shot upload, or use 'Add to queue' to append.",
                )
              : undefined
          }
          className="rounded-md bg-[var(--color-accent)] px-6 py-2 text-sm font-medium text-[var(--color-accent-contrast)] disabled:opacity-50"
        >
          {preflightBusy
            ? "Checking…"
            : inFlight
              ? transferPhase.kind === "starting"
                ? "Starting…"
                : "Uploading…"
              : "Upload now"}
        </button>
      </div>
      {preflightError && (
        <div className="mt-2 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface)] p-2 text-[11px] text-[var(--color-bad)]">
          {tr(
            "upload_preflight_failed",
            { msg: preflightError },
            `Couldn't check the destination: ${preflightError}. Make sure the PS5 is reachable, then try again.`,
          )}
        </div>
      )}
      <MirrorToRosterButton
        sourceKind={source.kind}
        srcPath={source.path}
        destinationVolume={destinationVolume}
        destinationSubpath={destinationSubpath}
        excludes={excludes.filter((e) => e.enabled).map((e) => e.pattern)}
      />
    </>
  );
}

/** Fan-out the current upload to every other PS5 in the roster.
 *  Bypasses the regular transfer store — each mirrored upload runs
 *  via direct invoke so it doesn't compete for the store's
 *  single-job slot. Each appears in the Activity tab as its own
 *  entry. Best-effort: failures are logged as notifications, the
 *  rest of the fan-out continues. */
function MirrorToRosterButton({
  sourceKind,
  srcPath,
  destinationVolume,
  destinationSubpath,
  excludes,
}: {
  sourceKind: PickedSource["kind"];
  srcPath: string;
  destinationVolume: string | null;
  destinationSubpath: string;
  excludes: string[];
}) {
  const tr = useTr();
  const profiles = useRosterStore((s) => s.profiles);
  const activeId = useRosterStore((s) => s.active_id);
  const others = profiles.filter((p) => p.id !== activeId);
  const [busy, setBusy] = useState(false);
  if (others.length === 0) return null;
  if (
    sourceKind !== "folder" &&
    sourceKind !== "game-folder" &&
    sourceKind !== "file" &&
    sourceKind !== "archive"
  ) {
    return null;
  }
  if (!destinationVolume) return null;

  async function fanOut() {
    setBusy(true);
    try {
      const { startTransferDir, startTransferFile, startTransferZip, waitForJob } =
        await import("../../api/ps5");
      const rawLeaf =
        srcPath.replace(/\\/g, "/").split("/").filter(Boolean).pop() ?? "";
      // Archives extract into a folder named after the zip, minus `.zip`.
      const leaf =
        sourceKind === "archive" ? rawLeaf.replace(/\.zip$/i, "") : rawLeaf;
      const dest =
        `${destinationVolume}` +
        (destinationSubpath ? `/${destinationSubpath}` : "") +
        `/${leaf}`;
      const tasks = others.map(async (p) => {
        const addr = `${p.host}:${PS5_PAYLOAD_PORT}`;
        try {
          // startTransferFile/Dir only ENQUEUE the job; pre-fix we
          // fired a success notification here and walked away. A
          // 30 GB mirror to four PS5s would toast "Mirrored to X"
          // within a second per peer, even if half of them later
          // failed mid-stream (offline, full, path-denied). Await
          // waitForJob so the notification reflects actual outcome.
          const jobId =
            sourceKind === "file"
              ? await startTransferFile(srcPath, dest, addr)
              : sourceKind === "archive"
                ? await startTransferZip(srcPath, dest, addr, null, excludes)
                : await startTransferDir(srcPath, dest, addr, null, excludes);
          await waitForJob(jobId);
          pushNotification("info", `Mirrored to ${p.name}`, {
            body: `${srcPath} → ${dest}`,
          });
        } catch (e) {
          pushNotification("error", `Mirror failed: ${p.name}`, {
            body: e instanceof Error ? e.message : String(e),
          });
        }
      });
      await Promise.allSettled(tasks);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="mt-3 rounded-md border border-dashed border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-xs">
      <div className="flex items-center gap-2">
        <span className="flex-1 text-[var(--color-muted)]">
          {tr(
            "upload_mirror_label",
            { count: others.length },
            `Also send to other PS5s in roster (${others.length})`,
          )}
        </span>
        <button
          type="button"
          onClick={fanOut}
          disabled={busy}
          className="rounded-md border border-[var(--color-border)] px-2 py-1 text-[11px] hover:bg-[var(--color-surface-2)] disabled:opacity-50"
        >
          {busy
            ? tr("upload_mirror_busy", undefined, "Mirroring…")
            : tr("upload_mirror_send", undefined, "Mirror now")}
        </button>
      </div>
    </div>
  );
}

function ExistingDestinationDialog({
  entryCount,
  dest,
  isFolder,
  onOverride,
  onResume,
  onCancel,
}: {
  entryCount: number;
  dest: string;
  isFolder: boolean;
  onOverride: () => void;
  onResume: () => void;
  onCancel: () => void;
}) {
  const tr = useTr();
  // Clean destination → simple Start/Cancel confirm.
  // Existing content → three-way Override/Resume/Cancel for folders,
  // two-way Override/Cancel for single files (no resume concept on a
  // single blob).
  const hasExisting = entryCount > 0;
  const canResume = hasExisting && isFolder;

  const title = !hasExisting
    ? "Start upload?"
    : isFolder
      ? "Folder already has files"
      : "File already exists";

  const subtitle = !hasExisting ? (
    <>
      {tr("upload_dialog_upload_to", "Upload to")}{" "}
      <span className="font-mono text-xs">{dest}</span>?
    </>
  ) : (
    <>
      <span className="font-mono text-xs">{dest}</span>
      {isFolder
        ? ` contains ${entryCount.toLocaleString()} item${entryCount === 1 ? "" : "s"} already.`
        : ` is already on your PS5.`}
    </>
  );

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm">
      <div className="w-full max-w-md rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5 shadow-2xl">
        <h2 className="mb-1 text-base font-semibold">{title}</h2>
        <p className="mb-4 text-sm text-[var(--color-muted)]">{subtitle}</p>

        <div className="grid gap-2">
          {canResume && (
            <button
              type="button"
              onClick={onResume}
              className="flex items-start gap-3 rounded-md border border-[var(--color-accent)] bg-[var(--color-accent)] p-3 text-left text-sm text-[var(--color-accent-contrast)] hover:opacity-90"
            >
              <span className="font-medium">
                {tr("upload_dialog_resume", "Resume")}
              </span>
              <span className="text-xs opacity-90">
                {tr(
                  "upload_dialog_resume_desc",
                  "Skip files that are already there; only send what's new or changed. Compares file sizes -- per-shard BLAKE3 verification on the actual upload catches any mismatch.",
                )}
              </span>
            </button>
          )}
          <button
            type="button"
            onClick={onOverride}
            className={
              "flex items-start gap-3 rounded-md border p-3 text-left text-sm " +
              (hasExisting
                ? "border-[var(--color-border)] bg-[var(--color-surface)] hover:bg-[var(--color-surface-3)]"
                : "border-[var(--color-accent)] bg-[var(--color-accent)] text-[var(--color-accent-contrast)] hover:opacity-90")
            }
          >
            <span className="font-medium">
              {hasExisting ? "Override" : "Start upload"}
            </span>
            <span
              className={
                "text-xs " +
                (hasExisting ? "text-[var(--color-muted)]" : "opacity-90")
              }
            >
              {hasExisting
                ? "Replace everything — re-send the entire source."
                : "Send all files to the PS5."}
            </span>
          </button>
          <button
            type="button"
            onClick={onCancel}
            className="flex items-start gap-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-3 text-left text-sm hover:bg-[var(--color-surface-3)]"
          >
            <span className="font-medium">
              {tr("upload_dialog_cancel", "Cancel")}
            </span>
            <span className="text-xs text-[var(--color-muted)]">
              {hasExisting
                ? "Don't upload. Pick a different destination or source."
                : "Don't upload."}
            </span>
          </button>
        </div>

        <p className="mt-3 text-xs text-[var(--color-muted)]">
          {tr(
            "upload_dialog_turn_off_prompt",
            "You can turn off this prompt in Settings → Upload.",
          )}
        </p>
      </div>
    </div>
  );
}

function TransferStatus({ phase }: { phase: TransferPhase }) {
  const tr = useTr();
  // Read settings directly — threading through Step2Options just to get
  // here would add props for something that's a rendering decision.
  const showFiles = useUploadSettingsStore((s) => s.showTransferFiles);
  // For the Stop button in the running phase. Bumps the transfer
  // runId so the in-flight poll loop's next state-write is a no-op
  // and the UI returns to idle. Engine job continues server-side
  // until completion or the next reconnect — the payload's single-
  // client transfer port serializes the next BEGIN_TX behind it.
  const resetTransfer = useTransferStore((s) => s.reset);
  if (phase.kind === "idle") return null;

  if (phase.kind === "starting") {
    return (
      <div className="mb-3 flex items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 text-sm">
        <Loader2 size={14} className="animate-spin text-[var(--color-accent)]" />
        {tr("upload_status_preparing", "Preparing upload…")}
      </div>
    );
  }

  if (phase.kind === "running") {
    const {
      bytesSent,
      totalBytes,
      bytesPerSec,
      files,
      filesCompleted,
      skippedFiles,
    } = phase;
    // Reconcile Phase 1 interstitial: the engine set Running before the
    // walk completed, so we have nothing to show yet. "Uploading 0 B"
    // would be misleading — describe what's actually happening.
    if (totalBytes === 0 && files.length === 0) {
      return (
        <div className="mb-3 flex items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 text-sm">
          <Loader2
            size={14}
            className="animate-spin text-[var(--color-accent)]"
          />
          {tr(
            "upload_status_checking_existing",
            "Checking what's already on your PS5…",
          )}
        </div>
      );
    }
    // Pre-byte interstitial: reconcile is done (we have the to-send file
    // list and total bytes), but the first packed shard hasn't gone out yet.
    // The gap is engine-side: build the manifest JSON for N files, send
    // BEGIN_TX, then materialise the first shard (read + pack ~200 small
    // files from disk). On large folders sourced from a slow external drive
    // this can run minutes BEFORE any byte hits the wire, and showing
    // "Uploading 0 B · 0 of N files" makes it look stuck. Show an explicit
    // "Preparing N files…" so the user sees we know about the workload and
    // are working — without lying about the bytes.
    if (bytesSent === 0 && files.length > 0) {
      return (
        <div className="mb-3 flex items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 text-sm">
          <Loader2
            size={14}
            className="animate-spin text-[var(--color-accent)]"
          />
          <span>
            {tr(
              "upload_status_preparing_files",
              { count: files.length.toLocaleString() },
              "Preparing transfer of {count} files…",
            )}
          </span>
          <span className="text-xs text-[var(--color-muted)]">
            {tr(
              "upload_status_preparing_hint",
              "(building the file list and reading the first batch — large folders or slow source disks take a minute)",
            )}
          </span>
        </div>
      );
    }
    const pct =
      totalBytes > 0 ? Math.min(100, (bytesSent / totalBytes) * 100) : 0;
    const remaining = Math.max(0, totalBytes - bytesSent);
    const etaSec = bytesPerSec > 0 ? remaining / bytesPerSec : null;
    // Finalize phase: every shard has been pushed onto the wire but the
    // engine is still waiting for the PS5's COMMIT_TX_ACK / drain ACKs.
    // For large file counts this round-trip waits on the PS5 fsyncing
    // tens of thousands of inodes — many minutes is normal. Without
    // distinct copy here the user sees "Uploading 100%" with a frozen
    // bytes counter and force-quits, defeating the whole transfer.
    // Gate on totalBytes > 0 so a not-yet-stat'd transfer doesn't
    // false-positive on its first tick (the earlier 0-bytes/0-files
    // interstitial above also catches this, but defense in depth).
    const isFinalizing = totalBytes > 0 && bytesSent >= totalBytes;
    return (
      <div className="mb-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 text-sm">
        <div className="mb-2 flex items-center justify-between gap-2">
          <div className="flex items-center gap-2">
            <Loader2
              size={14}
              className="animate-spin text-[var(--color-accent)]"
            />
            <span className="font-medium">
              {isFinalizing
                ? tr(
                    "upload_status_finalizing",
                    "Finalizing on PS5",
                  )
                : tr("upload_status_uploading", "Uploading")}
            </span>
            <span className="text-xs text-[var(--color-muted)]">
              {formatBytes(bytesSent)}
              {totalBytes > 0 && ` / ${formatBytes(totalBytes)}`}
              {totalBytes > 0 && ` · ${pct.toFixed(1)}%`}
              {files.length > 0 && (
                <>
                  {" · "}
                  {filesCompleted.toLocaleString()}{" "}
                  {tr("upload_status_of", "of")}{" "}
                  {files.length.toLocaleString()}{" "}
                  {tr("upload_status_files", "files")}
                </>
              )}
              {skippedFiles > 0 && (
                <>
                  {" · "}
                  {skippedFiles.toLocaleString()}{" "}
                  {tr("upload_status_skipped", "skipped")}
                </>
              )}
            </span>
          </div>
          <div className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
            {!isFinalizing && (
              // Speed + ETA are meaningless once bytes hit 100% — speed
              // collapses to a stale prior reading (the smoother carries
              // the last live sample) and ETA divides "remaining: 0" by
              // a speed that may already be 0. The finalize hint below
              // replaces this readout entirely.
              <span>
                {bytesPerSec > 0 ? `${formatBytes(bytesPerSec)}/s` : "—"}
                {etaSec !== null && bytesPerSec > 0 && (
                  <>
                    {" · "}
                    {tr("upload_status_eta", "ETA")} {formatDuration(etaSec)}
                  </>
                )}
              </span>
            )}
            <button
              type="button"
              onClick={() => resetTransfer()}
              className="rounded-md border border-[var(--color-border)] px-2 py-0.5 text-[10px] text-[var(--color-text)] hover:bg-[var(--color-surface-3)]"
              title={tr(
                "upload_status_stop_tooltip",
                "Stop watching this upload (engine job continues server-side until next BEGIN_TX preempts it)",
              )}
            >
              {tr("upload_status_stop", "Stop")}
            </button>
          </div>
        </div>
        {isFinalizing && (
          // Explanatory line so users with big multi-file uploads
          // understand that 100% is not the end — the PS5 still has
          // to commit the manifest, and for 80k-file folders that
          // legitimately takes minutes (a user-reported 1h+ stall was
          // mistaken for a hang and force-quit).
          //
          // P3 / v2.18.0 — when the payload streams APPLY_PROGRESS,
          // we have a live counter to surface alongside the hint.
          // Falls back to just the hint on old payloads.
          <div className="mb-2 text-xs text-[var(--color-warn)]">
            {phase.filesFinalizingTotal > 0 ? (
              <>
                {tr(
                  "upload_status_finalizing_counter",
                  {
                    done: phase.filesFinalized.toLocaleString(),
                    total: phase.filesFinalizingTotal.toLocaleString(),
                  },
                  `Finalized ${phase.filesFinalized.toLocaleString()} / ${phase.filesFinalizingTotal.toLocaleString()} files. `,
                )}
              </>
            ) : null}
            {tr(
              "upload_status_finalizing_hint",
              "PS5 is committing the file index. This can take a while for large file counts — don't close the app.",
            )}
          </div>
        )}
        {totalBytes > 0 && (
          <div className="h-1.5 overflow-hidden rounded-full bg-[var(--color-surface-3)]">
            <div
              className="h-full bg-[var(--color-accent)] transition-[width] duration-300 ease-out"
              style={{ width: `${pct}%` }}
            />
          </div>
        )}
        {showFiles && files.length > 1 && (
          <FileListPanel files={files} completed={filesCompleted} />
        )}
      </div>
    );
  }

  if (phase.kind === "done") {
    const avg =
      phase.elapsedMs > 0
        ? (phase.bytesSent * 1000) / phase.elapsedMs
        : 0;
    // Reconcile "nothing to do" — everything was already on the PS5.
    // A "0 B sent across 0 files" line would read like a failure.
    const allSkipped =
      phase.bytesSent === 0 &&
      phase.filesSent === 0 &&
      phase.skippedFiles > 0;
    return (
      <div className="mb-3 rounded-md border border-[var(--color-good)] bg-[var(--color-surface-2)] p-4 text-sm">
        <div className="mb-2 font-medium text-[var(--color-good)]">
          {allSkipped ? "Already up to date" : "Upload complete"}
        </div>
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs text-[var(--color-muted)]">
          {!allSkipped && (
            <>
              <dt>{tr("upload_done_sent", "Sent")}</dt>
              <dd className="text-[var(--color-text)]">
                {formatBytes(phase.bytesSent)}
                {phase.filesSent > 0 && (
                  <>
                    {" "}
                    {tr("upload_done_across", "across")}{" "}
                    {phase.filesSent.toLocaleString()}{" "}
                    {tr("upload_done_files", "files")}
                  </>
                )}
              </dd>
            </>
          )}
          {phase.skippedFiles > 0 && (
            <>
              <dt>{allSkipped ? "Already present" : "Skipped"}</dt>
              <dd className="text-[var(--color-text)]">
                {phase.skippedFiles.toLocaleString()}{" "}
                {tr("upload_done_skipped_files", "files (")}
                {formatBytes(phase.skippedBytes)}
                {tr("upload_done_skipped_suffix", ") already on PS5")}
              </dd>
            </>
          )}
          <dt>{tr("upload_done_time", "Time")}</dt>
          <dd className="text-[var(--color-text)]">
            {formatDuration(phase.elapsedMs / 1000)}
            {avg > 0 && (
              <span className="text-[var(--color-muted)]">
                {" "}
                {tr("upload_done_avg", "— avg")} {formatBytes(avg)}/s
              </span>
            )}
          </dd>
          <dt>{tr("upload_done_destination", "Destination")}</dt>
          <dd className="font-mono text-[var(--color-text)]">
            {phase.dest}
          </dd>
          {phase.mountedAt && (
            <>
              <dt>{tr("upload_done_mounted_at", "Mounted at")}</dt>
              <dd className="font-mono text-[var(--color-text)]">
                {phase.mountedAt}
              </dd>
            </>
          )}
        </dl>
        {phase.mountWarnings && phase.mountWarnings.length > 0 && (
          <ul className="mt-2 space-y-1 rounded-md border border-[var(--color-warn)] bg-[var(--color-surface)] p-2 text-[11px] text-[var(--color-warn)]">
            {phase.mountWarnings.map((w) => (
              <li key={w}>⚠ {w}</li>
            ))}
          </ul>
        )}
      </div>
    );
  }

  // failed
  return (
    <div className="mb-3 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface-2)] p-3 text-sm">
      <div className="font-medium text-[var(--color-bad)]">
        {tr("upload_failed_title", "Upload failed")}
      </div>
      <div className="mt-0.5 text-xs text-[var(--color-muted)]">
        {humanizeUploadError(phase.error)}
      </div>
    </div>
  );
}

/** Thin wrapper delegating to the shared humanizer — the rules live in
 *  `lib/humanizeError.ts` so Upload, Volumes, and any future screen
 *  share one ruleset. */
const humanizeUploadError = humanizePs5Error;

/** Maximum number of file rows we ever render to the DOM.
 *
 *  Below this we render the full sorted list (current → pending → done-
 *  reversed). At or above it we render a moving window around the current
 *  file. The cap exists because a single poll tick (~500 ms) was rendering
 *  one `<li>` per planned file, so a 50,000-file game folder produced 50k
 *  DOM nodes plus 3 × O(n) array.map().filter() passes every 500 ms — the
 *  main thread saturated and the UI froze until the upload finished.
 *  The scroll viewport is 192 px tall (~12 visible rows) so 200 is well
 *  past the user-visible band; rows beyond it were never scrolled to.
 *
 *  Wins both ways: small uploads keep their full scrollable history,
 *  large uploads stay responsive. The header counts (`completed / total`)
 *  carry the bigger picture either way. */
const FILE_LIST_RENDER_CAP = 200;

/** Build the visible rows for the panel.
 *
 *  Exported (via `for-test`) so the windowing logic is unit-testable
 *  without standing up React. The shape is intentionally narrow — just
 *  `{ file, idx }` per row, in render order — to keep the test crisp.
 *
 *  Three regimes:
 *    1. total ≤ cap:   full list in [current, pending..., done-rev...] order.
 *    2. total >  cap:  windowed [current, pending-slice..., done-slice-rev...]
 *                      bounded so the total row count is ≤ cap.
 *
 *  Pending and done slices stay in the same order as the unwindowed
 *  version so the user's mental model ("see what's coming → scroll for
 *  history") survives unchanged. */
export function buildFileListRows_forTest(
  files: PlannedFile[],
  completed: number,
  cap: number = FILE_LIST_RENDER_CAP,
): Array<{ idx: number; rel_path: string; size: number }> {
  const total = files.length;
  if (total === 0) return [];

  // Two regimes for `completed`:
  //   - `completed < total`: there's a "current" file at index `completed`.
  //     Pending = indices > completed, done = indices < completed.
  //   - `completed >= total`: every file is done. No current row, no
  //     pending. This is the finalize-phase state — engine reports the
  //     full file count once shards are all on the wire. Done covers
  //     the entire list.
  // A separate `hasCurrent` flag keeps both regimes readable; without it
  // the earlier impl clamped `completed` and then dropped the last index
  // (because the done-loop started at clamped-1 and the current branch
  // was gated on the raw, un-clamped value).
  const hasCurrent = completed >= 0 && completed < total;
  const currentIdx = hasCurrent
    ? completed
    : // For the all-done case the "anchor" sits past the last index so
      // the done window walks backward from total-1 naturally.
      total;
  const pendingCount = hasCurrent ? total - completed - 1 : 0;
  const doneCount = hasCurrent ? completed : total;
  const fullSize = (hasCurrent ? 1 : 0) + pendingCount + doneCount;

  if (fullSize <= cap) {
    // Small list — render every row in a single pass. Beats the old
    // 3 × array.map().filter() shape (3n allocations → ~n).
    const rows: Array<{ idx: number; rel_path: string; size: number }> = [];
    if (hasCurrent) {
      rows.push({ idx: currentIdx, ...files[currentIdx] });
    }
    for (let i = currentIdx + 1; i < total; i++) {
      rows.push({ idx: i, ...files[i] });
    }
    for (let i = currentIdx - 1; i >= 0; i--) {
      rows.push({ idx: i, ...files[i] });
    }
    return rows;
  }

  // Windowed mode. Reserve 1 slot for the current row when present,
  // then split the remainder between "next-up pending" and
  // "recently-done" so the user sees both directions of context
  // without 50k DOM nodes.
  const currentSlot = hasCurrent ? 1 : 0;
  const remaining = cap - currentSlot;
  // 2:1 lean toward pending — users care more about what's *next* than
  // what's already done. The engine's per-file completion counter is
  // coarse (see PlannedFile docs), so deep done-history is the lower-
  // value side to truncate first.
  const pendingBudget = Math.min(pendingCount, Math.ceil((remaining * 2) / 3));
  const doneBudget = Math.min(doneCount, remaining - pendingBudget);

  const rows: Array<{ idx: number; rel_path: string; size: number }> = [];
  if (hasCurrent) {
    rows.push({ idx: currentIdx, ...files[currentIdx] });
  }
  for (let i = 1; i <= pendingBudget; i++) {
    const idx = currentIdx + i;
    rows.push({ idx, ...files[idx] });
  }
  for (let i = 1; i <= doneBudget; i++) {
    const idx = currentIdx - i;
    rows.push({ idx, ...files[idx] });
  }
  return rows;
}

/**
 * Scrollable per-file status panel.
 *
 * Renders the planned file list in a 192-px (h-48) scroll container,
 * current row pinned at the top. Done files get ✓, pending get ○.
 *
 * Above ~200 files we switch to a windowed slice (see
 * `FILE_LIST_RENDER_CAP`); below, we render the full list. Both modes
 * are memoised so the sort/window work doesn't repeat on every poll
 * tick — only when the underlying file list or the completed index
 * changes.
 *
 * `completed` is the engine-reported count of files whose cumulative
 * wire bytes are below `bytes_sent`. That's a *coarse* signal: for a
 * folder with one big file, it jumps from 0 → all-but-one → N only
 * at transfer boundaries. That's honest — until core emits per-file
 * completion events, there's no finer truth available.
 */
function FileListPanel({
  files,
  completed,
}: {
  files: PlannedFile[];
  completed: number;
}) {
  const tr = useTr();
  const total = files.length;
  // Memoise the row build so a parent re-render (every 500 ms while
  // uploading) doesn't redo the sort on 50k entries. Re-runs only when
  // the file list changes (length proxy — engine doesn't mutate planned
  // entries mid-upload) or the completed index moves.
  const rows = useMemo(
    () => buildFileListRows_forTest(files, completed),
    [files, completed],
  );
  const windowed = total > FILE_LIST_RENDER_CAP;

  return (
    <div className="mt-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-xs">
      <div className="mb-1 flex items-center justify-between text-[var(--color-muted)]">
        <span>{tr("upload_file_list_files", "Files")}</span>
        <span>
          {completed.toLocaleString()} / {total.toLocaleString()}
          {windowed && (
            <>
              {" · "}
              <span title={tr(
                "upload_file_list_windowed_hint",
                undefined,
                "Showing a window around the current file; rendering every entry of a 50k+ folder would freeze the app.",
              )}>
                {tr(
                  "upload_file_list_windowed",
                  { shown: rows.length },
                  `showing {shown}`,
                )}
              </span>
            </>
          )}
        </span>
      </div>
      <ul className="max-h-48 overflow-y-auto font-mono">
        {/* Rendering order:
         *   1. Current file (▶) — top, highlighted, always visible
         *      without scrolling.
         *   2. Pending files (○) — next up, in queue order so the
         *      user sees what's about to go.
         *   3. Done files (✓) — below, most-recent first so the last
         *      thing that finished is right under the pending block.
         *
         *   "Current at top, older uploaded at bottom" — matches the
         *   user's ask: glance at the panel → see what's happening
         *   right now + what's coming, scroll down for history. */}
        {rows.map(({ idx, rel_path, size }) => {
          const status =
            idx < completed
              ? "done"
              : idx === completed
                ? "current"
                : "pending";
          return (
            <li
              key={`${idx}-${rel_path}`}
              className={
                "flex items-center gap-2 px-1 py-0.5 " +
                (status === "pending"
                  ? "text-[var(--color-muted)]"
                  : status === "current"
                    ? "rounded bg-[var(--color-surface-3)] font-medium text-[var(--color-text)]"
                    : "text-[var(--color-muted)]")
              }
            >
              <span
                className={
                  "shrink-0 " +
                  (status === "done"
                    ? "text-[var(--color-good)]"
                    : status === "current"
                      ? "text-[var(--color-accent)]"
                      : "opacity-50")
                }
                aria-hidden
              >
                {status === "done" ? "✓" : status === "current" ? "▶" : "○"}
              </span>
              <span className="flex-1 truncate">{rel_path}</span>
              <span className="shrink-0 tabular-nums">
                {formatBytes(size)}
              </span>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/** Human-friendly duration: "3s", "1m 22s", "2h 14m". Used for
 *  ETA display in the progress row — rounds to whole seconds because
 *  a sub-second ETA is meaningless (noise from the rate smoother). */
function formatDuration(sec: number): string {
  if (!isFinite(sec) || sec < 0) return "—";
  if (sec < 60) return `${Math.ceil(sec)}s`;
  const m = Math.floor(sec / 60);
  const s = Math.floor(sec % 60);
  if (m < 60) return `${m}m ${s}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}


function WrappedHintChip({
  hint,
  onUse,
}: {
  hint: { path: string; title: string | null; title_id: string | null };
  onUse: () => void;
}) {
  const tr = useTr();
  const name = hint.title || hint.title_id || hint.path;
  return (
    <div className="mt-3 flex items-start gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-3)] p-3 text-xs">
      <Info size={14} className="mt-0.5 shrink-0 text-[var(--color-accent)]" />
      <div className="flex-1">
        {tr(
          "upload_wrapped_hint_intro",
          "This folder isn't a game on its own, but it contains a game folder inside:",
        )}{" "}
        <span className="font-medium">{name}</span>
        {tr(
          "upload_wrapped_hint_suffix",
          ". Did you mean to upload that one?",
        )}
      </div>
      <button
        type="button"
        onClick={onUse}
        className="shrink-0 rounded-md bg-[var(--color-accent)] px-2.5 py-1 text-xs font-medium text-[var(--color-accent-contrast)]"
      >
        {tr("upload_wrapped_hint_use", "Use it")}
      </button>
    </div>
  );
}

function GameMetaCard({
  meta,
}: {
  meta: {
    title: string | null;
    title_id: string | null;
    content_id: string | null;
    content_version: string | null;
    icon0_path: string | null;
    total_size: number;
    file_count: number;
    meta_source: string;
  };
}) {
  const tr = useTr();
  const coverSrc = useMemo(
    () => (meta.icon0_path ? convertFileSrc(meta.icon0_path) : null),
    [meta.icon0_path]
  );
  const [coverFailed, setCoverFailed] = useState(false);

  return (
    <section className="mb-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5">
      <div className="flex items-start gap-4">
        <div className="h-20 w-20 shrink-0 overflow-hidden rounded-md bg-[var(--color-surface-3)]">
          {coverSrc && !coverFailed ? (
            <img
              src={coverSrc}
              alt={tr("upload_game_cover_alt", "cover")}
              className="h-full w-full object-cover"
              onError={() => setCoverFailed(true)}
            />
          ) : (
            <div className="flex h-full w-full items-center justify-center text-xs text-[var(--color-muted)]">
              <Gamepad2 size={28} />
            </div>
          )}
        </div>
        <div className="min-w-0 flex-1">
          <div className="text-xs text-[var(--color-muted)]">
            {meta.title_id ?? "—"}
            {meta.content_id ? <> · {meta.content_id}</> : null}
          </div>
          <div className="truncate text-lg font-semibold">
            {meta.title ?? "(untitled)"}
          </div>
          <div className="mt-0.5 text-xs text-[var(--color-muted)]">
            {meta.content_version ? `v${meta.content_version} · ` : ""}
            {formatBytes(meta.total_size)} · {meta.file_count.toLocaleString()}{" "}
            {tr("upload_game_meta_files", "files")}
          </div>
          <div className="mt-1 flex items-center gap-1 text-xs text-[var(--color-muted)]">
            <Info size={12} /> {tr("upload_game_meta_parsed_from", "parsed from")}{" "}
            <code>{meta.meta_source}</code>
          </div>
        </div>
      </div>
    </section>
  );
}

function FolderStatsCard({
  totalBytes,
  fileCount,
}: {
  totalBytes: number;
  fileCount: number;
}) {
  const tr = useTr();
  return (
    <section className="mb-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4 text-sm text-[var(--color-muted)]">
      {formatBytes(totalBytes)} {tr("upload_folder_stats_across", "across")}{" "}
      {fileCount.toLocaleString()} {tr("upload_folder_stats_files", "files")}
    </section>
  );
}

/**
 * Pre-flight ETA banner for medium-and-large folder uploads.
 *
 * Suppressed for tiny folders (<1k files). Shows a one-line "≈ N min"
 * estimate in the 1k–10k band. Shows the full transfer + commit
 * breakdown for ≥10k file folders — that's where the post-100%
 * PS5 commit phase becomes the user-perceived dominant cost and
 * users absolutely need to know about it before they kick off a
 * 45-minute upload.
 *
 * Reads the per-host metrics store so the estimate gets sharper after
 * the first successful upload to a given PS5. First-ever upload uses
 * conservative defaults (100 MiB/s, 10 ms/file) and surfaces a
 * "(estimated)" qualifier in the banner copy so users don't read the
 * number as a promise.
 *
 * Design: `.claude/ralph-design-notes.md` §2 (gitignored).
 */
function PreflightEtaBanner({
  fileCount,
  totalBytes,
}: {
  fileCount: number;
  totalBytes: number;
}) {
  const tr = useTr();
  // Read host directly from the connection store rather than threading
  // it through props — the banner only needs it for the per-host
  // metrics lookup, and the source card it sits inside doesn't have
  // host in scope.
  const host = useConnectionStore((s) => s.host);
  const mode = useMemo(() => pickBannerMode(fileCount), [fileCount]);
  const hostMetrics = useRecentHostMetricsStore((s) =>
    host?.trim()
      ? s.lookup(`${host}:${PS5_PAYLOAD_PORT}`)
      : undefined,
  );
  // Staleness aging (MAX_AGE_MS, 7 days) is intentionally not applied
  // here — Date.now() in render trips the react-hooks/purity rule, and
  // the practical impact is nil: a session typically lives minutes
  // while the staleness cap is days. P3 (per-file apply progress) will
  // re-introduce a freshness check via a useEffect-based pattern once
  // commit-ms-per-file is a measurement we actively want to refresh.
  const eta = useMemo(
    () =>
      computeUploadEta({
        fileCount,
        totalBytes,
        throughputMibps: hostMetrics?.throughputMibps,
        commitMsPerFile: hostMetrics?.commitMsPerFile,
      }),
    [fileCount, totalBytes, hostMetrics],
  );

  if (mode === "hidden") return null;

  // Simplified band: medium folders just get a one-line estimate.
  if (mode === "simplified") {
    return (
      <section className="mb-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 text-xs text-[var(--color-muted)]">
        <span className="font-medium text-[var(--color-text)]">
          {tr(
            "upload_eta_simplified",
            { eta: formatEtaSeconds(eta.totalSeconds) },
            "Estimated upload time: {eta}",
          )}
        </span>
        {eta.usedDefaults && (
          <span className="ml-2 italic">
            (
            {tr(
              "upload_eta_default_qualifier",
              undefined,
              "estimate — first upload to this PS5",
            )}
            )
          </span>
        )}
      </section>
    );
  }

  // Detailed band: huge folders get the full breakdown so the post-
  // 100% commit phase isn't a surprise.
  return (
    <section className="mb-4 rounded-lg border border-[var(--color-warn)]/40 bg-[var(--color-warn)]/5 p-4 text-sm">
      <div className="mb-1 flex items-center gap-2 font-medium text-[var(--color-warn)]">
        <span aria-hidden>ⓘ</span>
        {tr("upload_eta_banner_title", undefined, "This is a large folder upload.")}
      </div>
      <div className="mb-3 text-[var(--color-muted)]">
        {fileCount.toLocaleString()}{" "}
        {tr("upload_folder_stats_files", "files")} ·{" "}
        {formatBytes(totalBytes)}
      </div>
      <div className="mb-2 text-xs uppercase tracking-wide text-[var(--color-muted)]">
        {tr("upload_eta_section_title", undefined, "Estimated time")}
        {eta.usedDefaults && (
          <span className="ml-2 normal-case tracking-normal italic">
            (
            {tr(
              "upload_eta_default_qualifier",
              undefined,
              "estimate — first upload to this PS5",
            )}
            )
          </span>
        )}
      </div>
      <table className="text-xs">
        <tbody>
          <tr>
            <td className="pr-3 text-[var(--color-muted)]">
              {tr("upload_eta_row_transfer", undefined, "Transfer")}
            </td>
            <td className="pr-3 tabular-nums">
              ≈ {formatEtaSeconds(eta.transferSeconds)}
            </td>
          </tr>
          <tr>
            <td className="pr-3 text-[var(--color-muted)]">
              {tr("upload_eta_row_commit", undefined, "PS5 commit")}
            </td>
            <td className="pr-3 tabular-nums">
              ≈ {formatEtaSeconds(eta.commitSeconds)}
            </td>
          </tr>
          <tr className="border-t border-[var(--color-border)]">
            <td className="pr-3 pt-1 font-medium">
              {tr("upload_eta_row_total", undefined, "Total")}
            </td>
            <td className="pr-3 pt-1 font-medium tabular-nums">
              ≈ {formatEtaSeconds(eta.totalSeconds)}
            </td>
          </tr>
        </tbody>
      </table>
      <div className="mt-3 text-xs text-[var(--color-muted)]">
        {tr(
          "upload_eta_resume_hint",
          undefined,
          "For folders this size, Resume mode (skip files already on the PS5) saves an hour or more on repeat uploads. You'll see the choice when you click Upload.",
        )}
      </div>
    </section>
  );
}

/** Preview card for a `.zip` source: what it stores vs. what lands on the
 *  PS5, the space saved, and the embedded game (if any). Reassures the user
 *  that the archive is decompressed on the host and lands extracted on the
 *  console — no extra step, no temp copy of the whole game. */
function ZipArchiveCard({ info }: { info: ZipInspect }) {
  const tr = useTr();
  // Space saved by keeping the dump zipped, as a percentage. Guard against a
  // zero/over-100% reading (already-compressed game data can make a "zip"
  // marginally larger than its contents).
  const savedPct =
    info.total_uncompressed > 0 && info.compressed_size < info.total_uncompressed
      ? Math.round(
          (1 - info.compressed_size / info.total_uncompressed) * 100,
        )
      : 0;
  const isGame = !!(info.title || info.title_id);
  return (
    <section className="mb-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5">
      <div className="flex items-start gap-3">
        <div className="flex h-12 w-12 shrink-0 items-center justify-center rounded-md bg-[var(--color-surface-3)] text-[var(--color-muted)]">
          {isGame ? <Gamepad2 size={22} /> : <FileArchive size={22} />}
        </div>
        <div className="min-w-0 flex-1">
          {isGame && (
            <>
              <div className="text-xs text-[var(--color-muted)]">
                {info.title_id ?? "—"}
                {info.content_id ? <> · {info.content_id}</> : null}
              </div>
              <div className="truncate text-lg font-semibold">
                {info.title ?? "(untitled)"}
              </div>
            </>
          )}
          <div className="mt-0.5 text-sm">
            <span className="font-medium">{formatBytes(info.compressed_size)}</span>{" "}
            {tr("upload_zip_zipped", "zipped")}
            {/* The extracted size is only shown when known. inspect_zip
                reports total_uncompressed=0 for archives whose entries use
                data descriptors (common with bsdtar/streaming zippers),
                where the seek-free size read isn't available — show
                count-only instead of a self-contradictory "→ 0 B". */}
            {info.total_uncompressed > 0 && (
              <>
                {" → "}
                <span className="font-medium">
                  {formatBytes(info.total_uncompressed)}
                </span>{" "}
                {tr("upload_zip_extracted", "extracted")}
              </>
            )}{" "}
            · {info.file_count.toLocaleString()}{" "}
            {tr("upload_folder_stats_files", "files")}
          </div>
          {savedPct > 0 && (
            <div className="mt-0.5 text-xs text-[var(--color-good)]">
              {tr("upload_zip_saves", "saves")} {savedPct}%{" "}
              {tr("upload_zip_on_disk", "on disk")}
            </div>
          )}
          <div className="mt-1 flex items-center gap-1 text-xs text-[var(--color-muted)]">
            <Info size={12} />{" "}
            {tr(
              "upload_zip_explainer",
              "Decompressed on your computer and streamed in — files land already extracted on the PS5.",
            )}
          </div>
        </div>
      </div>
    </section>
  );
}

/** Wrapper that pulls `host` from the connection store and adapts the
 *  Upload screen's exclude shape to FolderDiffPanel's API. Renders
 *  null when the inputs aren't sufficient (no host, no destination,
 *  not a folder upload) — keeps Step2Options' main render clean. */
function FolderDiffSlot({
  source,
  destinationVolume,
  destinationSubpath,
  excludes,
}: {
  source: PickedSource;
  destinationVolume: string | null;
  destinationSubpath: string;
  excludes: { pattern: string; enabled: boolean }[];
}) {
  const host = useConnectionStore((s) => s.host);
  if (source.kind !== "folder" && source.kind !== "game-folder") return null;
  if (!destinationVolume || !host?.trim()) return null;
  const leaf = source.path.replace(/\\/g, "/").split("/").filter(Boolean).pop() ?? "";
  const dest =
    `${destinationVolume}` +
    (destinationSubpath ? `/${destinationSubpath}` : "") +
    `/${leaf}`;
  const enabledPatterns = excludes
    .filter((e) => e.enabled)
    .map((e) => e.pattern);
  return (
    <FolderDiffPanel
      srcDir={source.path}
      destRoot={dest}
      transferAddr={`${host.trim()}:${PS5_PAYLOAD_PORT}`}
      excludes={enabledPatterns}
    />
  );
}

/** Per-job bandwidth-cap input. Persists to localStorage via the
 *  upload settings store so the user's preferred cap survives app
 *  restarts. 0 = no cap. The actual transport-layer pacing happens
 *  inside the engine's PipelinedSender (Phase 29's BandwidthThrottle). */
function BandwidthCard() {
  const tr = useTr();
  const cap = useUploadSettingsStore((s) => s.bandwidthCapMbps);
  const setCap = useUploadSettingsStore((s) => s.setBandwidthCapMbps);
  return (
    <section className="mb-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <div className="flex items-center gap-2">
        <label className="text-sm font-medium">
          {tr("upload_bandwidth_label", undefined, "Upload speed cap")}
        </label>
        <input
          type="number"
          min={0}
          step={0.5}
          value={cap}
          onChange={(e) => {
            const n = parseFloat(e.target.value);
            setCap(isFinite(n) && n > 0 ? n : 0);
          }}
          className="w-20 rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-xs"
          placeholder="0"
        />
        <span className="text-xs text-[var(--color-muted)]">MB/s</span>
        {cap > 0 && (
          <button
            type="button"
            onClick={() => setCap(0)}
            className="text-[10px] text-[var(--color-muted)] underline-offset-2 hover:underline"
          >
            {tr("upload_bandwidth_clear", undefined, "remove cap")}
          </button>
        )}
      </div>
      <p className="mt-1 text-[11px] text-[var(--color-muted)]">
        {cap > 0
          ? tr(
              "upload_bandwidth_active",
              { cap },
              `Outbound shards paced to ~${cap} MB/s. Useful when sharing the LAN with video calls or game streaming.`,
            )
          : tr(
              "upload_bandwidth_off",
              undefined,
              "0 = no cap (default). Set a positive value to throttle uploads when sharing bandwidth.",
            )}
      </p>
    </section>
  );
}

function MountAfterUploadCard({
  checked,
  onChange,
  readOnly,
  onChangeReadOnly,
}: {
  checked: boolean;
  onChange: (on: boolean) => void;
  readOnly: boolean;
  onChangeReadOnly: (on: boolean) => void;
}) {
  const tr = useTr();
  return (
    <section className="mb-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5">
      <label className="flex items-start gap-3 text-sm">
        <input
          type="checkbox"
          checked={checked}
          onChange={(e) => onChange(e.target.checked)}
          className="mt-0.5 accent-[var(--color-accent)]"
        />
        <div>
          <div className="font-medium">
            {tr("upload_mount_after_title", "Mount after upload")}
          </div>
          <div className="mt-0.5 text-xs text-[var(--color-muted)]">
            {tr(
              "upload_mount_after_desc",
              "After the image lands on the PS5, the payload mounts it via the kernel's LVD backend. Off by default — turn on if you also want the image attached so the title shows up in the launcher immediately.",
            )}
          </div>
        </div>
      </label>

      {/* Sub-toggle: read-only mode. Visible (greyed when mount-after-upload
          is off) so users can see the option exists. Default on — keeps the
          PS5 from silently writing save-data back into the image and
          corrupting it on next mount. */}
      <label
        className={`mt-3 ml-6 flex items-start gap-3 text-sm ${
          checked ? "" : "opacity-50"
        }`}
      >
        <input
          type="checkbox"
          checked={readOnly}
          onChange={(e) => onChangeReadOnly(e.target.checked)}
          disabled={!checked}
          className="mt-0.5 accent-[var(--color-accent)]"
        />
        <div>
          <div className="font-medium">
            {tr("upload_mount_readonly_title", "Mount read-only")}
          </div>
          <div className="mt-0.5 text-xs text-[var(--color-muted)]">
            {tr(
              "upload_mount_readonly_desc",
              "Recommended. Prevents the PS5 from writing save data into the image (which would silently corrupt the file on disk and break re-mount). Turn off only for editable scratch images.",
            )}
          </div>
        </div>
      </label>
    </section>
  );
}

/** Path-suffix presets corresponding to established on-disk conventions
 *  under any writable storage root. Subpaths match what third-party
 *  managers already look for, so uploads appear where users expect —
 *  but hint copy stays neutral and describes the destination in plain
 *  "what goes here" terms rather than naming specific managers. */
const DESTINATION_PRESETS: { label: string; subpath: string; hint: string }[] = [
  // homebrew is first because it's the community-standard scan path
  // — most PS5 game scanners read from <volume>/homebrew, so files
  // landed here are auto-discoverable.
  { label: "homebrew", subpath: "homebrew", hint: "Homebrew apps & games (recommended)" },
  { label: "exfat", subpath: "exfat", hint: "Disk images" },
  { label: "ps5upload", subpath: "ps5upload", hint: "Tool-specific generic folder" },
];

function DestinationCard({
  volume,
  subpath,
  onChange,
  resolvedDest,
  availableVolumes,
}: {
  volume: string | null;
  subpath: string;
  onChange: (v: string | null, s?: string) => void;
  /** The fully-resolved on-PS5 path (`<volume>/<subpath>/<source-name>`)
   *  as surfaced to the user so they see exactly where the source will
   *  land — catches preset + typo mistakes before the upload starts. */
  resolvedDest: string;
  /** Live list of writable volumes from FS_LIST_VOLUMES. Drives the
   *  dropdown so users see every mount point the PS5 actually exposes
   *  (e.g. `/mnt/ext1`, multiple USB drives, ps5upload-mounted images)
   *  rather than a hardcoded short list. Empty when host isn't set or
   *  the payload isn't reachable yet. */
  availableVolumes: Volume[];
}) {
  const tr = useTr();
  // Trust the payload when it answers: it already filters to writable
  // non-placeholder volumes (see the upstream filter where
  // availableVolumes is set). Hardcoded roots are only a placeholder
  // for the brief window before the probe completes — surfacing them
  // afterwards re-introduces phantom mount points like /mnt/ext0 when
  // nothing is plugged into that slot, which contradicts the Volumes
  // screen and lets the user pick a destination the upload will fail
  // on.
  const FALLBACK_VOLUMES = ["/data", "/mnt/ext0", "/mnt/usb0"];
  const dropdownPaths =
    availableVolumes.length > 0
      ? availableVolumes.map((v) => v.path).sort()
      : [...FALLBACK_VOLUMES].sort();
  // Build a {path → free-bytes} map so we can show "/mnt/ext1 (450 GB
  // free)" inline. Only when we actually got the live list back.
  const freeBytesByPath = new Map<string, number>();
  for (const v of availableVolumes) {
    freeBytesByPath.set(v.path, v.free_bytes);
  }
  const formatFree = (bytes: number) => {
    const gib = bytes / (1024 ** 3);
    if (gib >= 1024) return `${(gib / 1024).toFixed(1)} TB free`;
    if (gib >= 10) return `${gib.toFixed(0)} GB free`;
    return `${gib.toFixed(1)} GB free`;
  };

  return (
    <section className="mb-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5">
      <div className="mb-3 flex items-center gap-2 text-sm font-semibold">
        <HardDrive size={14} />
        {tr("upload_dest_card_title", "Destination")}
      </div>

      <div className="mb-3 flex items-center gap-2 text-sm">
        <select
          value={volume ?? ""}
          onChange={(e) => onChange(e.target.value || null)}
          className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-1.5 text-sm"
        >
          <option value="">
            {tr("upload_dest_auto", "(auto — largest free)")}
          </option>
          {dropdownPaths.map((p) => {
            const free = freeBytesByPath.get(p);
            return (
              <option key={p} value={p}>
                {free !== undefined ? `${p} (${formatFree(free)})` : p}
              </option>
            );
          })}
        </select>
        <span className="text-[var(--color-muted)]">/</span>
        <input
          value={subpath}
          onChange={(e) => onChange(volume, e.target.value)}
          className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-1.5 text-sm"
        />
      </div>

      <div className="flex flex-wrap items-center gap-1.5">
        <span className="text-xs text-[var(--color-muted)]">
          {tr("upload_dest_presets", "Presets:")}
        </span>
        {DESTINATION_PRESETS.map((p) => {
          const active = subpath === p.subpath;
          return (
            <button
              key={p.subpath}
              type="button"
              onClick={() => onChange(volume, p.subpath)}
              title={p.hint}
              className={clsx(
                "rounded-full border px-2.5 py-0.5 text-xs transition-colors",
                active
                  ? "border-[var(--color-accent)] bg-[var(--color-accent)] text-[var(--color-accent-contrast)]"
                  : "border-[var(--color-border)] bg-[var(--color-surface)] hover:bg-[var(--color-surface-3)]"
              )}
            >
              {p.label}
            </button>
          );
        })}
      </div>

      <div className="mt-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2.5">
        <div className="text-xs uppercase tracking-wide text-[var(--color-muted)]">
          {tr("upload_dest_final_path", "Final path on PS5")}
        </div>
        <div className="mt-0.5 break-all font-mono text-xs text-[var(--color-text)]">
          {resolvedDest}
        </div>
      </div>
      <p className="mt-2 text-xs text-[var(--color-muted)]">
        {tr(
          "upload_dest_will_create",
          "If the destination folder doesn't exist yet on the PS5, it will be created when you start the upload.",
        )}
      </p>
    </section>
  );
}

function ExcludesCard({
  mode,
  excludes,
  onSetMode,
  onToggle,
  onAdd,
  onRemove,
}: {
  mode: ExcludeMode;
  excludes: { pattern: string; enabled: boolean }[];
  onSetMode: (m: ExcludeMode) => void;
  onToggle: (p: string) => void;
  onAdd: (p: string) => void;
  onRemove: (p: string) => void;
}) {
  const tr = useTr();
  const [draft, setDraft] = useState("");
  return (
    <section className="mb-6 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5">
      <h2 className="mb-3 text-sm font-semibold">
        {tr("upload_files_to_upload", "Files to upload")}
      </h2>

      <div className="mb-3 grid gap-2 sm:grid-cols-2">
        <ModeRadio
          label={tr("upload_include_all_files", "Include all files")}
          hint="Default. Nothing is filtered."
          checked={mode === "all"}
          onCheck={() => onSetMode("all")}
        />
        <ModeRadio
          label={tr("upload_apply_exclude_rules", "Apply exclude rules")}
          hint="Skip junk files and anything you add below."
          checked={mode === "rules"}
          onCheck={() => onSetMode("rules")}
        />
      </div>

      {mode === "rules" && (
        <div>
          <div className="grid gap-1.5 sm:grid-cols-2">
            {excludes.map((rule) => (
              <div
                key={rule.pattern}
                className="flex items-center gap-2 text-sm"
              >
                <input
                  type="checkbox"
                  checked={rule.enabled}
                  onChange={() => onToggle(rule.pattern)}
                  className="accent-[var(--color-accent)]"
                />
                <code className="flex-1 rounded bg-[var(--color-surface-3)] px-1.5 py-0.5 text-xs">
                  {rule.pattern}
                </code>
                <button
                  type="button"
                  onClick={() => onRemove(rule.pattern)}
                  className="rounded-md p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
                  title={`Remove ${rule.pattern}`}
                >
                  <X size={12} />
                </button>
              </div>
            ))}
          </div>
          <form
            className="mt-3 flex items-center gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              if (draft.trim()) {
                onAdd(draft);
                setDraft("");
              }
            }}
          >
            <input
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              placeholder="Add pattern (e.g. *.log)"
              className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-1.5 text-sm"
            />
            <button
              type="submit"
              disabled={!draft.trim()}
              className="flex items-center gap-1 rounded-md border border-[var(--color-border)] px-3 py-1.5 text-xs hover:bg-[var(--color-surface-3)] disabled:opacity-50"
            >
              <Plus size={12} />
              {tr("upload_exclude_add", "Add")}
            </button>
          </form>
        </div>
      )}
    </section>
  );
}

function ModeRadio({
  label,
  hint,
  checked,
  onCheck,
}: {
  label: string;
  hint: string;
  checked: boolean;
  onCheck: () => void;
}) {
  return (
    <label
      className={clsx(
        "cursor-pointer rounded-md border px-3 py-2 text-sm transition-colors",
        checked
          ? "border-[var(--color-accent)] bg-[var(--color-surface-3)]"
          : "border-[var(--color-border)] bg-[var(--color-surface)] hover:border-[var(--color-muted)]"
      )}
    >
      <div className="flex items-center gap-2">
        <input
          type="radio"
          checked={checked}
          onChange={onCheck}
          className="accent-[var(--color-accent)]"
        />
        <span className="font-medium">{label}</span>
      </div>
      <div className="mt-0.5 pl-6 text-xs text-[var(--color-muted)]">
        {hint}
      </div>
    </label>
  );
}

// Suppress an "unused" warning for the SourceKind re-export in the file —
// we only re-import the type for clarity above.
export type { SourceKind };

/**
 * Warning card shown at the top of Upload when the PS5 payload isn't
 * currently reachable. Appears for `payloadStatus !== "up"` — covers
 * both "never connected" (user navigated straight to Upload) and
 * "payload was up but dropped" (network toggle, crash).
 *
 * A button sends the user back to Connection to resolve. This is
 * softer than an outright "Upload disabled" block — the upload flow
 * still lets you set up a drop and tweak excludes; it just tells you
 * the actual transfer won't work until the PS5 is listening.
 */
function PayloadReadinessBanner() {
  const tr = useTr();
  const status = useConnectionStore((s) => s.payloadStatus);
  const navigate = useNavigate();
  if (status === "up") return null;
  const title =
    status === "down"
      ? "PS5 payload isn't reachable"
      : "PS5 payload status unknown";
  const detail =
    status === "down"
      ? "Uploads will fail until the payload is running. Head back to Connection, press Send, and wait for the third step to turn green."
      : "We haven't confirmed your PS5 is running the payload yet. Set your IP and send the payload on the Connection tab first.";
  return (
    <div className="mb-4">
      <WarningCard
        title={title}
        detail={detail}
        action={
          <Button
            variant="secondary"
            size="sm"
            onClick={() => navigate("/connection")}
          >
            {tr("upload_go_to_connection", "Go to Connection")}
          </Button>
        }
      />
    </div>
  );
}
