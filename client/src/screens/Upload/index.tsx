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
} from "../../api/ps5";
import {
  useTransferStore,
  type TransferPhase,
  type UploadStrategy,
} from "../../state/transfer";
import { useConnectionStore, PS5_PAYLOAD_PORT } from "../../state/connection";
import { useNavigate } from "react-router-dom";
import { PageHeader, WarningCard, Button } from "../../components";
import { useUploadSettingsStore } from "../../state/uploadSettings";
import { useUploadQueueStore } from "../../state/uploadQueue";
import { resolveUploadDest } from "../../lib/uploadDest";
import { QueuePanel } from "./QueuePanel";
import { humanizePs5Error } from "../../lib/humanizeError";

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
  }
}

export default function UploadScreen() {
  const store = useUploadStore();
  const {
    source,
    detecting,
    detectError,
    mountAfterUpload,
    destinationVolume,
    destinationSubpath,
    excludeMode,
    excludes,
    pickFile,
    pickFolder,
    reset,
    setMountAfterUpload,
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
      if (cancelled) {
        fn();
      } else {
        unlisten = fn;
      }
    });
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
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

  // Live list of writable PS5 volumes for the destination dropdown.
  // Refreshed when the host changes; previously the dropdown was a
  // hardcoded `/data /mnt/ext0 /mnt/usb0` list which hid every other
  // mount point (e.g. `/mnt/ext1`, `/mnt/usbN` for N>0, and any
  // ps5upload-mounted images).
  const [availableVolumes, setAvailableVolumes] = useState<Volume[]>([]);
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
    });
  };

  const handleUpload = async () => {
    if (!source || detecting || preflightBusy) return;
    if (!host?.trim()) {
      useTransferStore.setState({
        phase: {
          kind: "failed",
          error: "Set your PS5's IP on the Connection tab first.",
        },
      });
      return;
    }
    const isFolder =
      source.kind === "folder" || source.kind === "game-folder";
    const { dest } = resolveUploadDest(
      destinationVolume,
      destinationSubpath,
      source.path,
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
      });
      return;
    }
    setPreflightBusy(true);
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
        title="Upload"
        description={
          <>
            Drag a game folder, an <code>.exfat</code> image, or any file
            onto this window — or use the buttons below. ps5upload
            figures out what to do based on what you drop.
          </>
        }
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
          mountAfterUpload={mountAfterUpload}
          destinationVolume={destinationVolume}
          destinationSubpath={destinationSubpath}
          availableVolumes={availableVolumes}
          excludeMode={excludeMode}
          excludes={excludes}
          transferPhase={transferPhase}
          preflightBusy={preflightBusy}
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
        <span className="text-xs">or</span>
        <FolderOpen size={22} />
      </div>
      <div className="text-sm">
        Drop a file or folder here
      </div>
      <div className="mt-4 flex items-center justify-center gap-2">
        <button
          type="button"
          onClick={onFile}
          className="rounded-md border border-[var(--color-border)] px-3 py-1.5 text-xs hover:bg-[var(--color-surface-3)]"
        >
          Choose file
        </button>
        <button
          type="button"
          onClick={onFolder}
          className="rounded-md border border-[var(--color-border)] px-3 py-1.5 text-xs hover:bg-[var(--color-surface-3)]"
        >
          Choose folder
        </button>
      </div>
      <p className="mx-auto mt-3 max-w-md text-xs text-[var(--color-muted)]">
        Files: any file — <code>.exfat</code> images unlock a mount-after-upload
        option. Folders: game folders are auto-detected from{" "}
        <code>sce_sys/param.sfo</code>.
      </p>
    </section>
  );
}

// ─── Step 2: options (branching by kind) ───────────────────────────────────

