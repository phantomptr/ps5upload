import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";
import {
  Check,
  ChevronUp,
  File as FileIcon,
  Folder,
  HardDrive,
  Lock,
  PackagePlus,
  RefreshCw,
  Send,
  X,
  ScanSearch,
} from "lucide-react";

import { Button } from ".";
import { localFs } from "../api/localFs";
import { remoteApi, RemoteApiError } from "../api/remote";
import { parentOf, fmtSize } from "../lib/pathBrowser";
import { displayPath, remotePath } from "../lib/remotePath";
import { hostOf, transferAddr } from "../lib/addr";
import { fsListDir } from "../api/ps5";
import { useConnectionsStore } from "../state/connections";
import { useLocalPickerStore } from "../state/localPicker";
import { usePackageViewer } from "../state/packageViewer";
import { useTr } from "../state/lang";

/**
 * Global in-app file/folder browser. Mounted once at the app root; driven
 * by the localPicker store (screens call `pickLocalPath({ mode })`).
 *
 * Two jobs. On Android, scoped storage makes `plugin-dialog`'s directory
 * picker a no-op and its file picker return `content://` URIs, neither of
 * which the engine (it walks real `std::fs` paths) can use; with all-files
 * access granted we browse the real filesystem via `local_list_dir` instead.
 * And on every platform it browses a saved server (SMB, FTP, SFTP) through
 * the engine, resolving with a `remote://` path.
 */

export interface PickerEntry {
  name: string;
  /** Local: the full path. Server: the server path ("/games/a.pkg"). */
  path: string;
  is_dir: boolean;
  size: number;
}

export interface PickerViewProps {
  title: string;
  mode: "file" | "folder" | "any";
  entries: PickerEntry[];
  cwdLabel: string;
  canGoUp: boolean;
  loading: boolean;
  error: string | null;
  hint: string | null;
  hasMore: boolean;
  filters?: { name: string; extensions: string[] }[];
  /** Android grant state (local only). */
  granted: boolean | null;
  roots: string[];
  currentRoot?: string;
  remote: boolean;
  actions: boolean;
  onOpenDir: (path: string) => void;
  onPickFile: (path: string) => void;
  onUp: () => void;
  onUseFolder: () => void;
  onLoadMore: () => void;
  onRetry: () => void;
  onEditConnection: () => void;
  onInstall: (path: string) => void;
  /** Open a file in the package viewer (Connections → Browse). */
  onView?: (path: string) => void;
  onSend: (path: string) => void;
  onCancel: () => void;
  onRoot: (root: string) => void;
  onRequestAccess: () => void;
  /** Several at once: pickable rows get a checkbox, kept across folders. */
  multiple?: boolean;
  selected?: string[];
  onToggle?: (path: string) => void;
  onAddSelected?: () => void;
}

function passes(name: string, filters?: { extensions: string[] }[]): boolean {
  const exts = (filters ?? []).flatMap((f) => f.extensions.map((e) => e.toLowerCase()));
  if (exts.length === 0 || exts.includes("*")) return true;
  const dot = name.lastIndexOf(".");
  return dot > 0 && exts.includes(name.slice(dot + 1).toLowerCase());
}

