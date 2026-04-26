import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  FolderTree,
  Folder,
  File as FileIcon,
  Loader2,
  AlertTriangle,
  ChevronRight,
  Home,
  ArrowUp,
  Trash2,
  Pencil,
  FolderPlus,
  RefreshCw,
  Scissors,
  Copy,
  ClipboardPaste,
  Download,
  X,
} from "lucide-react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { PageHeader, Button } from "../../components";
import { useTr } from "../../state/lang";

import { useConnectionStore, PS5_PAYLOAD_PORT } from "../../state/connection";
import {
  fsDelete,
  fsMove,
  fsMkdir,
  fsCopy,
  jobStatus,
  startTransferDownload,
} from "../../api/ps5";
import {
  useFsClipboardStore,
  type ClipboardItem,
} from "../../state/fsClipboard";
import {
  useFsBulkOpStore,
  useFsDownloadOpStore,
} from "../../state/fsBulkOp";
import { useElapsed } from "../../lib/useElapsed";

/**
 * PS5 file explorer with multi-select + cut/copy/paste.
 *
 * Select model: checkboxes per row, plus a "select all" in the header.
 * Once ≥1 entry is selected a bulk-action toolbar appears (Cut / Copy /
 * Delete) that applies to the whole selection. Cut/Copy seeds the
 * shared clipboard store (see state/fsClipboard.ts); Paste is available
 * from the toolbar whenever the clipboard is non-empty, pasting into
 * the currently-browsed directory.
 *
 * Cross-volume note: fsMove uses rename() which fails with EXDEV across
 * mount points. fsCopy works across mounts because the payload reads +
 * writes bytes explicitly. The UI falls back to "Copy, then delete"
 * when a Cut+Paste fails with EXDEV — matches how file managers on
 * Linux/macOS handle cross-filesystem moves.
 */

interface DirEntry {
  name: string;
  kind: string; // "file" | "dir" | "link" | "other" | "unknown"
  size: number;
}

