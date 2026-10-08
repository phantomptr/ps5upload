import { useEffect, useMemo, useState } from "react";
import { ArrowUp, Clock, Folder, FolderInput, HardDrive, Usb } from "lucide-react";

import { fsListDir, type Volume } from "../../api/ps5";
import { Button, Modal } from "../../components";
import { formatBytes } from "../../lib/format";
import { planMove, quickDestinations, recentDestinations } from "../../lib/moveTo";
import { useTr } from "../../state/lang";

export interface MoveItem {
  path: string;
  name: string;
  size: number;
}

function parentDir(p: string): string {
  if (p === "/" || p === "") return "/";
  const i = p.lastIndexOf("/");
  return i <= 0 ? "/" : p.slice(0, i);
}

/** Pick where to move the selection, without browsing there first. Says before anything runs
 *  whether it is an instant move on one drive or a copy to another drive, and won't start a
 *  copy that certainly doesn't fit. */
export function MoveToDialog({
  open,
  host,
  addr,
  items,
  volumes,
  startPath,
  onCancel,
  onConfirm,
}: {
  open: boolean;
  host: string;
  addr: string;
  items: MoveItem[];
  volumes: Volume[];
  startPath: string;
  onCancel: () => void;
  onConfirm: (dest: string) => void;
}) {
  const tr = useTr();
  const [dest, setDest] = useState(startPath);
  const [dirs, setDirs] = useState<string[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const recent = useMemo(() => (open ? recentDestinations(host) : []), [open, host]);
  const quick = useMemo(() => quickDestinations(volumes), [volumes]);

  useEffect(() => {
    if (open) setDest(startPath);
  }, [open, startPath]);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setDirs(null);
    setListError(null);
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
          setListError(e instanceof Error ? e.message : String(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [open, addr, dest]);

  const plan = planMove(items, dest, volumes);
  const blocked = plan.fits === "no" || plan.intoItself || plan.alreadyThere || !!listError;
  const what =
    items.length === 1
      ? `"${items[0].name}"`
      : tr("fs_move_n_items", { count: items.length }, `${items.length} items`);

  const summary = plan.intoItself
    ? tr("fs_move_into_itself", undefined, "A folder can't be moved into itself.")
    : plan.alreadyThere
      ? tr("fs_move_already_there", undefined, "Already in this folder.")
      : listError
        ? tr("fs_move_dest_missing", undefined, "This folder can't be opened. Pick another.")
        : !plan.crossDrive
          ? tr("fs_move_same_drive", undefined, "Same drive: moves instantly.")
          : plan.fits === "no"
            ? tr(
                "fs_move_no_room",
                { size: formatBytes(plan.bytes) },
                `Another drive, and ${formatBytes(plan.bytes)} won't fit there.`,
              )
            : tr(
                "fs_move_cross_drive",
                { size: formatBytes(plan.bytes) },
                `Another drive: copies ${formatBytes(plan.bytes)}, checks it, then removes the originals.`,
              );

  const destButton = (path: string, label: React.ReactNode, key: string) => (
    <button
      key={key}
      type="button"
      onClick={() => setDest(path)}
      className={`flex w-full items-center gap-2 rounded-md border px-2 py-1.5 text-left text-xs ${
        dest === path
          ? "border-[var(--color-accent)] bg-[var(--color-accent)]/10"
          : "border-[var(--color-border)] hover:bg-[var(--color-surface-3)]"
      }`}
    >
      {label}
    </button>
  );

  return (
    <Modal
      open={open}
      onClose={onCancel}
      size="lg"
      titleIcon={<FolderInput size={18} />}
      title={tr("fs_move_title", { what }, `Move ${what} to…`)}
      footer={
        <div className="flex w-full flex-wrap items-center justify-end gap-2">
          <Button variant="ghost" onClick={onCancel}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
          <Button variant="primary" disabled={blocked} onClick={() => onConfirm(dest)}>
            {tr("fs_move_here", undefined, "Move here")}
          </Button>
        </div>
      }
    >
      <div className="grid gap-3">
        <div>
          <div className="mb-1 text-[11px] font-semibold uppercase text-[var(--color-muted)]">
            {tr("fs_move_drives", undefined, "Drives")}
          </div>
          <div className="grid gap-1 sm:grid-cols-2">
            {quick.map(({ path, volume }) => {
              const external =
                volume.path.startsWith("/mnt/usb") || volume.path.startsWith("/mnt/ext");
              const Icon = external ? Usb : HardDrive;
              return destButton(
                path,
                <>
                  <Icon size={14} className="shrink-0" />
                  <span className="min-w-0 flex-1 truncate font-mono">{path}</span>
                  <span className="shrink-0 text-[var(--color-muted)]">
                    {formatBytes(volume.free_bytes)} {tr("fs_free", undefined, "free")}
                  </span>
                </>,
                `q:${path}`,
              );
            })}
          </div>
        </div>

        {recent.length > 0 && (
          <div>
            <div className="mb-1 text-[11px] font-semibold uppercase text-[var(--color-muted)]">
              {tr("fs_move_recent", undefined, "Recent")}
            </div>
            <div className="grid gap-1">
              {recent.map((path) =>
                destButton(
                  path,
                  <>
                    <Clock size={14} className="shrink-0" />
                    <span className="min-w-0 flex-1 truncate font-mono">{path}</span>
                  </>,
                  `r:${path}`,
                ),
              )}
            </div>
          </div>
        )}

        <div>
          <div className="mb-1 flex items-center gap-2">
            <button
              type="button"
              onClick={() => setDest(parentDir(dest))}
              disabled={dest === "/"}
              aria-label={tr("fs_up", undefined, "Up")}
              className="rounded-md border border-[var(--color-border)] p-1 hover:bg-[var(--color-surface-3)] disabled:opacity-40"
            >
              <ArrowUp size={14} />
            </button>
            <span className="min-w-0 flex-1 truncate font-mono text-xs" title={dest}>
              {dest}
            </span>
          </div>
          <div className="max-h-56 overflow-y-auto rounded-md border border-[var(--color-border)]">
            {dirs === null ? (
              <div className="p-2 text-xs text-[var(--color-muted)]">
                {tr("loading", undefined, "Loading…")}
              </div>
            ) : dirs.length === 0 ? (
              <div className="p-2 text-xs text-[var(--color-muted)]">
                {tr("fs_move_no_subfolders", undefined, "No folders inside.")}
              </div>
            ) : (
              dirs.map((name) => (
                <button
                  key={name}
                  type="button"
                  onClick={() => setDest(dest === "/" ? `/${name}` : `${dest}/${name}`)}
                  className="flex w-full items-center gap-2 px-2 py-1.5 text-left text-xs hover:bg-[var(--color-surface-3)]"
                >
                  <Folder size={14} className="shrink-0 text-[var(--color-muted)]" />
                  <span className="truncate">{name}</span>
                </button>
              ))
            )}
          </div>
        </div>

        <p
          className={`rounded-md border p-2 text-sm ${
            blocked
              ? "border-[var(--color-bad)] bg-[var(--color-bad-soft)]"
              : plan.fits === "tight"
                ? "border-[var(--color-warn)] bg-[var(--color-warn-soft)]"
                : "border-[var(--color-border)] bg-[var(--color-surface-2)]"
          }`}
        >
          {summary}
          {plan.fits === "tight" && !blocked && (
            <>
              {" "}
              {tr(
                "fs_move_tight",
                undefined,
                "It may not fit: the PS5 holds back more space as it writes.",
              )}
            </>
          )}
        </p>
      </div>
    </Modal>
  );
}