export function PickerView(p: PickerViewProps) {
  const tr = useTr();
  const shown = p.entries.filter((e) => e.is_dir || passes(e.name, p.filters));
  return (
    <div
      className="anim-scrim scrim fixed inset-0 z-50 flex items-end justify-center sm:items-center"
      style={{ paddingTop: "env(safe-area-inset-top)", paddingBottom: "var(--safe-bottom)" }}
      onClick={p.onCancel}
    >
      <div
        role="dialog"
        aria-modal="true"
        className="anim-sheet glass-float elev-3 flex max-h-[88dvh] w-full max-w-lg flex-col rounded-t-[var(--radius-panel)] sm:max-h-[80dvh] sm:rounded-[var(--radius-panel)]"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="flex items-center gap-2 border-b border-[var(--color-border)] px-4 py-3">
          <span className="flex-1 truncate text-sm font-semibold">{p.title}</span>
          <button
            type="button"
            aria-label={tr("cancel", undefined, "Cancel")}
            className="rounded-full p-1.5 text-[var(--color-muted)] hover:bg-[var(--color-surface)]"
            onClick={p.onCancel}
          >
            <X size={18} />
          </button>
        </header>

        {p.granted === false ? (
          <div className="flex flex-col items-center gap-3 px-5 py-8 text-center">
            <Lock size={28} className="text-[var(--color-muted)]" />
            <p className="text-sm font-medium">
              {tr("picker_grant_title", undefined, "Allow access to your files")}
            </p>
            <p className="text-xs text-[var(--color-muted)]">
              {tr(
                "picker_grant_hint",
                undefined,
                "To upload a game folder or .zip, PS5 Upload needs permission to read your files. Tap below, enable “Allow access to manage all files”, then come back and tap Retry.",
              )}
            </p>
            <div className="mt-2 flex gap-2">
              <Button variant="primary" size="sm" onClick={p.onRequestAccess}>
                {tr("picker_open_settings", undefined, "Open settings")}
              </Button>
              <Button variant="ghost" size="sm" onClick={p.onRetry}>
                <RefreshCw size={14} />
                {tr("picker_retry", undefined, "Retry")}
              </Button>
            </div>
          </div>
        ) : (
          <>
            <div className="flex items-center gap-2 border-b border-[var(--color-border)] px-3 py-2">
              <button
                type="button"
                disabled={!p.canGoUp}
                aria-label={tr("picker_up", undefined, "Up one level")}
                className="rounded p-1.5 text-[var(--color-muted)] hover:bg-[var(--color-surface)] disabled:opacity-30"
                onClick={p.onUp}
              >
                <ChevronUp size={18} />
              </button>
              <span className="flex-1 truncate text-xs text-[var(--color-muted)]">{p.cwdLabel}</span>
              {!p.remote && p.roots.length > 1 && (
                <select
                  className="max-w-[40%] truncate rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-1 py-0.5 text-xs"
                  value={p.currentRoot ?? ""}
                  onChange={(e) => e.target.value && p.onRoot(e.target.value)}
                >
                  <option value="" disabled>
                    {tr("picker_roots", undefined, "Storage")}
                  </option>
                  {p.roots.map((r) => (
                    <option key={r} value={r}>
                      {r}
                    </option>
                  ))}
                </select>
              )}
            </div>

            <div className="min-h-0 flex-1 overflow-y-auto">
              {p.loading && shown.length === 0 ? (
                <div className="px-4 py-8 text-center text-xs text-[var(--color-muted)]">
                  {tr("picker_loading", undefined, "Loading…")}
                </div>
              ) : p.error ? (
                <div className="flex flex-col items-center gap-2 px-4 py-8 text-center text-xs">
                  <span className="text-[var(--color-bad)]">{p.error}</span>
                  {p.hint && <span className="text-[var(--color-muted)]">{p.hint}</span>}
                  <div className="mt-1 flex gap-2">
                    <Button variant="ghost" size="sm" onClick={p.onRetry}>
                      <RefreshCw size={14} />
                      {tr("picker_retry", undefined, "Retry")}
                    </Button>
                    {p.remote && (
                      <Button variant="ghost" size="sm" onClick={p.onEditConnection}>
                        {tr("picker_edit_connection", undefined, "Edit connection")}
                      </Button>
                    )}
                  </div>
                </div>
              ) : shown.length === 0 ? (
                <div className="flex flex-col items-center gap-2 px-4 py-8 text-center text-xs text-[var(--color-muted)]">
                  <HardDrive size={20} />
                  {tr("picker_empty", undefined, "This folder is empty.")}
                </div>
              ) : (
                <ul>
                  {shown.map((e) => {
                    const selectable = e.is_dir || p.mode !== "folder";
                    const isPkg = !e.is_dir && e.name.toLowerCase().endsWith(".pkg");
                    const tickable = p.multiple && (e.is_dir ? p.mode !== "file" : p.mode !== "folder");
                    return (
                      <li key={e.path} className="flex items-center">
                        {tickable && (
                          <input
                            type="checkbox"
                            className="ms-4 h-4 w-4 shrink-0"
                            checked={p.selected?.includes(e.path) ?? false}
                            onChange={() => p.onToggle?.(e.path)}
                            aria-label={tr("batch_include", { name: e.name }, "Include {name}")}
                          />
                        )}
                        <button
                          type="button"
                          disabled={!selectable && !p.actions}
                          className="flex min-w-0 flex-1 items-center gap-3 px-4 py-3 text-left hover:bg-[var(--color-surface)] disabled:opacity-40"
                          onClick={() => {
                            if (e.is_dir) p.onOpenDir(e.path);
                            else if (p.mode !== "folder" && !p.actions) {
                              if (p.multiple) p.onToggle?.(e.path);
                              else p.onPickFile(e.path);
                            }
                          }}
                        >
                          {e.is_dir ? (
                            <Folder size={18} className="shrink-0 text-[var(--color-accent)]" />
                          ) : (
                            <FileIcon size={18} className="shrink-0 text-[var(--color-muted)]" />
                          )}
                          <span className="flex-1 truncate text-sm">{e.name}</span>
                          {!e.is_dir && (
                            <span className="shrink-0 text-xs text-[var(--color-muted)]">
                              {fmtSize(e.size)}
                            </span>
                          )}
                        </button>
                        {p.actions && !e.is_dir && (
                          <span className="flex shrink-0 gap-1 pe-3">
                            {p.onView && /\.(f?pkg|exfat|ffpkg|ffpfsc)$/i.test(e.name) && (
                              <Button variant="ghost" size="sm" onClick={() => p.onView!(e.path)}>
                                <ScanSearch size={14} />
                                {tr("viewer_related_view", undefined, "View")}
                              </Button>
                            )}
                            {isPkg && (
                              <Button variant="ghost" size="sm" onClick={() => p.onInstall(e.path)}>
                                <PackagePlus size={14} />
                                {tr("picker_install", undefined, "Install")}
                              </Button>
                            )}
                            <Button variant="ghost" size="sm" onClick={() => p.onSend(e.path)}>
                              <Send size={14} />
                              {tr("picker_send", undefined, "Send to PS5")}
                            </Button>
                          </span>
                        )}
                      </li>
                    );
                  })}
                  {p.hasMore && (
                    <li>
                      <button
                        type="button"
                        disabled={p.loading}
                        className="w-full px-4 py-3 text-center text-xs text-[var(--color-accent)] hover:bg-[var(--color-surface)]"
                        onClick={p.onLoadMore}
                      >
                        {p.loading
                          ? tr("picker_loading", undefined, "Loading…")
                          : tr("picker_load_more", undefined, "Load more")}
                      </button>
                    </li>
                  )}
                </ul>
              )}
            </div>

            {(p.mode !== "file" || p.multiple) && !p.actions && (
              <footer className="flex flex-wrap items-center gap-2 border-t border-[var(--color-border)] px-4 py-3">
                <span
                  className={`min-w-0 flex-1 text-xs text-[var(--color-muted)] ${p.mode !== "file" ? "truncate" : ""}`}
                >
                  {p.mode !== "file" ? (
                    <>
                      {tr("picker_use_this", undefined, "Use the open folder:")}{" "}
                      <span className="font-medium text-[var(--color-text)]">{p.cwdLabel}</span>
                    </>
                  ) : (
                    tr("picker_tick_hint", undefined, "Tick files in any folder, then add them together.")
                  )}
                </span>
                {/* With folders ticked, "Use this folder" would drop them: Add is the one action. */}
                {p.mode !== "file" && !(p.multiple && p.selected?.length) && (
                  <Button variant={p.multiple ? "secondary" : "primary"} size="sm" onClick={p.onUseFolder}>
                    <Check size={14} />
                    {tr("picker_use_folder", undefined, "Use this folder")}
                  </Button>
                )}
                {p.multiple && (
                  <Button
                    variant="primary"
                    size="sm"
                    disabled={!p.selected?.length}
                    onClick={p.onAddSelected}
                  >
                    <Check size={14} />
                    {tr("picker_add_selected", { count: p.selected?.length ?? 0 }, "Add {count}")}
                  </Button>
                )}
              </footer>
            )}
          </>
        )}
      </div>
    </div>
  );
}

