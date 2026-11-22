import { useState } from "react";
import { FolderPlus, History, X } from "lucide-react";

import { Button } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import {
  saveFoldersApi,
  type FolderBackup,
  type FolderBackupFrom,
} from "../../api/saveBackup";
import { consolePathOf } from "../../api/meshDrive";
import { hostOf } from "../../lib/addr";
import { formatBytes } from "../../lib/format";
import { formatDate } from "../../lib/formatDate";
import { pickPath } from "../../lib/pickPath";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";

/** Whether the engine can back up console folders (#899): an older one leaves `folders` out. */
export function foldersSupported(
  settings: { folders?: string[] } | null | undefined,
): boolean {
  return Array.isArray(settings?.folders);
}

/** A folder list with `path` added once (as the engine will keep it: no trailing slash). */
export function withFolder(list: readonly string[], path: string): string[] {
  const p = path.length > 1 ? path.replace(/\/+$/, "") : path;
  return list.some((x) => x.toLowerCase() === p.toLowerCase())
    ? [...list]
    : [...list, p];
}

/** "2026-11-20_031200" → a Date (UTC, as the engine names backups); null when it isn't one. */
export function stampDate(stamp: string): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})_(\d{2})(\d{2})(\d{2})/.exec(stamp);
  if (!m) return null;
  return new Date(Date.UTC(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], +m[6]));
}

/** "Also back up these folders on the PS5" (#899): console folders (a port's or an emulator's
 *  saves) zipped with each backup run, kept like the saves, and put back from here. `from` is
 *  where this card keeps its backups; copies on the PS5 drive are listed too when `driveCopies`. */
