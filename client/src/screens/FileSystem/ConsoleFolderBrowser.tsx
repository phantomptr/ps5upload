import { useEffect, useMemo, useState } from "react";
import { ArrowUp, Clock, Folder } from "lucide-react";

import { fsListDir } from "../../api/ps5";
import { recentDestinations } from "../../lib/moveTo";
import { useTr } from "../../state/lang";
import { consoleParentDir as parentDir } from "../../lib/consolePath";
import { humanizeCaught } from "../../lib/humanizeError";

/** One choosable folder in a destination list (a drive, a recent folder). */
export function DestButton({
  path,
  selected,
  onPick,
  children,
}: {
  path: string;
  selected: boolean;
  onPick: (path: string) => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={() => onPick(path)}
      className={`border rounded-[var(--radius-card)] transition-[background-color,border-color,box-shadow] flex w-full items-center gap-2 px-3.5 py-2.5 text-start text-xs ${
        selected
          ? "border-[color-mix(in_srgb,var(--color-accent)_45%,transparent)] bg-[var(--color-surface-raised)] shadow-[var(--edge-highlight),var(--shadow-1)] ring-1 ring-[color-mix(in_srgb,var(--color-accent)_30%,transparent)]"
          : "border-[var(--color-border)] bg-[var(--color-surface)] hover:bg-[var(--color-surface-raised)]"
      }`}
    >
      {children}
    </button>
  );
}

/**
 * Browse the console's folders to pick one: recent destinations, Up, and the folders inside the
 * one picked. Shared by Move to… and Extract's "Extract to". `onListError` says whether the
 * folder shown could be opened (null when it could).
 */
export function ConsoleFolderBrowser({
  open,
  host,
  addr,
  dest,
  onDest,
  onListError,
}: {
  open: boolean;
  host: string;
  addr: string;
  dest: string;
  onDest: (path: string) => void;
  onListError?: (error: string | null) => void;
}) {
  const tr = useTr();
  const [dirs, setDirs] = useState<string[] | null>(null);
  const recent = useMemo(() => (open ? recentDestinations(host) : []), [open, host]);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setDirs(null);
    onListError?.(null);
    fsListDir(addr, dest)
      .then((entries) => {
        if (cancelled) return;
        setDirs(
          entries
            .filter((e) => e.kind === "dir")
            .map((e) => e.name)
            .sort((a, b) => a.localeCompare(b)),
        );
      })
      .catch((e) => {
        if (!cancelled) {
          setDirs([]);
          onListError?.(humanizeCaught(e));
        }
      });
    return () => {
      cancelled = true;
    };
    // onListError is a setter from the parent: it does not decide when to list.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, addr, dest]);

  return (
    <>
      {recent.length > 0 && (
        <div>
          <div className="mb-1 text-[11px] font-semibold uppercase text-[var(--color-muted)]">
            {tr("fs_move_recent", undefined, "Recent")}
          </div>
          <div className="grid gap-1">
            {recent.map((path) => (
              <DestButton key={`r:${path}`} path={path} selected={dest === path} onPick={onDest}>
                <Clock size={14} className="shrink-0" />
                <span className="min-w-0 flex-1 truncate font-mono">{path}</span>
              </DestButton>
            ))}
          </div>
        </div>
      )}

      <div>
        <div className="mb-1 flex items-center gap-2">
          <button
            type="button"
            onClick={() => onDest(parentDir(dest))}
            disabled={dest === "/"}
            aria-label={tr("fs_up", undefined, "Up")}
            className="rounded-full border shadow-[var(--edge-highlight),var(--shadow-1)] hover:bg-[var(--color-float)] border-[var(--glass-edge)] bg-[var(--color-surface-raised)] p-1 disabled:opacity-40"
          >
            <ArrowUp size={14} />
          </button>
          <span className="min-w-0 flex-1 truncate font-mono text-xs" title={dest}>
            {dest}
          </span>
        </div>
        <div className="max-h-56 overflow-y-auto rounded-[var(--radius-card)] border border-[var(--glass-edge)]">
          {dirs === null ? (
            <div className="p-2 text-xs text-[var(--color-muted)]">{tr("loading", undefined, "Loading…")}</div>
          ) : dirs.length === 0 ? (
            <div className="p-2 text-xs text-[var(--color-muted)]">
              {tr("fs_move_no_subfolders", undefined, "No folders inside.")}
            </div>
          ) : (
            dirs.map((name) => (
              <button
                key={name}
                type="button"
                onClick={() => onDest(dest === "/" ? `/${name}` : `${dest}/${name}`)}
                className="flex w-full items-center gap-2 px-2 py-1.5 text-start text-xs hover:bg-[var(--color-surface-3)]"
              >
                <Folder size={14} className="shrink-0 text-[var(--color-muted)]" />
                <span className="truncate">{name}</span>
              </button>
            ))
          )}
        </div>
      </div>
    </>
  );
}