function Step2Options(props: {
  source: PickedSource;
  detecting: boolean;
  detectError: string | null;
  mountAfterUpload: boolean;
  destinationVolume: string | null;
  destinationSubpath: string;
  availableVolumes: Volume[];
  excludeMode: ExcludeMode;
  excludes: { pattern: string; enabled: boolean }[];
  transferPhase: TransferPhase;
  preflightBusy: boolean;
  onClear: () => void;
  onUseWrappedHint: (path: string) => void;
  onSetMountAfterUpload: (on: boolean) => void;
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
    mountAfterUpload,
    destinationVolume,
    destinationSubpath,
    availableVolumes,
    excludeMode,
    excludes,
    transferPhase,
    preflightBusy,
    onClear,
    onUseWrappedHint,
    onSetMountAfterUpload,
    onSetDestination,
    onSetExcludeMode,
    onToggleExclude,
    onAddExclude,
    onRemoveExclude,
    onUpload,
    onAddToQueue,
  } = props;

  const { icon: KindIcon, label: kindLabel } = detectedLabel(source);
  const showExcludes = source.kind === "folder" || source.kind === "game-folder";
  const showMountToggle = source.kind === "image";
  const inFlight =
    transferPhase.kind === "starting" || transferPhase.kind === "running";
  const uploadDisabled = detecting || inFlight || preflightBusy;

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
                Detected: {kindLabel}
              </span>
              {detecting && (
                <span className="inline-flex items-center gap-1 text-xs text-[var(--color-muted)]">
                  <Loader2 size={12} className="animate-spin" />
                  Inspecting…
                </span>
              )}
            </div>
            <div className="mt-1 truncate font-mono text-xs text-[var(--color-muted)]">
              {source.path}
            </div>
            {detectError && (
              <div className="mt-2 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface-3)] p-2 text-xs text-[var(--color-bad)]">
                {detectError}
              </div>
            )}
          </div>
          <button
            type="button"
            onClick={onClear}
            title="Choose a different source"
            className="rounded-md p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
          >
            <X size={16} />
          </button>
        </div>

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

      {showMountToggle && (
        <MountAfterUploadCard
          checked={mountAfterUpload}
          onChange={onSetMountAfterUpload}
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
          ).dest
        }
      />

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
          Cancel
        </button>
        {/* Add-to-queue: capture the current source + options into the
            persisted upload queue without starting the transfer. The
            Resume variant only makes sense for folders (single files
            don't reconcile), so it's hidden for image/file sources. */}
        <button
          type="button"
          onClick={() => onAddToQueue("overwrite")}
          disabled={detecting || preflightBusy}
          className="rounded-md border border-[var(--color-border)] px-4 py-2 text-sm hover:bg-[var(--color-surface-3)] disabled:opacity-50"
          title="Add this upload to the queue without starting it"
        >
          Add to queue
        </button>
        <button
          type="button"
          onClick={onUpload}
          disabled={uploadDisabled}
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
    </>
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
      Upload to <span className="font-mono text-xs">{dest}</span>?
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
              <span className="font-medium">Resume</span>
              <span className="text-xs opacity-90">
                Skip files that are already there; only send what's new
                or changed. Compares file sizes -- per-shard BLAKE3
                verification on the actual upload catches any mismatch.
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
            <span className="font-medium">Cancel</span>
            <span className="text-xs text-[var(--color-muted)]">
              {hasExisting
                ? "Don't upload. Pick a different destination or source."
                : "Don't upload."}
            </span>
          </button>
        </div>

        <p className="mt-3 text-xs text-[var(--color-muted)]">
          You can turn off this prompt in Settings → Upload.
        </p>
      </div>
    </div>
  );
}