export function FolderBackupChoice({
  folders,
  onChange,
  from,
  driveCopies,
  testId,
}: {
  folders: string[];
  onChange: (folders: string[]) => void;
  from: Exclude<FolderBackupFrom, "drive">;
  driveCopies: boolean;
  testId: string;
}) {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const addr = host?.trim() ? hostOf(host.trim()) : "";
  const { confirm, dialog } = useConfirm();
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [lists, setLists] = useState<Array<{
    from: FolderBackupFrom;
    path: string;
    backups: FolderBackup[];
  }> | null>(null);
  const [busy, setBusy] = useState(false);

  async function add() {
    if (!addr) return;
    setError(null);
    const picked = await pickPath({
      mode: "folder",
      source: { console: addr },
      title: tr(
        "saves_folders_pick",
        undefined,
        "Folder on the PS5 to back up",
      ),
    });
    if (!picked) return;
    const path = consolePathOf(picked);
    try {
      const ok = await saveFoldersApi.check(path);
      if (!ok.ok) {
        setError(
          ok.error ??
            tr(
              "saves_folders_refused",
              undefined,
              "This folder can't be backed up.",
            ),
        );
        return;
      }
      onChange(withFolder(folders, ok.path ?? path));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  async function load() {
    setBusy(true);
    setError(null);
    try {
      const out: Array<{
        from: FolderBackupFrom;
        path: string;
        backups: FolderBackup[];
      }> = [];
      // Asked for this PS5: without it only backups from before they were kept per PS5 come back.
      for (const f of (await saveFoldersApi.list(from, addr)) ?? [])
        out.push({ from, ...f });
      if (driveCopies && addr) {
        for (const f of (await saveFoldersApi
          .list("drive", addr)
          .catch(() => [])) ?? []) {
          out.push({ from: "drive", ...f });
        }
      }
      setLists(out);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function restore(src: FolderBackupFrom, path: string, b: FolderBackup) {
    if (!addr) return;
    const ok = await confirm({
      title: tr(
        "saves_folders_restore_title",
        undefined,
        "Put this folder backup back?",
      ),
      message: tr(
        "saves_folders_restore_body",
        { path },
        "{path} on the PS5 is replaced with this backup. Close the app that uses it first. What is in the folder now is kept first, as a backup beside this one.",
      ),
      confirmLabel: tr("saves_folders_restore", undefined, "Restore"),
    });
    if (!ok) return;
    setBusy(true);
    setError(null);
    setNote(null);
    try {
      await saveFoldersApi.restore({ from: src, addr, path, file: b.file });
      setNote(tr("saves_folders_restored", { path }, "{path} was put back."));
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  const placeName = (f: FolderBackupFrom) =>
    f === "share"
      ? tr("backup_place_share", undefined, "Network share")
      : f === "drive"
        ? tr("backup_place_drive", undefined, "PS5 drive")
        : tr("backup_place_local", undefined, "This computer");

  return (
    <div
      className="mt-3 border-t border-[var(--color-border)] pt-3 text-sm"
      data-testid={testId}
    >
      <div className="flex flex-wrap items-center gap-2">
        <span className="flex-1 basis-[12rem]">
          {tr(
            "saves_folders_title",
            undefined,
            "Also back up these folders on the PS5",
          )}
        </span>
        <Button
          variant="secondary"
          size="sm"
          leftIcon={<FolderPlus size={12} />}
          onClick={() => void add()}
          disabled={!addr}
          data-testid={`${testId}-add`}
        >
          {tr("saves_folders_add", undefined, "Add folder")}
        </Button>
      </div>
      <p className="mt-1 text-xs text-[var(--color-muted)]">
        {tr(
          "saves_folders_note",
          undefined,
          "For saves an app keeps in a folder of its own (ports and emulators). Each folder is zipped with every backup, keeps the same number of versions, and goes in Folders. Restore puts it back where it came from.",
        )}
      </p>
      {!addr && (
        <p className="mt-1 text-xs text-[var(--color-muted)]">
          {tr(
            "saves_folders_no_console",
            undefined,
            "Connect to a PS5 to choose its folders.",
          )}
        </p>
      )}
      {folders.length > 0 && (
        <ul className="mt-2 space-y-1" data-testid={`${testId}-list`}>
          {folders.map((f) => (
            <li key={f} className="flex items-center gap-2">
              <span className="min-w-0 flex-1 break-all font-mono text-xs">
                {f}
              </span>
              <Button
                variant="ghost"
                size="sm"
                leftIcon={<X size={12} />}
                aria-label={tr(
                  "saves_folders_remove",
                  { path: f },
                  "Stop backing up {path}",
                )}
                onClick={() => onChange(folders.filter((x) => x !== f))}
              />
            </li>
          ))}
        </ul>
      )}
      {folders.length > 0 && (
        <details
          className="mt-2 text-xs"
          onToggle={(e) => {
            if ((e.target as HTMLDetailsElement).open && lists === null)
              void load();
          }}
        >
          <summary className="cursor-pointer text-[var(--color-muted)]">
            <History size={12} className="me-1 inline" />
            {tr("saves_folders_backups", undefined, "Folder backups")}
          </summary>
          {busy && lists === null && (
            <p className="mt-1 text-[var(--color-muted)]">
              {tr("loading", undefined, "Loading…")}
            </p>
          )}
          {lists?.map((l) => (
            <div key={`${l.from}|${l.path}`} className="mt-2">
              <div className="break-all font-mono">
                {l.path}{" "}
                <span className="text-[var(--color-muted)]">
                  · {placeName(l.from)}
                </span>
              </div>
              {l.backups.length === 0 ? (
                <p className="text-[var(--color-muted)]">
                  {tr(
                    "saves_folders_none",
                    undefined,
                    "No backup of this folder yet.",
                  )}
                </p>
              ) : (
                <ul className="mt-1 space-y-0.5">
                  {l.backups.map((b) => {
                    const when = stampDate(b.stamp);
                    return (
                      <li
                        key={b.file}
                        className="flex flex-wrap items-center gap-2"
                      >
                        <span className="flex-1">
                          {when ? formatDate(when) : b.stamp} ·{" "}
                          {formatBytes(b.size)}
                          {b.tag === "beforerestore"
                            ? ` · ${tr("saves_folders_before_restore", undefined, "kept before a restore")}`
                            : ""}
                          {b.legacy
                            ? ` · ${tr("saves_folders_legacy", undefined, "from before backups were kept per PS5")}`
                            : ""}
                        </span>
                        <Button
                          variant="ghost"
                          size="sm"
                          onClick={() => void restore(l.from, l.path, b)}
                          disabled={busy || !addr}
                        >
                          {tr("saves_folders_restore", undefined, "Restore")}
                        </Button>
                      </li>
                    );
                  })}
                </ul>
              )}
            </div>
          ))}
        </details>
      )}
      {note && <p className="mt-1 text-xs text-[var(--color-good)]">{note}</p>}
      {error && (
        <p className="mt-1 break-words text-xs text-[var(--color-bad)]">
          {error}
        </p>
      )}
      {dialog}
    </div>
  );
}
