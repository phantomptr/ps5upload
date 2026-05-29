import { useCallback, useEffect, useState } from "react";
import {
  Check,
  ChevronUp,
  File as FileIcon,
  Folder,
  HardDrive,
  Lock,
  RefreshCw,
  X,
} from "lucide-react";

import { Button } from ".";
import { localFs, type LocalEntry } from "../api/localFs";
import { parentOf, fmtSize } from "../lib/pathBrowser";
import { useLocalPickerStore } from "../state/localPicker";
import { useTr } from "../state/lang";

/**
 * Global in-app file/folder browser. Mounted once at the app root; driven
 * by the localPicker store (screens call `pickLocalPath({ mode })`).
 *
 * Why this exists: Android scoped storage makes `plugin-dialog`'s
 * directory picker a no-op and its file picker return `content://` URIs,
 * neither of which the engine (it walks real `std::fs` paths) can use —
 * and copying a multi-GB game into app cache is impractical. With
 * all-files access granted, we browse the real filesystem via
 * `local_list_dir` so the user picks a real path the engine reads
 * directly: folders and big `.zip`s included, no copying.
 */

export function LocalPathPicker() {
  const tr = useTr();
  const pending = useLocalPickerStore((s) => s.pending);
  const settle = useLocalPickerStore((s) => s.settle);

  const [granted, setGranted] = useState<boolean | null>(null);
  const [roots, setRoots] = useState<string[]>([]);
  const [cwd, setCwd] = useState<string | null>(null);
  const [entries, setEntries] = useState<LocalEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadDir = useCallback(async (path: string) => {
    setLoading(true);
    setError(null);
    try {
      const list = await localFs.listDir(path);
      setEntries(list);
      setCwd(path);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const begin = useCallback(async () => {
    setError(null);
    setGranted(null);
    try {
      const ok = await localFs.accessGranted();
      setGranted(ok);
      if (!ok) return;
      const rs = await localFs.storageRoots();
      setRoots(rs);
      if (rs.length) await loadDir(rs[0]);
    } catch (e) {
      setError(String(e));
      setGranted(true); // don't trap the user on a probe failure
    }
  }, [loadDir]);

  // Each time a new request opens, (re)start the browser.
  useEffect(() => {
    if (pending) {
      setCwd(null);
      setEntries([]);
      void begin();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pending]);

  // Esc cancels.
  useEffect(() => {
    if (!pending) return;
    const h = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        settle(null);
      }
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [pending, settle]);

  const onRequestAccess = useCallback(async () => {
    try {
      await localFs.requestAccess();
    } catch {
      /* opening settings is best-effort */
    }
  }, []);

  if (!pending) return null;

  const parent = cwd ? parentOf(cwd) : null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-end justify-center bg-black/50 sm:items-center"
      style={{
        paddingTop: "env(safe-area-inset-top)",
        paddingBottom: "env(safe-area-inset-bottom)",
      }}
      onClick={() => settle(null)}
    >
      <div
        role="dialog"
        aria-modal="true"
        className="flex max-h-[88vh] w-full max-w-lg flex-col rounded-t-xl border border-[var(--color-border)] bg-[var(--color-surface-2)] sm:max-h-[80vh] sm:rounded-xl"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="flex items-center gap-2 border-b border-[var(--color-border)] px-4 py-3">
          <span className="flex-1 truncate text-sm font-semibold">
            {pending.title ??
              (pending.mode === "folder"
                ? tr("picker_choose_folder", undefined, "Choose a folder")
                : tr("picker_choose_file", undefined, "Choose a file"))}
          </span>
          <button
            type="button"
            aria-label={tr("cancel", undefined, "Cancel")}
            className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface)]"
            onClick={() => settle(null)}
          >
            <X size={18} />
          </button>
        </header>

        {granted === false ? (
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
              <Button variant="primary" size="sm" onClick={onRequestAccess}>
                {tr("picker_open_settings", undefined, "Open settings")}
              </Button>
              <Button variant="ghost" size="sm" onClick={() => void begin()}>
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
                disabled={!parent}
                aria-label={tr("picker_up", undefined, "Up one level")}
                className="rounded p-1.5 text-[var(--color-muted)] hover:bg-[var(--color-surface)] disabled:opacity-30"
                onClick={() => parent && void loadDir(parent)}
              >
                <ChevronUp size={18} />
              </button>
              <span className="flex-1 truncate text-xs text-[var(--color-muted)]">
                {cwd ?? "…"}
              </span>
              {roots.length > 1 && (
                <select
                  className="max-w-[40%] truncate rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-1 py-0.5 text-xs"
                  value={roots.includes(cwd ?? "") ? (cwd ?? "") : ""}
                  onChange={(e) =>
                    e.target.value && void loadDir(e.target.value)
                  }
                >
                  <option value="" disabled>
                    {tr("picker_roots", undefined, "Storage")}
                  </option>
                  {roots.map((r) => (
                    <option key={r} value={r}>
                      {r}
                    </option>
                  ))}
                </select>
              )}
            </div>

            <div className="min-h-0 flex-1 overflow-y-auto">
              {loading ? (
                <div className="px-4 py-8 text-center text-xs text-[var(--color-muted)]">
                  {tr("picker_loading", undefined, "Loading…")}
                </div>
              ) : error ? (
                <div className="px-4 py-8 text-center text-xs text-[var(--color-bad)]">
                  {error}
                </div>
              ) : entries.length === 0 ? (
                <div className="flex flex-col items-center gap-2 px-4 py-8 text-center text-xs text-[var(--color-muted)]">
                  <HardDrive size={20} />
                  {tr("picker_empty", undefined, "This folder is empty.")}
                </div>
              ) : (
                <ul>
                  {entries.map((e) => {
                    const selectable = e.is_dir || pending.mode === "file";
                    return (
                      <li key={e.path}>
                        <button
                          type="button"
                          disabled={!selectable}
                          className="flex w-full items-center gap-3 px-4 py-3 text-left hover:bg-[var(--color-surface)] disabled:opacity-40"
                          onClick={() => {
                            if (e.is_dir) void loadDir(e.path);
                            else if (pending.mode === "file") settle(e.path);
                          }}
                        >
                          {e.is_dir ? (
                            <Folder
                              size={18}
                              className="shrink-0 text-[var(--color-accent)]"
                            />
                          ) : (
                            <FileIcon
                              size={18}
                              className="shrink-0 text-[var(--color-muted)]"
                            />
                          )}
                          <span className="flex-1 truncate text-sm">
                            {e.name}
                          </span>
                          {!e.is_dir && (
                            <span className="shrink-0 text-xs text-[var(--color-muted)]">
                              {fmtSize(e.size)}
                            </span>
                          )}
                        </button>
                      </li>
                    );
                  })}
                </ul>
              )}
            </div>

            {pending.mode === "folder" && (
              <footer className="flex items-center gap-2 border-t border-[var(--color-border)] px-4 py-3">
                <span className="flex-1 truncate text-xs text-[var(--color-muted)]">
                  {tr("picker_use_this", undefined, "Use the open folder:")}{" "}
                  <span className="font-medium text-[var(--color-text)]">
                    {cwd ? cwd.split("/").pop() || cwd : "…"}
                  </span>
                </span>
                <Button
                  variant="primary"
                  size="sm"
                  disabled={!cwd}
                  onClick={() => cwd && settle(cwd)}
                >
                  <Check size={14} />
                  {tr("picker_use_folder", undefined, "Use this folder")}
                </Button>
              </footer>
            )}
          </>
        )}
      </div>
    </div>
  );
}
