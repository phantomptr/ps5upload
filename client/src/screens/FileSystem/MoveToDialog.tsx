import { useEffect, useMemo, useState } from "react";
import { FolderInput, HardDrive, Usb } from "lucide-react";

import type { Volume } from "../../api/ps5";
import { Button, Modal } from "../../components";
import { formatBytes } from "../../lib/format";
import { planMove, quickDestinations } from "../../lib/moveTo";
import { useTr } from "../../state/lang";
import { ConsoleFolderBrowser, DestButton } from "./ConsoleFolderBrowser";

export interface MoveItem {
  path: string;
  name: string;
  size: number;
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
  const [listError, setListError] = useState<string | null>(null);
  const quick = useMemo(() => quickDestinations(volumes), [volumes]);

  useEffect(() => {
    if (open) setDest(startPath);
  }, [open, startPath]);

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
              return (
                <DestButton key={`q:${path}`} path={path} selected={dest === path} onPick={setDest}>
                  <Icon size={14} className="shrink-0" />
                  <span className="min-w-0 flex-1 truncate font-mono">{path}</span>
                  <span className="shrink-0 text-[var(--color-muted)]">
                    {formatBytes(volume.free_bytes)} {tr("fs_free", undefined, "free")}
                  </span>
                </DestButton>
              );
            })}
          </div>
        </div>

        <ConsoleFolderBrowser
          open={open}
          host={host}
          addr={addr}
          dest={dest}
          onDest={setDest}
          onListError={setListError}
        />

        <p
          className={`rounded-[var(--radius-field)] border px-3 py-2 text-sm ${
            blocked
              ? "border-[color-mix(in_srgb,var(--color-bad)_40%,transparent)] bg-[var(--color-bad-soft)]"
              : plan.fits === "tight"
                ? "border-[color-mix(in_srgb,var(--color-warn)_40%,transparent)] bg-[var(--color-warn-soft)]"
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