const LAST_DIR_KEY = "ps5upload.remoteLastDir.";

function readLastDir(id: string): string | null {
  try {
    return window.localStorage.getItem(LAST_DIR_KEY + id);
  } catch {
    return null;
  }
}

function writeLastDir(id: string, dir: string) {
  try {
    window.localStorage.setItem(LAST_DIR_KEY + id, dir);
  } catch {
    /* a remembered folder is a convenience */
  }
}

/** "/games/ps5" → "/games"; "/" has no parent. */
function serverParent(path: string): string | null {
  const t = path.replace(/\/+$/, "");
  if (!t) return null;
  const i = t.lastIndexOf("/");
  return i <= 0 ? "/" : t.slice(0, i);
}

/** Where a game can live on the console: internal storage and the external drives. */
const CONSOLE_ROOTS = ["/data", "/mnt/ext0", "/mnt/ext1", "/mnt/usb0", "/mnt/usb1"];

/** A pick on the console, as the converter reads it: `ps5://<host><path>`. */
export function consolePickResult(host: string, path: string): string {
  return `ps5://${hostOf(host)}${path.startsWith("/") ? path : `/${path}`}`;
}

function joinServer(dir: string, name: string): string {
  return `${dir.replace(/\/+$/, "")}/${name}`;
}

