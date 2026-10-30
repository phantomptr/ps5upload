import { useCallback, useEffect, useState } from "react";
import { RotateCcw } from "lucide-react";

import { Button } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { saveBackupApi, type SaveBackupFile } from "../../api/saveBackup";
import type { SaveEntry } from "../../api/ps5";
import { formatBytes } from "../../lib/format";
import { hostOf } from "../../lib/addr";
import { pushNotification } from "../../state/notifications";
import { useAutoSaveBackupStore } from "../../state/autoSaveBackup";
import { useTr } from "../../state/lang";

/** How many versions a save shows before "show all". */
const SHOWN = 3;

/** `2026-10-24_101500` → `2026-10-24 10:15 UTC` (a snapshot's `_snap` after it is dropped). */
export function stampLabel(stamp: string): string {
  const m = /^(\d{4}-\d{2}-\d{2})_(\d{2})(\d{2})\d{2}(?:_[a-z]+)?$/.exec(stamp);
  return m ? `${m[1]} ${m[2]}:${m[3]} UTC` : stamp;
}

/** The automatic backups of one save, newest first, each with Restore. The engine puts the
 *  backup back after keeping what is there now, so a restore can itself be undone. */
export function SaveBackupsList({ host, save }: { host: string; save: SaveEntry }) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const settings = useAutoSaveBackupStore((s) => s.settings);
  const refreshSettings = useAutoSaveBackupStore((s) => s.refresh);
  const [backups, setBackups] = useState<SaveBackupFile[]>([]);
  const [all, setAll] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);

  useEffect(() => {
    if (!settings) void refreshSettings();
  }, [settings, refreshSettings]);
  const dir = settings?.dir ?? "";
  const load = useCallback(async () => {
    if (!dir.trim()) return setBackups([]);
    try {
      const list = await saveBackupApi.list(save.title_id);
      setBackups(list.filter((b) => b.user_id === save.user_id >>> 0));
    } catch {
      setBackups([]);
    }
  }, [dir, save.title_id, save.user_id]);
  useEffect(() => {
    void load();
  }, [load]);

  if (backups.length === 0) return null;
  const shown = all ? backups : backups.slice(0, SHOWN);

  async function restore(b: SaveBackupFile) {
    const ok = await confirm({
      title: tr("save_backup_restore_title", { when: stampLabel(b.stamp) }, "Restore the backup from {when}?"),
      message:
        tr(
          "save_backup_restore_body",
          undefined,
          "Close the game first. The save on the PS5 is backed up, then replaced with this backup.",
        ) +
        (b.tag === "snap"
          ? " " +
            tr(
              "save_backup_snap_note",
              undefined,
              "This one was taken while the game was running: it was copied while the save sat still, but if the game misbehaves, restore an automatic backup instead.",
            )
          : ""),
      confirmLabel: tr("save_backup_restore", undefined, "Restore"),
      destructive: true,
    });
    if (!ok) return;
    setBusy(b.file);
    try {
      await saveBackupApi.restore({
        addr: hostOf(host),
        title_id: save.title_id,
        file: b.file,
        save_path: save.path,
        user_id: save.user_id >>> 0,
      });
      pushNotification(
        "success",
        tr("save_backup_restored", { title: save.title_id, when: stampLabel(b.stamp) }, "Restored {title} from {when}"),
      );
      await load();
    } catch (e) {
      pushNotification("error", tr("save_backup_restore_failed", undefined, "Couldn't restore the save"), {
        body: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="mt-2 rounded-[var(--radius-card)] border border-[var(--color-border)] p-2">
      <div className="mb-1 text-xs font-medium text-[var(--color-muted)]">
        {tr("save_backup_versions", { n: backups.length }, "Automatic backups ({n})")}
      </div>
      <ul className="space-y-1">
        {shown.map((b) => (
          <li key={b.file} className="flex items-center justify-between gap-3 text-xs">
            <span className="tabular-nums">
              {stampLabel(b.stamp)} · {formatBytes(b.size)}
              {b.tag === "beforeps" && (
                <span className="ms-2 rounded-full bg-[var(--color-surface-2)] px-2 py-0.5 text-[11px] text-[var(--color-muted)]">
                  {tr("save_backup_before_ps4", undefined, "Before a PS4 save")}
                </span>
              )}
              {b.tag === "snap" && (
                <span className="ms-2 rounded-full bg-[var(--color-surface-2)] px-2 py-0.5 text-[11px] text-[var(--color-muted)]" data-testid="save-backup-snap">
                  {tr("save_backup_snap_label", undefined, "Taken while playing")}
                </span>
              )}
            </span>
            <Button
              variant="ghost"
              size="sm"
              leftIcon={<RotateCcw size={12} />}
              loading={busy === b.file}
              disabled={busy !== null}
              onClick={() => void restore(b)}
            >
              {tr("save_backup_restore", undefined, "Restore")}
            </Button>
          </li>
        ))}
      </ul>
      {backups.length > SHOWN && (
        <button
          type="button"
          onClick={() => setAll((v) => !v)}
          className="mt-1 text-xs text-[var(--color-accent)]"
        >
          {all
            ? tr("save_backup_show_fewer", undefined, "Show fewer")
            : tr("save_backup_show_all", { n: backups.length }, "Show all {n}")}
        </button>
      )}
      {dialog}
    </div>
  );
}
