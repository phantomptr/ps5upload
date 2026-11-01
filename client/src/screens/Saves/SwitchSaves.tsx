import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { FolderOpen, Gamepad2, History } from "lucide-react";

import { Button } from "../../components";
import { saveBackupApi, type SwitchSave } from "../../api/saveBackup";
import type { SaveEntry } from "../../api/ps5";
import { hostOf } from "../../lib/addr";
import { formatBytes } from "../../lib/format";
import { formatDate } from "../../lib/formatDate";
import { captureHostProbe } from "../../lib/staleHostGuard";
import { openInFileSystem } from "../../state/fsNavigation";
import { pushNotification } from "../../state/notifications";
import { useAutoSaveBackupStore } from "../../state/autoSaveBackup";
import { useTr } from "../../state/lang";
import { SaveBackupsList } from "../GameHub/SaveBackupsList";

/** The profile folder's number as the engine gives it (its first 7 hex digits; 0 for the
 *  console's own save), as a label's id. */
export function profileHex(userId: number): string {
  return (userId >>> 0).toString(16).toUpperCase().padStart(7, "0");
}

/** The Switch games' saves of ProsperoEden (PPSA99008), with their backups: shown only when the
 *  console has one. No game names are known, so each shows its 16-hex-digit title ID. */
export default function SwitchSaves({ host, refreshKey }: { host: string; refreshKey: number }) {
  const tr = useTr();
  const [saves, setSaves] = useState<SwitchSave[]>([]);

  useEffect(() => {
    setSaves([]);
    if (!host.trim()) return;
    const probe = captureHostProbe();
    // An older engine, or a console that does not answer: no Switch group.
    saveBackupApi
      .switchSaves(hostOf(host))
      .then((r) => {
        if (!probe.isStale()) setSaves(r.saves ?? []);
      })
      .catch(() => {});
  }, [host, refreshKey]);

  if (saves.length === 0) return null;
  const running = saves.some((s) => s.running === true);
  const games = [...new Set(saves.map((s) => s.title_id))].sort();

  return (
    <section className="surface-panel mt-3 p-5" data-testid="switch-saves">
      <header className="mb-1 flex items-center gap-2">
        <Gamepad2 size={16} aria-hidden />
        <h3 className="text-sm font-semibold">
          {tr("saves_switch_title", undefined, "Switch games (ProsperoEden)")}
        </h3>
      </header>
      <p
        className={
          "mb-3 text-xs " +
          (running ? "text-[var(--color-warn)]" : "text-[var(--color-muted)]")
        }
        data-testid="switch-saves-note"
      >
        {running
          ? tr(
              "saves_switch_running",
              undefined,
              "ProsperoEden is running. Close it before restoring.",
            )
          : tr("saves_switch_close_note", undefined, "Close ProsperoEden before restoring.")}
      </p>
      <div className="space-y-3">
        {games.map((game) => (
          <div key={game} data-title-id={game}>
            <div className="mb-1 flex items-center gap-2">
              <code className="text-sm font-semibold">{game}</code>
              <span className="rounded bg-[var(--color-surface-3)] px-1.5 py-0.5 text-xs">
                {tr("saves_switch_badge", undefined, "Switch")}
              </span>
            </div>
            <ul className="space-y-2">
              {saves
                .filter((s) => s.title_id === game)
                .map((s) => (
                  <SwitchSaveRow key={s.path} host={host} save={s} />
                ))}
            </ul>
          </div>
        ))}
      </div>
    </section>
  );
}

function SwitchSaveRow({ host, save }: { host: string; save: SwitchSave }) {
  const tr = useTr();
  const navigate = useNavigate();
  const dir = useAutoSaveBackupStore((s) => s.settings?.dir ?? "");
  const [busy, setBusy] = useState(false);
  const [reload, setReload] = useState(0);
  const entry: SaveEntry = {
    title_id: save.title_id,
    user_id: save.user_id,
    path: save.path,
    size: save.size,
    mtime: save.mtime,
    kind: save.kind,
  };
  const profile =
    save.user_id === 0
      ? tr("saves_switch_console", undefined, "Console")
      : tr("saves_switch_profile", { id: profileHex(save.user_id) }, "Profile {id}");

  const backUp = useCallback(async () => {
    setBusy(true);
    try {
      await saveBackupApi.snapshot({ addr: hostOf(host), title_id: save.title_id });
      pushNotification(
        "success",
        tr("saves_switch_backed_up", { title: save.title_id }, "Backed up {title}"),
      );
      setReload((n) => n + 1);
    } catch (e) {
      pushNotification(
        "error",
        tr("saves_switch_backup_failed", { title: save.title_id }, "Couldn't back up {title}"),
        { body: e instanceof Error ? e.message : String(e) },
      );
    } finally {
      setBusy(false);
    }
  }, [host, save.title_id, tr]);

  return (
    <li
      className="rounded-[var(--radius-field)] bg-[var(--color-surface)] px-2 py-1.5 text-xs"
      data-testid="switch-save"
    >
      <div className="flex flex-wrap items-center gap-3">
        <div className="min-w-0 flex-1 basis-[12rem]">
          <div className="font-medium">{profile}</div>
          <div className="text-[var(--color-muted)]">
            {save.mtime > 0 && (
              <>
                {tr(
                  "saves_switch_changed",
                  { when: formatDate(save.mtime * 1000, "datetime") },
                  "Last changed {when}",
                )}{" "}
                ·{" "}
              </>
            )}
            {formatBytes(save.size)}
          </div>
        </div>
        <Button
          variant="secondary"
          size="sm"
          leftIcon={<History size={11} />}
          loading={busy}
          disabled={busy || !dir.trim()}
          onClick={() => void backUp()}
          title={
            dir.trim()
              ? tr(
                  "saves_switch_backup_hint",
                  undefined,
                  "Back up this game's save now, into the folder for automatic backups.",
                )
              : tr(
                  "saves_switch_need_dir",
                  undefined,
                  "Choose a folder for automatic backups first.",
                )
          }
          data-testid="switch-save-backup"
        >
          {tr("saves_switch_backup_now", undefined, "Back up now")}
        </Button>
        <Button
          variant="ghost"
          size="sm"
          leftIcon={<FolderOpen size={11} />}
          onClick={() => openInFileSystem(navigate, save.path)}
          title={tr(
            "saves_open_folder_hint",
            undefined,
            "Open this save's folder in the File System browser",
          )}
        >
          {tr("saves_open_folder", undefined, "Open folder")}
        </Button>
      </div>
      <SaveBackupsList
        host={host}
        save={entry}
        reload={reload}
        empty={
          <div className="mt-1 text-[var(--color-muted)]">
            {tr("saves_switch_no_backups", undefined, "No backups yet.")}
          </div>
        }
      />
    </li>
  );
}
