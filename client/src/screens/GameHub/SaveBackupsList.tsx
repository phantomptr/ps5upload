import { useCallback, useEffect, useState, type ReactNode } from "react";
import { RotateCcw, Users } from "lucide-react";

import { Button } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { saveBackupApi, serverBackupApi, type SaveBackupFile } from "../../api/saveBackup";
import type { ProfileUser, SaveEntry } from "../../api/ps5";
import { restoreBackupToUser, partLabel, SaveCopyError } from "../../api/saveShare";
import CopyToUserDialog, { targetName } from "../Saves/CopyToUserDialog";
import { copyTargets, type CopyTarget } from "../Saves/copyTargets";
import { formatBytes } from "../../lib/format";
import { hostOf } from "../../lib/addr";
import { pushNotification } from "../../state/notifications";
import { useAutoSaveBackupStore } from "../../state/autoSaveBackup";
import { useServerBackupStore } from "../../state/serverSaveBackup";
import { useTr } from "../../state/lang";

/** How many versions a save shows before "show all". */
const SHOWN = 3;

/** `2026-10-24_101500` → `2026-10-24 10:15 UTC` (a snapshot's `_snap` after it is dropped). */
export function stampLabel(stamp: string): string {
  const m = /^(\d{4}-\d{2}-\d{2})_(\d{2})(\d{2})\d{2}(?:_[a-z]+)?$/.exec(stamp);
  return m ? `${m[1]} ${m[2]}:${m[3]} UTC` : stamp;
}

/** A backup in this computer's backup folder, or on the network share (#592). */
export type ListedBackup = SaveBackupFile & { server?: boolean };

/** Local and server backups as one list, newest first (a stamp sorts in time order). */
export function mergeBackups(local: SaveBackupFile[], server: SaveBackupFile[]): ListedBackup[] {
  const all: ListedBackup[] = [...local, ...server.map((b) => ({ ...b, server: true }))];
  return all.sort((a, b) =>
    a.stamp < b.stamp ? 1 : a.stamp > b.stamp ? -1 : a.server ? 1 : -1,
  );
}

/** The automatic backups of one save, newest first, each with Restore: those in the backup
 *  folder and those on the network share. The engine puts the backup back after keeping what is
 *  there now, so a restore can itself be undone. */