function formatDuration(sec: number): string {
  if (!isFinite(sec) || sec < 0) return "—";
  if (sec < 60) return `${Math.ceil(sec)}s`;
  const m = Math.floor(sec / 60);
  const s = Math.floor(sec % 60);
  if (m < 60) return `${m}m ${s}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}

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

function toMgmtAddr(transferAddr: string): string {
  const i = transferAddr.lastIndexOf(":");
  return i < 0 ? `${transferAddr}:9114` : `${transferAddr.slice(0, i)}:9114`;
}

function parent(p: string): string {
  if (p === "/" || p === "") return "/";
  const i = p.lastIndexOf("/");
  if (i <= 0) return "/";
  return p.slice(0, i);
}

function joinPath(dir: string, name: string): string {
  if (dir.endsWith("/")) return `${dir}${name}`;
  return `${dir}/${name}`;
}

function crumbs(p: string): { label: string; path: string }[] {
  const parts = p.split("/").filter(Boolean);
  const out: { label: string; path: string }[] = [
    { label: "/", path: "/" },
  ];
  let cur = "";
  for (const part of parts) {
    cur += `/${part}`;
    out.push({ label: part, path: cur });
  }
  return out;
}

export default function FileSystemScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const clipboard = useFsClipboardStore();
  const [path, setPath] = useState("/data");
  const [entries, setEntries] = useState<DirEntry[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [mkdirDraft, setMkdirDraft] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busyEntry, setBusyEntry] = useState<{
    name: string;
    op: "rename" | "mkdir";
  } | null>(null);
  // Lifted into Zustand so the in-flight bulk op survives navigation.
  // The async runner writes to the store; the screen reads from it.
  // Re-mount after a tab switch sees the still-running operation.
  const bulkOp = useFsBulkOpStore();
  const downloadOp = useFsDownloadOpStore();
  const elapsedMs = useElapsed(
    busyEntry !== null || bulkOp.op !== null || downloadOp.active,
  );
  // Note: no mountedRef needed for the download / bulk-op runners
  // anymore — both write to global Zustand stores, so an unmount
  // mid-run is harmless. The runner loops keep going (writing to
  // the store), the screen re-mount sees the in-flight progress.

  const refresh = useCallback(async () => {
    if (!host?.trim()) return;
    const addr = toMgmtAddr(`${host}:${PS5_PAYLOAD_PORT}`);
    setLoading(true);
    setError(null);
    try {
      const listing = await invoke<{
        entries?: DirEntry[];
        truncated?: boolean;
      }>("ps5_list_dir", { addr, path, offset: 0, limit: 256 });
      const raw = listing.entries ?? [];
      raw.sort((a, b) => {
        if (a.kind !== b.kind) {
          if (a.kind === "dir") return -1;
          if (b.kind === "dir") return 1;
        }
        return a.name.localeCompare(b.name);
      });
      setEntries(raw);
      // Prune selections for items that no longer exist (e.g. after a
      // refresh that followed a delete). Selections keyed by name are
      // cheap to re-validate.
      setSelected((s) => {
        const names = new Set(raw.map((e) => e.name));
        const next = new Set<string>();
        for (const n of s) if (names.has(n)) next.add(n);
        return next;
      });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setEntries(null);
    } finally {
      setLoading(false);
    }
  }, [host, path]);

  useEffect(() => {
    if (payloadStatus === "up") refresh();
  }, [payloadStatus, refresh]);

  // Changing directories clears the selection — carrying selections
  // across directories would be confusing (what does "delete" act on?).
  useEffect(() => {
    setSelected(new Set());
  }, [path]);

  const toggleSelected = (name: string) => {
    setSelected((s) => {
      const next = new Set(s);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  };

  const toggleAll = () => {
    if (!entries) return;
    setSelected((s) => {
      if (s.size === entries.length) return new Set();
      return new Set(entries.map((e) => e.name));
    });
  };

  const selectedEntries = useMemo(
    () => (entries ? entries.filter((e) => selected.has(e.name)) : []),
    [entries, selected]
  );

  const runDelete = async (name: string) => {
    if (!confirm(`Delete "${name}"? This removes the path recursively.`)) return;
    setBusyEntry({ name, op: "rename" }); // cheap reuse of spinner
    setError(null);
    try {
      await fsDelete(`${host}:${PS5_PAYLOAD_PORT}`, joinPath(path, name));
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusyEntry(null);
    }
  };

  const runRename = async (oldName: string) => {
    const newName = renameDraft.trim();
    if (!newName || newName === oldName) {
      setRenaming(null);
      return;
    }
    setBusyEntry({ name: oldName, op: "rename" });
    setError(null);
    try {
      await fsMove(
        `${host}:${PS5_PAYLOAD_PORT}`,
        joinPath(path, oldName),
        joinPath(path, newName)
      );
      setRenaming(null);
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusyEntry(null);
    }
  };

  const runMkdir = async () => {
    const name = (mkdirDraft ?? "").trim();
    if (!name) {
      setMkdirDraft(null);
      return;
    }
    setBusyEntry({ name, op: "mkdir" });
    setError(null);
    try {
      await fsMkdir(`${host}:${PS5_PAYLOAD_PORT}`, joinPath(path, name));
      setMkdirDraft(null);
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusyEntry(null);
    }
  };

  // Bulk: delete all selected entries. Best-effort — surfaces the first
  // error but continues so a batch where one item fails doesn't abort
  // the rest.
  const runBulkDelete = async () => {
    // Single-flight: refuse if a bulk op is already running. Without
    // this, a tab-switch + return + click sequence could double-fire
    // since the runner is async and outlives the original event.
    if (useFsBulkOpStore.getState().op !== null) return;
    const names = [...selected];
    if (names.length === 0) return;
    if (
      !confirm(
        `Delete ${names.length} item${
          names.length === 1 ? "" : "s"
        } from ${path}?`
      )
    ) {
      return;
    }
    setError(null);
    useFsBulkOpStore.getState().begin({
      op: "delete",
      total: names.length,
      fromPath: path,
      toPath: "",
    });
    let firstError: string | null = null;
    try {
      // Snapshot the per-name sizes from the current entries listing
      // so the busy banner can show "deleting 1.17 GiB foo.ffpkg"
      // even for items the user picked from the directory before
      // this op started — list_dir already gave us sizes; re-stat
      // would be a wasted round trip.
      const sizeByName = new Map<string, number>(
        (entries ?? []).map((e) => [e.name, e.size]),
      );
      for (let i = 0; i < names.length; i++) {
        const name = names[i];
        const itemPath = joinPath(path, name);
        useFsBulkOpStore.getState().setProgress({
          done: i,
          currentPath: itemPath,
          currentName: name,
          currentSize: sizeByName.get(name) ?? null,
        });
        try {
          await fsDelete(`${host}:${PS5_PAYLOAD_PORT}`, itemPath);
        } catch (e) {
          if (!firstError) {
            firstError = `${name}: ${e instanceof Error ? e.message : String(e)}`;
          }
        }
      }
    } finally {
      // Always release the bulk-op store, even if a sync error in
      // the loop body throws. Without this finally, an exception
      // would leave op !== null in the store, and the single-flight
      // guard would permanently block future bulk ops on this
      // screen until reload.
      useFsBulkOpStore.getState().end(firstError);
    }
    if (firstError) setError(firstError);
    await refresh();
  };

  // Cut / Copy: stage selection into the shared clipboard. Clears local
  // selection so the UI reflects that the items are "in flight" via
  // the toolbar instead of by highlighting.
  const stageClipboard = (op: "cut" | "copy") => {
    const items: ClipboardItem[] = selectedEntries.map((e) => ({
      path: joinPath(path, e.name),
      name: e.name,
      size: e.size,
      kind: e.kind === "dir" ? "dir" : "file",
    }));
    if (items.length === 0) return;
    clipboard.set(op, items, path);
    setSelected(new Set());
  };

  // Paste: apply clipboard.op to every item, pasting into `path`.
  //
  // Failure modes tracked explicitly:
  //   - `errors[]`: any item that didn't succeed cleanly. Shown to the
  //     user so they see exactly which items failed and why.
  //   - `duplicated[]`: cut+EXDEV fallback did copy but couldn't delete
  //     the source. The file now lives in both places — flagged
  //     distinctly so the user knows it's not a silent loss.
  //
  // Clipboard clears only when every cut succeeded cleanly. Any
  // failure keeps the clipboard so the user can retry (maybe after
  // freeing space or fixing permissions).
  const runPaste = async () => {
    // Single-flight guard: same rationale as runBulkDelete.
    if (useFsBulkOpStore.getState().op !== null) return;
    if (clipboard.items.length === 0 || !clipboard.op) return;
    const op = clipboard.op;
    const items = clipboard.items;
    setError(null);
    useFsBulkOpStore.getState().begin({
      op: op === "cut" ? "paste-move" : "paste-copy",
      total: items.length,
      fromPath: clipboard.sourceLabel ?? "",
      toPath: path,
    });
    const addr = `${host}:${PS5_PAYLOAD_PORT}`;
    const errors: string[] = [];
    const duplicated: string[] = [];
    try {
      for (let i = 0; i < items.length; i++) {
        const item = items[i];
        const target = joinPath(path, item.name);
        useFsBulkOpStore.getState().setProgress({
          done: i,
          currentPath: item.path,
          currentName: item.name,
          currentSize: item.size ?? null,
        });
        try {
          if (op === "cut") {
            try {
              await fsMove(addr, item.path, target);
            } catch (e) {
              // Cross-filesystem move failure → fall back to copy + delete.
              // The payload emits "fs_move_cross_mount" for EXDEV; matching
              // on substring keeps us resilient to minor wording drift.
              const msg = e instanceof Error ? e.message : String(e);
              if (msg.includes("cross_mount") || msg.includes("EXDEV")) {
                await fsCopy(addr, item.path, target);
                try {
                  await fsDelete(addr, item.path);
                } catch (delErr) {
                  // Copy succeeded, delete failed. The file exists in
                  // both locations — surface distinctly so the user
                  // knows the move was incomplete, not silently lost.
                  duplicated.push(
                    `${item.name} (copied to ${target}, couldn't remove original: ${
                      delErr instanceof Error ? delErr.message : String(delErr)
                    })`
                  );
                }
              } else {
                throw e;
              }
            }
          } else {
            await fsCopy(addr, item.path, target);
          }
        } catch (e) {
          errors.push(
            `${item.name}: ${e instanceof Error ? e.message : String(e)}`
          );
        }
      }
    } finally {
      const problemMsgs = [...duplicated, ...errors];
      // Always release the bulk-op store so the single-flight guard
      // doesn't strand the screen with op !== null after a sync error
      // in the loop body.
      useFsBulkOpStore.getState().end(
        problemMsgs.length > 0 ? problemMsgs.join(" · ") : null,
      );
    }
    const problemMsgs = [...duplicated, ...errors];
    if (problemMsgs.length > 0) {
      setError(problemMsgs.join(" · "));
    }
    // Only clear the clipboard when every cut succeeded cleanly — a
    // partial failure means the user might want to retry the leftover
    // items. Copies always keep the clipboard (same-items-many-places
    // is the usual copy workflow).
    if (op === "cut" && problemMsgs.length === 0) clipboard.clear();
    await refresh();
  };

  /** Save a file or folder under the current dir to the host. Same
   *  flow as Library's per-row Download — pick a host folder, kick
   *  off `transfer_download`, poll jobStatus to terminal. Single-
   *  flight at the screen level: only one download at a time. */
  const runDownload = async (entry: DirEntry) => {
    if (useFsDownloadOpStore.getState().active) return;
    const picked = await openDialog({
      multiple: false,
      directory: true,
      title: tr(
        "fs_download_dialog_title",
        { name: entry.name },
        `Pick a destination folder for "${entry.name}"`,
      ),
    });
    if (typeof picked !== "string") return;
    const addr = `${host}:${PS5_PAYLOAD_PORT}`;
    const remote = joinPath(path, entry.name);
    const kind: "file" | "folder" = entry.kind === "dir" ? "folder" : "file";
    setError(null);
    let jobId: string;
    try {
      jobId = await startTransferDownload(remote, picked, addr, kind);
    } catch (e) {
      const msg =
        e instanceof Error ? e.message : String(e);
      const friendly = tr(
        "fs_download_start_failed",
        { error: msg },
        `Couldn't start the download: ${msg}`,
      );
      setError(friendly);
      useFsDownloadOpStore.getState().end(friendly);
      return;
    }
    const myRunId = useFsDownloadOpStore.getState().begin({
      jobId,
      rootName: entry.name,
      rootSrcPath: remote,
      destDir: picked,
    });
    // Generation-counted abort: the runner captures myRunId and
    // bails when the store's runId moves on (begin() of a new
    // download OR explicit requestStop() from the UI). Without the
    // check, a navigation-away + back + new-download sequence
    // would leave the old runner still polling against the old
    // job and the new runner's setProgress writes would race with
    // it. Bonus: gives the user a way to abort an in-flight pull
    // by clicking Stop in the banner — store.requestStop() flips
    // runId; this loop exits at its next check; the engine job
    // continues server-side and the .part promotion eventually
    // happens (no engine cancel API today).
    const isLive = () => useFsDownloadOpStore.getState().runId === myRunId;
    while (true) {
      if (!isLive()) return;
      try {
        const snap = await jobStatus(jobId);
        if (!isLive()) return;
        if (snap.status === "done") {
          useFsDownloadOpStore.getState().end(null);
          return;
        }
        if (snap.status === "failed") {
          const friendly = tr(
            "fs_download_failed",
            { error: snap.error ?? "download failed" },
            `Download failed: ${snap.error ?? "unknown error"}`,
          );
          setError(friendly);
          useFsDownloadOpStore.getState().end(friendly);
          return;
        }
        useFsDownloadOpStore.getState().setProgress({
          bytesReceived: snap.bytes_sent ?? 0,
          totalBytes: snap.total_bytes ?? 0,
        });
      } catch (e) {
        if (!isLive()) return;
        const msg = e instanceof Error ? e.message : String(e);
        const friendly = tr(
          "fs_download_poll_failed",
          { error: msg },
          `Lost contact with the engine while downloading: ${msg}`,
        );
        setError(friendly);
        useFsDownloadOpStore.getState().end(friendly);
        return;
      }
      await new Promise((resolve) => setTimeout(resolve, 500));
    }
  };

  const selectionActive = selected.size > 0;
  const clipboardActive = clipboard.items.length > 0;

  return (
    <div className="p-6">
      <PageHeader
        icon={FolderTree}
        title={tr("file_system", undefined, "File System")}
        loading={loading}
        right={
          <div className="flex items-center gap-1.5">
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<FolderPlus size={12} />}
              onClick={() => setMkdirDraft("")}
              disabled={loading || !host?.trim()}
            >
              New folder
            </Button>
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<RefreshCw size={12} />}
              onClick={refresh}
              disabled={loading || !host?.trim()}
              loading={loading}
            >
              Refresh
            </Button>
          </div>
        }
      />

      {/* Breadcrumbs + up-button */}
      <div className="mb-3 flex items-center gap-1 overflow-x-auto rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-2 text-xs">
        <button
          type="button"
          onClick={() => setPath(parent(path))}
          disabled={path === "/"}
          title={tr("fs_parent_dir", undefined, "Parent directory")}
          className="rounded-md p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)] disabled:opacity-30"
        >
          <ArrowUp size={14} />
        </button>
        {crumbs(path).map((c, i, arr) => (
          <span key={c.path} className="flex items-center gap-1">
            <button
              type="button"
              onClick={() => setPath(c.path)}
              className={
                "rounded px-1.5 py-0.5 font-mono " +
                (i === arr.length - 1
                  ? "font-medium text-[var(--color-text)]"
                  : "text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]")
              }
            >
              {i === 0 ? <Home size={12} className="inline -translate-y-[1px]" /> : c.label}
            </button>
            {i < arr.length - 1 && (
              <ChevronRight size={12} className="text-[var(--color-muted)]" />
            )}
          </span>
        ))}
      </div>

      {/* Clipboard banner: non-empty clipboard shows what's staged and
          where it came from; paste target is always the current path. */}
      {clipboardActive && (
        <div className="mb-3 flex items-center gap-2 rounded-md border border-[var(--color-accent)] bg-[var(--color-surface-2)] p-2 text-xs">
          <ClipboardPaste size={14} className="text-[var(--color-accent)]" />
          <span>
            <span className="font-medium">
              {tr(
                clipboard.items.length === 1 ? "fs_item_one" : "fs_item_many",
                { count: clipboard.items.length },
                `${clipboard.items.length} item${clipboard.items.length === 1 ? "" : "s"}`,
              )}
            </span>{" "}
            {clipboard.op === "cut"
              ? tr("fs_to_move", undefined, "to move")
              : tr("fs_to_copy", undefined, "to copy")}
            {clipboard.sourceLabel
              ? ` ${tr("fs_from", undefined, "from")} ${clipboard.sourceLabel}`
              : ""}
          </span>
          <div className="ml-auto flex items-center gap-1">
            <button
              type="button"
              onClick={runPaste}
              disabled={bulkOp.op !== null || !host?.trim()}
              className="flex items-center gap-1 rounded-md bg-[var(--color-accent)] px-2 py-1 text-xs font-medium text-[var(--color-accent-contrast)] disabled:opacity-50"
            >
              {tr("fs_paste_here", undefined, "Paste here")}
            </button>
            <button
              type="button"
              onClick={() => clipboard.clear()}
              className="rounded-md border border-[var(--color-border)] p-1 hover:bg-[var(--color-surface-3)]"
              title={tr("fs_cancel_clipboard", undefined, "Cancel — forget the staged items")}
            >
              <X size={12} />
            </button>
          </div>
        </div>
      )}

      {/* Selection toolbar: only visible with ≥1 selected. */}
      {selectionActive && (
        <div className="mb-3 flex items-center gap-2 rounded-md border border-[var(--color-accent)] bg-[var(--color-surface-2)] p-2 text-xs">
          <span className="font-medium">
            {tr("fs_selected_count", { count: selected.size }, `${selected.size} selected`)}
          </span>
          <button
            type="button"
            onClick={() => stageClipboard("cut")}
            disabled={bulkOp.op !== null}
            className="flex items-center gap-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 hover:bg-[var(--color-surface-3)] disabled:opacity-50"
          >
            <Scissors size={12} />
            {tr("fs_cut", undefined, "Cut")}
          </button>
          <button
            type="button"
            onClick={() => stageClipboard("copy")}
            disabled={bulkOp.op !== null}
            className="flex items-center gap-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 hover:bg-[var(--color-surface-3)] disabled:opacity-50"
          >
            <Copy size={12} />
            {tr("copy", undefined, "Copy")}
          </button>
          <button
            type="button"
            onClick={runBulkDelete}
            disabled={bulkOp.op !== null}
            className="flex items-center gap-1 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface)] px-2 py-1 text-[var(--color-bad)] hover:bg-[var(--color-surface-3)] disabled:opacity-50"
          >
            <Trash2 size={12} />
            {tr("library_delete", undefined, "Delete")}
          </button>
          <button
            type="button"
            onClick={() => setSelected(new Set())}
            className="ml-auto rounded-md border border-[var(--color-border)] p-1 hover:bg-[var(--color-surface-3)]"
            title={tr("fs_clear_selection", undefined, "Clear selection")}
          >
            <X size={12} />
          </button>
        </div>
      )}

      {mkdirDraft !== null && (
        <div className="mb-3 flex items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-2 text-sm">
          <FolderPlus size={14} className="text-[var(--color-muted)]" />
          <span className="text-xs text-[var(--color-muted)]">
            {tr("fs_create_in", undefined, "Create in")} <span className="font-mono">{path}</span>
          </span>
          <input
            autoFocus
            value={mkdirDraft}
            placeholder={tr("fs_folder_name", undefined, "folder name")}
            onChange={(e) => setMkdirDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") runMkdir();
              if (e.key === "Escape") setMkdirDraft(null);
            }}
            className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-xs"
          />
          <button
            type="button"
            onClick={runMkdir}
            className="rounded-md bg-[var(--color-accent)] px-3 py-1 text-xs font-medium text-[var(--color-accent-contrast)]"
          >
            {tr("fs_create", undefined, "Create")}
          </button>
          <button
            type="button"
            onClick={() => setMkdirDraft(null)}
            className="rounded-md border border-[var(--color-border)] px-3 py-1 text-xs hover:bg-[var(--color-surface-3)]"
          >
            {tr("cancel", undefined, "Cancel")}
          </button>
        </div>
      )}

      {bulkOp.op !== null && (
        <BulkOpBanner
          op={bulkOp.op}
          total={bulkOp.total}
          done={bulkOp.done}
          currentName={bulkOp.currentName}
          currentSize={bulkOp.currentSize}
          fromPath={bulkOp.fromPath}
          toPath={bulkOp.toPath}
          startedAtMs={bulkOp.startedAtMs}
        />
      )}

      {busyEntry && bulkOp.op === null && (
        <div className="mb-3 flex items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-2 text-xs">
          <Loader2 size={12} className="animate-spin text-[var(--color-accent)]" />
          <span className="font-medium">
            {busyEntry.op === "rename"
              ? tr("fs_busy_renaming", undefined, "Renaming")
              : tr("fs_busy_creating_folder", undefined, "Creating folder")}
          </span>
          <span className="text-[var(--color-muted)]">
            {busyEntry.name} · {formatDuration(elapsedMs / 1000)}
          </span>
        </div>
      )}

      {downloadOp.active && (
        <DownloadOpBanner
          rootName={downloadOp.rootName}
          rootSrcPath={downloadOp.rootSrcPath}
          destDir={downloadOp.destDir}
          bytesReceived={downloadOp.bytesReceived}
          totalBytes={downloadOp.totalBytes}
          startedAtMs={downloadOp.startedAtMs}
        />
      )}

      {error && (
        <div className="mb-3 flex items-start gap-2 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface-2)] p-2 text-xs">
          <AlertTriangle size={12} className="mt-0.5 text-[var(--color-bad)]" />
          <span>{error}</span>
        </div>
      )}

      {entries === null && !loading && !error && (
        <div className="rounded-md border border-dashed border-[var(--color-border)] p-4 text-center text-xs text-[var(--color-muted)]">
          Connect to your PS5 first — use the Connection tab.
        </div>
      )}

      {entries && entries.length === 0 && (
        <div className="rounded-md border border-dashed border-[var(--color-border)] p-4 text-center text-xs text-[var(--color-muted)]">
          Empty folder.
        </div>
      )}

      {entries && entries.length > 0 && (
        <div className="mb-1 flex items-center gap-2 px-2 text-xs text-[var(--color-muted)]">
          <input
            type="checkbox"
            checked={selected.size > 0 && selected.size === entries.length}
            ref={(el) => {
              if (el) el.indeterminate =
                selected.size > 0 && selected.size < entries.length;
            }}
            onChange={toggleAll}
            className="h-3.5 w-3.5 rounded border-[var(--color-border)]"
            title={tr("fs_select_all", undefined, "Select all")}
          />
          <span>
            {tr(
              entries.length === 1 ? "fs_item_one" : "fs_item_many",
              { count: entries.length },
              `${entries.length} item${entries.length === 1 ? "" : "s"}`,
            )}
          </span>
        </div>
      )}

      <ul className="grid gap-1">
        {entries?.map((e) => {
          const isDir = e.kind === "dir";
          const Icon = isDir ? Folder : FileIcon;
          const isRenaming = renaming === e.name;
          const isSelected = selected.has(e.name);
          return (
            <li
              key={e.name}
              className={
                "flex items-center gap-3 rounded-md border p-2 text-sm " +
                (isSelected
                  ? "border-[var(--color-accent)] bg-[var(--color-surface-2)]"
                  : "border-[var(--color-border)] bg-[var(--color-surface-2)]")
              }
            >
              <input
                type="checkbox"
                checked={isSelected}
                onChange={() => toggleSelected(e.name)}
                className="h-3.5 w-3.5 shrink-0 rounded border-[var(--color-border)]"
              />
              <Icon
                size={16}
                className={
                  isDir
                    ? "shrink-0 text-[var(--color-accent)]"
                    : "shrink-0 text-[var(--color-muted)]"
                }
              />
              {isRenaming ? (
                <input
                  autoFocus
                  value={renameDraft}
                  onChange={(ev) => setRenameDraft(ev.target.value)}
                  onKeyDown={(ev) => {
                    if (ev.key === "Enter") runRename(e.name);
                    if (ev.key === "Escape") setRenaming(null);
                  }}
                  onBlur={() => setRenaming(null)}
                  className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-0.5 text-sm"
                />
              ) : (
                <button
                  type="button"
                  onClick={() => {
                    if (isDir) setPath(joinPath(path, e.name));
                  }}
                  className={
                    "flex-1 truncate text-left font-mono text-sm " +
                    (isDir ? "hover:text-[var(--color-accent)]" : "")
                  }
                  disabled={!isDir}
                >
                  {e.name}
                </button>
              )}
              <span className="shrink-0 text-xs text-[var(--color-muted)] tabular-nums">
                {isDir ? "—" : formatBytes(e.size)}
              </span>
              <div className="ml-2 flex shrink-0 items-center gap-1">
                <button
                  type="button"
                  onClick={() => runDownload(e)}
                  disabled={downloadOp.active}
                  title={tr(
                    "fs_download_tooltip",
                    undefined,
                    "Save a copy of this entry to a folder on this computer",
                  )}
                  className="rounded-md border border-[var(--color-border)] p-1 hover:bg-[var(--color-surface-3)] disabled:opacity-30"
                >
                  {downloadOp.active && downloadOp.rootName === e.name ? (
                    <Loader2
                      size={12}
                      className="animate-spin text-[var(--color-accent)]"
                    />
                  ) : (
                    <Download size={12} />
                  )}
                </button>
                <button
                  type="button"
                  onClick={() => {
                    setRenaming(e.name);
                    setRenameDraft(e.name);
                  }}
                  title={tr("fs_rename", undefined, "Rename")}
                  className="rounded-md border border-[var(--color-border)] p-1 hover:bg-[var(--color-surface-3)]"
                >
                  <Pencil size={12} />
                </button>
                <button
                  type="button"
                  onClick={() => runDelete(e.name)}
                  title={tr("library_delete", undefined, "Delete")}
                  className="rounded-md border border-[var(--color-bad)] p-1 text-[var(--color-bad)] hover:bg-[var(--color-surface-3)]"
                >
                  <Trash2 size={12} />
                </button>
              </div>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/**
 * Rich progress banner for bulk file ops (delete, paste-move, paste-copy).
 *
 * Shows the per-file size + source/destination paths so users know
 * what's actually moving and where it's going. The progress bar is
 * "determinate by file count" but the per-file copy is still an
 * opaque payload-side fs_copy — for a single 10 GiB file the bar
 * sits at 0% for the whole copy. We layer an indeterminate animated
 * stripe on top so the bar still LOOKS alive during long single-
 * file copies. Real byte-level progress would need payload-side
 * incremental fs_copy events; future work.
 */
function BulkOpBanner({
  op,
  total,
  done,
  currentName,
  currentSize,
  fromPath,
  toPath,
  startedAtMs,
}: {
  op: "delete" | "paste-move" | "paste-copy";
  total: number;
  done: number;
  currentName: string;
  currentSize: number | null;
  fromPath: string;
  toPath: string;
  startedAtMs: number;
}) {
  const tr = useTr();
  const [now, setNow] = useState<number>(() => Date.now());
  useEffect(() => {
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(id);
  }, []);
  const elapsedSec = startedAtMs > 0 ? Math.max(0, (now - startedAtMs) / 1000) : 0;
  const pctByFiles = total > 0 ? Math.min(100, (done / total) * 100) : 0;
  const verb =
    op === "delete"
      ? tr("library_busy_delete", undefined, "Deleting")
      : op === "paste-move"
        ? tr("fs_busy_moving", undefined, "Moving")
        : tr("fs_busy_copying", undefined, "Copying");

  return (
    <div className="mb-3 rounded-md border border-[var(--color-accent)] bg-[var(--color-surface-2)] p-3 text-xs">
      <div className="mb-2 flex items-center gap-2">
        <Loader2 size={14} className="animate-spin text-[var(--color-accent)]" />
        <span className="font-semibold">{verb}</span>
        <span className="text-[var(--color-muted)]">
          {tr(
            "fs_bulk_progress",
            { done: done + 1, total },
            `${done + 1} of ${total}`,
          )}
          {" · "}
          {formatDuration(elapsedSec)}
        </span>
      </div>

      {currentName && (
        <div className="mb-2 flex flex-wrap items-baseline gap-x-3 gap-y-0.5 font-mono text-[11px]">
          <span className="text-[var(--color-text)]">{currentName}</span>
          {currentSize !== null && currentSize > 0 && (
            <span className="text-[var(--color-muted)]">
              {formatBytes(currentSize)}
            </span>
          )}
        </div>
      )}

      {(fromPath || toPath) && (
        <div className="mb-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 text-[11px] text-[var(--color-muted)]">
          {fromPath && (
            <>
              <span>{tr("fs_bulk_from", undefined, "From")}</span>
              <span className="break-all font-mono">{fromPath}</span>
            </>
          )}
          {toPath && op !== "delete" && (
            <>
              <span>{tr("fs_bulk_to", undefined, "To")}</span>
              <span className="break-all font-mono">{toPath}</span>
            </>
          )}
        </div>
      )}

      {/* Determinate-by-file-count bar with a subtle pulse so single-
          file copies (where the bar sits at 0% the whole copy) still
          LOOK alive. The spinner icon at top is the secondary
          motion signal. Real byte-level progress would need
          payload-side incremental fs_copy events; future work. */}
      <div className="h-1.5 w-full overflow-hidden rounded-full bg-[var(--color-surface-3)]">
        <div
          className={`h-full bg-[var(--color-accent)] transition-[width] duration-300 ${
            done < total ? "animate-pulse" : ""
          }`}
          style={{ width: `${Math.max(pctByFiles, 4)}%` }}
        />
      </div>
    </div>
  );
}

/**
 * Rich progress banner for downloads. Similar shape to BulkOpBanner
 * but with real bytes/total since download progresses are streamed
 * (the engine sums fs_read chunks into the shared progress AtomicU64).
 */
function DownloadOpBanner({
  rootName,
  rootSrcPath,
  destDir,
  bytesReceived,
  totalBytes,
  startedAtMs,
}: {
  rootName: string;
  rootSrcPath: string;
  destDir: string;
  bytesReceived: number;
  totalBytes: number;
  startedAtMs: number;
}) {
  const tr = useTr();
  const [now, setNow] = useState<number>(() => Date.now());
  useEffect(() => {
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(id);
  }, []);
  const elapsedSec = startedAtMs > 0 ? Math.max(0, (now - startedAtMs) / 1000) : 0;
  const pct = totalBytes > 0 ? Math.min(100, (bytesReceived / totalBytes) * 100) : 0;
  const speed = elapsedSec > 0 ? bytesReceived / elapsedSec : 0;

  return (
    <div className="mb-3 rounded-md border border-[var(--color-accent)] bg-[var(--color-surface-2)] p-3 text-xs">
      <div className="mb-2 flex items-center gap-2">
        <Loader2 size={14} className="animate-spin text-[var(--color-accent)]" />
        <span className="font-semibold">
          {tr("fs_busy_downloading", undefined, "Downloading from PS5")}
        </span>
        <span className="text-[var(--color-muted)]">
          {formatDuration(elapsedSec)}
        </span>
      </div>

      <div className="mb-2 flex flex-wrap items-baseline gap-x-3 gap-y-0.5 font-mono text-[11px]">
        <span>{rootName}</span>
        {totalBytes > 0 ? (
          <span className="text-[var(--color-muted)]">
            {formatBytes(bytesReceived)} / {formatBytes(totalBytes)} ({pct.toFixed(0)}%)
          </span>
        ) : (
          <span className="text-[var(--color-muted)]">
            {formatBytes(bytesReceived)}
          </span>
        )}
        {speed > 0 && (
          <span className="text-[var(--color-muted)]">
            {formatBytes(speed)}/s
          </span>
        )}
      </div>

      <div className="mb-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 text-[11px] text-[var(--color-muted)]">
        <span>{tr("fs_bulk_from", undefined, "From")}</span>
        <span className="break-all font-mono">{rootSrcPath}</span>
        <span>{tr("fs_bulk_to", undefined, "To")}</span>
        <span className="break-all font-mono">{destDir}</span>
      </div>

      <div className="h-1.5 w-full overflow-hidden rounded-full bg-[var(--color-surface-3)]">
        <div
          className="h-full bg-[var(--color-accent)] transition-[width] duration-300"
          style={{ width: `${pct}%` }}
        />
      </div>
    </div>
  );
}