function TransferStatus({ phase }: { phase: TransferPhase }) {
  // Read settings directly — threading through Step2Options just to get
  // here would add props for something that's a rendering decision.
  const showFiles = useUploadSettingsStore((s) => s.showTransferFiles);
  if (phase.kind === "idle") return null;

  if (phase.kind === "starting") {
    return (
      <div className="mb-3 flex items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 text-sm">
        <Loader2 size={14} className="animate-spin text-[var(--color-accent)]" />
        Preparing upload…
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
          Checking what's already on your PS5…
        </div>
      );
    }
    const pct =
      totalBytes > 0 ? Math.min(100, (bytesSent / totalBytes) * 100) : 0;
    const remaining = Math.max(0, totalBytes - bytesSent);
    const etaSec = bytesPerSec > 0 ? remaining / bytesPerSec : null;
    return (
      <div className="mb-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 text-sm">
        <div className="mb-2 flex items-center justify-between gap-2">
          <div className="flex items-center gap-2">
            <Loader2
              size={14}
              className="animate-spin text-[var(--color-accent)]"
            />
            <span className="font-medium">Uploading</span>
            <span className="text-xs text-[var(--color-muted)]">
              {formatBytes(bytesSent)}
              {totalBytes > 0 && ` / ${formatBytes(totalBytes)}`}
              {totalBytes > 0 && ` · ${pct.toFixed(1)}%`}
              {files.length > 0 && (
                <>
                  {" · "}
                  {filesCompleted.toLocaleString()} of{" "}
                  {files.length.toLocaleString()} files
                </>
              )}
              {skippedFiles > 0 && (
                <> · {skippedFiles.toLocaleString()} skipped</>
              )}
            </span>
          </div>
          <div className="text-xs text-[var(--color-muted)]">
            {bytesPerSec > 0 ? `${formatBytes(bytesPerSec)}/s` : "—"}
            {etaSec !== null && bytesPerSec > 0 && (
              <> · ETA {formatDuration(etaSec)}</>
            )}
          </div>
        </div>
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
              <dt>Sent</dt>
              <dd className="text-[var(--color-text)]">
                {formatBytes(phase.bytesSent)}
                {phase.filesSent > 0 && (
                  <> across {phase.filesSent.toLocaleString()} files</>
                )}
              </dd>
            </>
          )}
          {phase.skippedFiles > 0 && (
            <>
              <dt>{allSkipped ? "Already present" : "Skipped"}</dt>
              <dd className="text-[var(--color-text)]">
                {phase.skippedFiles.toLocaleString()} files (
                {formatBytes(phase.skippedBytes)}) already on PS5
              </dd>
            </>
          )}
          <dt>Time</dt>
          <dd className="text-[var(--color-text)]">
            {formatDuration(phase.elapsedMs / 1000)}
            {avg > 0 && (
              <span className="text-[var(--color-muted)]">
                {" "}
                — avg {formatBytes(avg)}/s
              </span>
            )}
          </dd>
          <dt>Destination</dt>
          <dd className="font-mono text-[var(--color-text)]">
            {phase.dest}
          </dd>
          {phase.mountedAt && (
            <>
              <dt>Mounted at</dt>
              <dd className="font-mono text-[var(--color-text)]">
                {phase.mountedAt}
              </dd>
            </>
          )}
        </dl>
      </div>
    );
  }

  // failed
  return (
    <div className="mb-3 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface-2)] p-3 text-sm">
      <div className="font-medium text-[var(--color-bad)]">Upload failed</div>
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

/**
 * Scrollable per-file status panel.
 *
 * Renders every planned file in a 192-px (h-48) scroll container and
 * auto-scrolls so the currently-transferring file (▶) stays visible.
 * Done files get ✓, pending get ○. For very large trees (1000+ files)
 * rendering all DOM nodes is measurably slower but still well under a
 * frame budget; if a real perf issue emerges, swap this for a windowed
 * virtualizer later.
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
  const total = files.length;
  // No auto-scroll needed — the ordering puts the current file at the
  // top of the list, so it's visible without any scrolling math.

  return (
    <div className="mt-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-xs">
      <div className="mb-1 flex items-center justify-between text-[var(--color-muted)]">
        <span>Files</span>
        <span>
          {completed.toLocaleString()} / {total.toLocaleString()}
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
        {[
          ...files
            .map((f, idx) => ({ f, idx }))
            .filter((x) => x.idx === completed),
          ...files
            .map((f, idx) => ({ f, idx }))
            .filter((x) => x.idx > completed),
          ...files
            .map((f, idx) => ({ f, idx }))
            .filter((x) => x.idx < completed)
            .reverse(),
        ].map(({ f, idx }) => {
          const status =
            idx < completed
              ? "done"
              : idx === completed
                ? "current"
                : "pending";
          return (
            <li
              key={`${idx}-${f.rel_path}`}
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
              <span className="flex-1 truncate">{f.rel_path}</span>
              <span className="shrink-0 tabular-nums">
                {formatBytes(f.size)}
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
  const name = hint.title || hint.title_id || hint.path;
  return (
    <div className="mt-3 flex items-start gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-3)] p-3 text-xs">
      <Info size={14} className="mt-0.5 shrink-0 text-[var(--color-accent)]" />
      <div className="flex-1">
        This folder isn't a game on its own, but it contains a game folder
        inside: <span className="font-medium">{name}</span>. Did you mean to
        upload that one?
      </div>
      <button
        type="button"
        onClick={onUse}
        className="shrink-0 rounded-md bg-[var(--color-accent)] px-2.5 py-1 text-xs font-medium text-[var(--color-accent-contrast)]"
      >
        Use it
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
              alt="cover"
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
            files
          </div>
          <div className="mt-1 flex items-center gap-1 text-xs text-[var(--color-muted)]">
            <Info size={12} /> parsed from <code>{meta.meta_source}</code>
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
  return (
    <section className="mb-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4 text-sm text-[var(--color-muted)]">
      {formatBytes(totalBytes)} across {fileCount.toLocaleString()} files
    </section>
  );
}

function MountAfterUploadCard({
  checked,
  onChange,
}: {
  checked: boolean;
  onChange: (on: boolean) => void;
}) {
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
          <div className="font-medium">Mount after upload</div>
          <div className="mt-0.5 text-xs text-[var(--color-muted)]">
            After the image lands on the PS5, the payload mounts it via the
            kernel's LVD backend. Turn off if you only want to stage the
            image without mounting.
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
  { label: "etaHEN/games", subpath: "etaHEN/games", hint: "Installed games" },
  { label: "homebrew", subpath: "homebrew", hint: "Homebrew apps" },
  { label: "exfat", subpath: "exfat", hint: "Disk images" },
  { label: "ps5upload", subpath: "ps5upload", hint: "Generic folder" },
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
        Destination
      </div>

      <div className="mb-3 flex items-center gap-2 text-sm">
        <select
          value={volume ?? ""}
          onChange={(e) => onChange(e.target.value || null)}
          className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-1.5 text-sm"
        >
          <option value="">(auto — largest free)</option>
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
        <span className="text-xs text-[var(--color-muted)]">Presets:</span>
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
          Final path on PS5
        </div>
        <div className="mt-0.5 break-all font-mono text-xs text-[var(--color-text)]">
          {resolvedDest}
        </div>
      </div>
      <p className="mt-2 text-xs text-[var(--color-muted)]">
        If the destination folder doesn't exist yet on the PS5, it will
        be created when you start the upload.
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
  const [draft, setDraft] = useState("");
  return (
    <section className="mb-6 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5">
      <h2 className="mb-3 text-sm font-semibold">Files to upload</h2>

      <div className="mb-3 grid gap-2 sm:grid-cols-2">
        <ModeRadio
          label="Include all files"
          hint="Default. Nothing is filtered."
          checked={mode === "all"}
          onCheck={() => onSetMode("all")}
        />
        <ModeRadio
          label="Apply exclude rules"
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
              Add
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
            Go to Connection
          </Button>
        }
      />
    </div>
  );
}