/** Whether the picker steps aside for the package viewer. It does only for a View pressed in
 *  the picker itself, and only while that viewer is open: closing the viewer returns to the
 *  picker in the folder it was left in, instead of making the user find the folder again. */
export function pickerHiddenForViewer(viewingFromPicker: boolean, viewerOpen: boolean): boolean {
  return viewingFromPicker && viewerOpen;
}

export function LocalPathPicker() {
  const tr = useTr();
  const viewerOpen = usePackageViewer((s) => s.request !== null);
  const [viewingFromPicker, setViewingFromPicker] = useState(false);
  const navigate = useNavigate();
  const pending = useLocalPickerStore((s) => s.pending);
  const settle = useLocalPickerStore((s) => s.settle);
  const settleMany = useLocalPickerStore((s) => s.settleMany);
  const connections = useConnectionsStore((s) => s.connections);

  const source = pending?.source;
  const connectionId = source && source !== "local" && "connectionId" in source ? source.connectionId : null;
  const consoleHost = source && source !== "local" && "console" in source ? source.console : null;
  const connection = connectionId ? connections.find((c) => c.id === connectionId) : undefined;

  const [granted, setGranted] = useState<boolean | null>(null);
  const [roots, setRoots] = useState<string[]>([]);
  const [cwd, setCwd] = useState<string | null>(null);
  const [entries, setEntries] = useState<PickerEntry[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [hint, setHint] = useState<string | null>(null);
  /** A multi-pick's ticked paths, in the order ticked (raw, before the source prefix). */
  const [selected, setSelected] = useState<string[]>([]);

  const loadDir = useCallback(
    async (path: string, more?: string) => {
      setLoading(true);
      setError(null);
      setHint(null);
      try {
        if (consoleHost) {
          const list = await fsListDir(transferAddr(consoleHost), path);
          setEntries(
            list.map((e) => ({
              name: e.name,
              path: joinServer(path, e.name),
              is_dir: e.kind === "dir",
              size: e.size ?? 0,
            })),
          );
          setCursor(null);
        } else if (connectionId) {
          const page = await remoteApi.listDir(remotePath(connectionId, path), more);
          const list = page.entries.map((e) => ({
            name: e.name,
            path: joinServer(path, e.name),
            is_dir: e.is_dir,
            size: e.size,
          }));
          setEntries((prev) => (more ? [...prev, ...list] : list));
          setCursor(page.next_cursor);
          writeLastDir(connectionId, path);
        } else {
          setEntries(await localFs.listDir(path));
          setCursor(null);
        }
        setCwd(path);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
        setHint(e instanceof RemoteApiError ? (e.hint ?? null) : null);
        setCwd(path);
      } finally {
        setLoading(false);
      }
    },
    [connectionId, consoleHost],
  );

  const begin = useCallback(async () => {
    setError(null);
    setHint(null);
    setLoading(true);
    if (consoleHost) {
      setGranted(true);
      setRoots(CONSOLE_ROOTS);
      await loadDir(CONSOLE_ROOTS[0]);
      return;
    }
    if (connectionId) {
      setGranted(true);
      const start = readLastDir(connectionId) || connection?.start_path || "/";
      await loadDir(start.startsWith("/") ? start : `/${start}`);
      return;
    }
    setGranted(null);
    try {
      const ok = await localFs.accessGranted();
      setGranted(ok);
      if (!ok) {
        setLoading(false);
        return;
      }
      const rs = await localFs.storageRoots();
      setRoots(rs);
      if (rs.length) await loadDir(rs[0]);
      else setLoading(false);
    } catch (e) {
      setError(String(e));
      setGranted(true); // don't trap the user on a probe failure
      setLoading(false);
    }
  }, [connectionId, consoleHost, connection?.start_path, loadDir]);

  // Each time a new request opens, (re)start the browser.
  useEffect(() => {
    if (pending) {
      setCwd(null);
      setEntries([]);
      setCursor(null);
      setSelected([]);
      void begin();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pending]);

  // Esc cancels.
  useEffect(() => {
    if (!pending) return;
    const h = (e: KeyboardEvent) => {
      // While the package viewer is open on top, Esc is the viewer's: it must not also
      // close the picker waiting underneath.
      if (e.key === "Escape" && usePackageViewer.getState().request === null) {
        e.preventDefault();
        settle(null);
      }
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [pending, settle]);

  // The viewer opened from here has closed: the flag has done its job. (Derived visibility,
  // so no effect is needed to "re-show" the picker.)
  const hidden = pickerHiddenForViewer(viewingFromPicker, viewerOpen);
  if (!pending) return null;
  if (hidden) return null;

  const parent = cwd ? (connectionId || consoleHost ? serverParent(cwd) : parentOf(cwd)) : null;
  const resultOf = (path: string) =>
    consoleHost
      ? consolePickResult(consoleHost, path)
      : connectionId
        ? remotePath(connectionId, path)
        : path;
  const finish = (path: string) => settle(resultOf(path));
  const nameOf = (id: string) => connections.find((c) => c.id === id)?.name;
  const cwdLabel = cwd
    ? consoleHost
      ? `PS5 › ${cwd}`
      : connectionId
        ? displayPath(remotePath(connectionId, cwd), nameOf)
        : cwd
    : "…";

  return (
    <PickerView
      title={
        pending.title ??
        (pending.mode === "folder"
          ? tr("picker_choose_folder", undefined, "Choose a folder")
          : tr("picker_choose_file", undefined, "Choose a file"))
      }
      mode={pending.mode}
      entries={entries}
      cwdLabel={cwdLabel}
      canGoUp={!!parent}
      loading={loading}
      error={error}
      hint={hint}
      hasMore={!!cursor}
      filters={pending.filters}
      granted={granted}
      roots={roots}
      currentRoot={roots.includes(cwd ?? "") ? (cwd ?? "") : ""}
      remote={!!connectionId}
      actions={!!pending.actions}
      onOpenDir={(p) => void loadDir(p)}
      onPickFile={finish}
      onUp={() => parent && void loadDir(parent)}
      onUseFolder={() => cwd && finish(cwd)}
      onLoadMore={() => cwd && cursor && void loadDir(cwd, cursor)}
      onRetry={() => (cwd ? void loadDir(cwd) : void begin())}
      onEditConnection={() => {
        settle(null);
        if (connectionId) navigate(`/connections?edit=${encodeURIComponent(connectionId)}`);
      }}
      onInstall={(p) => {
        const act = pending.actions;
        settle(null);
        if (act && connectionId) act.onInstall(remotePath(connectionId, p));
      }}
      onView={(p) => {
        if (!connectionId) return;
        // The picker stays open underneath, with its folder: see pickerHiddenForViewer.
        setViewingFromPicker(true);
        usePackageViewer.getState().open(remotePath(connectionId, p));
      }}
      onSend={(p) => {
        const act = pending.actions;
        settle(null);
        if (act && connectionId) act.onSend(remotePath(connectionId, p));
      }}
      onCancel={() => settle(null)}
      onRoot={(r) => void loadDir(r)}
      onRequestAccess={() => void localFs.requestAccess().catch(() => {})}
      multiple={!!pending.multiple}
      selected={selected}
      onToggle={(p) =>
        setSelected((cur) => (cur.includes(p) ? cur.filter((x) => x !== p) : [...cur, p]))
      }
      onAddSelected={() => selected.length && settleMany(selected.map(resultOf))}
    />
  );
}