export function SaveBackupsList({
  host,
  save,
  reload = 0,
  empty = null,
  users = [],
  saves = [],
  game,
}: {
  host: string;
  save: SaveEntry;
  /** The console's local users and every save on it: with another user, a PS5 save's backups
   *  can be restored into that user's save of the game (#612). */
  users?: readonly ProfileUser[];
  saves?: readonly SaveEntry[];
  /** The game's name, for the dialog. */
  game?: string;
  /** Changed to look again (a backup was just made). */
  reload?: number;
  /** Shown when the save has no backup yet. */
  empty?: ReactNode;
}) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const settings = useAutoSaveBackupStore((s) => s.settings);
  const refreshSettings = useAutoSaveBackupStore((s) => s.refresh);
  const serverSettings = useServerBackupStore((s) => s.settings);
  const refreshServer = useServerBackupStore((s) => s.refresh);
  const [backups, setBackups] = useState<ListedBackup[]>([]);
  const [all, setAll] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  // Why the last restore was refused (the game is running, say), shown under the list too.
  const [failed, setFailed] = useState<string | null>(null);
  // Restore to another user: the backup being put in.
  const [toUser, setToUser] = useState<ListedBackup | null>(null);
  const targets: CopyTarget[] =
    save.kind === "ps5" ? copyTargets(users, saves, save.title_id, save.user_id) : [];

  useEffect(() => {
    if (!settings) void refreshSettings();
  }, [settings, refreshSettings]);
  useEffect(() => {
    if (!serverSettings) void refreshServer();
  }, [serverSettings, refreshServer]);
  const dir = settings?.dir ?? "";
  const target = serverSettings?.target ?? "";
  const load = useCallback(async () => {
    const mine = (b: SaveBackupFile) => b.user_id === save.user_id >>> 0;
    // A server that can't be reached leaves its backups out; the local ones still show.
    const none: SaveBackupFile[] = [];
    const [local, server] = await Promise.all([
      dir.trim() ? saveBackupApi.list(save.title_id).catch(() => none) : none,
      target ? serverBackupApi.list(save.title_id).catch(() => none) : none,
    ]);
    setBackups(mergeBackups(local.filter(mine), server.filter(mine)));
  }, [dir, target, save.title_id, save.user_id]);
  useEffect(() => {
    void load();
  }, [load, reload]);

  if (backups.length === 0) return <>{empty}</>;
  const shown = all ? backups : backups.slice(0, SHOWN);

  const key = (b: ListedBackup) => `${b.server ? "server" : "local"}:${b.file}`;

  async function restore(b: ListedBackup) {
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
    setBusy(key(b));
    setFailed(null);
    try {
      await (b.server ? serverBackupApi : saveBackupApi).restore({
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
      const why = e instanceof Error ? e.message : String(e);
      setFailed(why);
      pushNotification("error", tr("save_backup_restore_failed", undefined, "Couldn't restore the save"), {
        body: why,
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
          <li key={key(b)} className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1 text-xs">
            <span className="tabular-nums">
              {stampLabel(b.stamp)} · {formatBytes(b.size)}
              {b.tag === "beforeps" && (
                <span className="ms-2 rounded-full bg-[var(--color-surface-2)] px-2 py-0.5 text-[11px] text-[var(--color-muted)]">
                  {tr("save_backup_before_ps4", undefined, "Before a PS4 save")}
                </span>
              )}
              {b.server && (
                <span className="ms-2 rounded-full bg-[var(--color-surface-2)] px-2 py-0.5 text-[11px] text-[var(--color-muted)]" data-testid="save-backup-server">
                  {tr("save_backup_on_server", undefined, "On the server")}
                </span>
              )}
              {b.tag === "beforerestore" && (
                <span className="ms-2 rounded-full bg-[var(--color-surface-2)] px-2 py-0.5 text-[11px] text-[var(--color-muted)]">
                  {tr("save_backup_before_restore", undefined, "Before a restore")}
                </span>
              )}
              {b.tag === "snap" && (
                <span className="ms-2 rounded-full bg-[var(--color-surface-2)] px-2 py-0.5 text-[11px] text-[var(--color-muted)]" data-testid="save-backup-snap">
                  {tr("save_backup_snap_label", undefined, "Taken while playing")}
                </span>
              )}
            </span>
            <span className="flex shrink-0 flex-wrap justify-end gap-1">
              <Button
                variant="ghost"
                size="sm"
                leftIcon={<RotateCcw size={12} />}
                loading={busy === key(b)}
                disabled={busy !== null}
                onClick={() => void restore(b)}
              >
                {tr("save_backup_restore", undefined, "Restore")}
              </Button>
              {targets.length > 0 && (
                <Button
                  variant="ghost"
                  size="sm"
                  leftIcon={<Users size={12} />}
                  disabled={busy !== null}
                  onClick={() => setToUser(b)}
                  data-testid="save-backup-restore-to-user"
                >
                  {tr("save_backup_restore_to_user", undefined, "Restore to another user…")}
                </Button>
              )}
            </span>
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
      {failed && (
        <p className="mt-1 text-xs text-[var(--color-bad)]" role="alert" data-testid="save-backup-restore-error">
          {tr("save_backup_restore_failed", undefined, "Couldn't restore the save")}: {failed}
        </p>
      )}
      {toUser && (
        <CopyToUserDialog
          game={game ?? save.title_id}
          source={{ kind: "backup", when: stampLabel(toUser.stamp) }}
          targets={targets}
          onClose={() => setToUser(null)}
          run={async (t) => {
            try {
              await restoreBackupToUser(hostOf(host), {
                title_id: save.title_id,
                file: toUser.file,
                to_user: t.uid,
                server: toUser.server ?? false,
              });
            } catch (e) {
              if (e instanceof SaveCopyError && e.missing.length > 0) {
                const main = tr("saves_part_main", undefined, "main save");
                // The same error, said plainly (the parts the target is missing).
                e.message = tr(
                  "saves_part_missing",
                  { game: game ?? save.title_id, part: e.missing.map((m) => partLabel(m, main)).join(", "), user: targetName(t, tr) },
                  "{game}'s {part} isn't on {user} yet: start the game once as {user} and save, then try again.",
                );
              }
              throw e;
            }
          }}
        />
      )}
      {dialog}
    </div>
  );
}
