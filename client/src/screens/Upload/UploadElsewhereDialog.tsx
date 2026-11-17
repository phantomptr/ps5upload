import { useEffect, useMemo, useState } from "react";
import { FolderInput, HardDrive, Usb } from "lucide-react";

import { listVolumes, type Volume } from "../../api/ps5";
import { Button, Modal } from "../../components";
import { hostOf, transferAddr } from "../../lib/addr";
import { consoleParentDir } from "../../lib/consolePath";
import { destInFolder } from "../../lib/destinationConflict";
import { formatBytes } from "../../lib/format";
import { quickDestinations } from "../../lib/moveTo";
import { useTr } from "../../state/lang";
import { ConsoleFolderBrowser, DestButton } from "../FileSystem/ConsoleFolderBrowser";

/**
 * Pick another folder on the PS5 for an upload whose destination is taken: drives, recent
 * folders, then browse. Says where the upload will land (the folder plus its name). Used by a
 * failed queue row and by the Upload screen's "already on the PS5" question.
 */
export function UploadElsewhereDialog({
  open,
  addr,
  dest,
  flat = false,
  onCancel,
  onPick,
}: {
  open: boolean;
  /** The console's address (any form `transferAddr` accepts). */
  addr: string;
  /** Where the upload goes now: its folder is where browsing starts. */
  dest: string;
  /** An archive extracted straight into `dest`: the picked folder is where it goes, and
   *  browsing starts in `dest` itself. */
  flat?: boolean;
  onCancel: () => void;
  onPick: (folder: string) => void;
}) {
  const tr = useTr();
  const start = (flat ? dest.replace(/\/+$/, "") : consoleParentDir(dest)) || "/data";
  const [folder, setFolder] = useState(start);
  const [listError, setListError] = useState<string | null>(null);
  const [volumes, setVolumes] = useState<Volume[]>([]);

  useEffect(() => {
    if (open) setFolder(start);
  }, [open, start]);

  useEffect(() => {
    if (!open || !addr) return;
    let live = true;
    listVolumes(transferAddr(addr)).then(
      (vols) => live && setVolumes(vols.filter((v) => v.writable && !v.is_placeholder)),
      () => live && setVolumes([]),
    );
    return () => {
      live = false;
    };
  }, [open, addr]);

  const quick = useMemo(() => quickDestinations(volumes), [volumes]);
  const lands = destInFolder(dest, folder, flat);
  const same = lands === (dest.replace(/\/+$/, "") || "/");
  const blocked = same || !!listError;

  return (
    <Modal
      open={open}
      onClose={onCancel}
      size="lg"
      titleIcon={<FolderInput size={18} />}
      title={tr("upload_elsewhere_title", undefined, "Upload somewhere else")}
      footer={
        <div className="flex w-full flex-wrap items-center justify-end gap-2">
          <Button variant="ghost" onClick={onCancel}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
          <Button
            variant="primary"
            disabled={blocked}
            onClick={() => onPick(folder)}
            data-testid="upload-elsewhere-confirm"
          >
            {tr("upload_elsewhere_here", undefined, "Upload here")}
          </Button>
        </div>
      }
    >
      <div className="grid gap-3" data-testid="upload-elsewhere-dialog">
        {quick.length > 0 && (
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
                  <DestButton key={`q:${path}`} path={path} selected={folder === path} onPick={setFolder}>
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
        )}

        <ConsoleFolderBrowser
          open={open}
          host={hostOf(addr)}
          addr={transferAddr(addr)}
          dest={folder}
          onDest={setFolder}
          onListError={setListError}
        />

        <p
          className={`rounded-[var(--radius-field)] border px-3 py-2 text-sm break-words ${
            blocked
              ? "border-[color-mix(in_srgb,var(--color-bad)_40%,transparent)] bg-[var(--color-bad-soft)]"
              : "border-[var(--color-border)] bg-[var(--color-surface-2)]"
          }`}
        >
          {same
            ? tr("upload_elsewhere_same", undefined, "That's where it goes now. Pick another folder.")
            : listError
              ? tr("fs_move_dest_missing", undefined, "This folder can't be opened. Pick another.")
              : tr("upload_elsewhere_lands", { path: lands }, "It goes to {path}")}
        </p>
      </div>
    </Modal>
  );
}
