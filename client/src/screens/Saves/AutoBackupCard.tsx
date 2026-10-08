import { FolderOpen, History } from "lucide-react";

import { Button } from "../../components";
import { pickPath } from "../../lib/pickPath";
import { isTauriEnv } from "../../lib/tauriEnv";
import { clampKeep, useAutoSaveBackupStore } from "../../state/autoSaveBackup";
import { useTr } from "../../state/lang";

/** The automatic save backup's switch, folder and how many versions it keeps. Desktop only:
 *  the backups are written by the desktop app to a folder on this computer. */
export default function AutoBackupCard() {
  const tr = useTr();
  const { enabled, dir, keep, last, running, set } = useAutoSaveBackupStore();
  if (!isTauriEnv()) return null;

  async function chooseFolder() {
    const picked = await pickPath({
      mode: "folder",
      title: tr("saves_auto_pick", undefined, "Folder for automatic save backups"),
    });
    if (picked) set({ dir: picked });
  }

  // Same centred column as the save list below it, so the two line up on a wide window.
  return (
    <section
      className="mx-auto mb-4 max-w-4xl rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4"
      data-testid="auto-save-backup"
    >
      <header className="mb-1 flex items-center gap-2">
        <History size={14} />
        <h3 className="flex-1 text-sm font-semibold">
          {tr("saves_auto_title", undefined, "Automatic backups")}
        </h3>
      </header>
      <p className="mb-3 text-xs text-[var(--color-muted)]">
        {tr(
          "saves_auto_note",
          undefined,
          "While this app is open and connected, saves that changed are copied to a folder on this computer every ten minutes. A save still being written is left for the next round.",
        )}
      </p>
      <div className="grid gap-2 text-sm sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
        <div className="min-w-0">
          <div className="text-xs text-[var(--color-muted)]">
            {tr("saves_auto_folder_label", undefined, "Backup folder")}
          </div>
          <div className="break-all font-mono text-xs">
            {dir.trim()
              ? dir
              : tr("saves_auto_no_folder", undefined, "Choose a folder to turn this on.")}
          </div>
        </div>
        <Button variant="secondary" size="sm" leftIcon={<FolderOpen size={12} />} onClick={chooseFolder}>
          {tr("saves_auto_folder", undefined, "Choose folder")}
        </Button>
        <label htmlFor="saves-auto-keep" className="text-xs text-[var(--color-muted)]">
          {tr("saves_auto_keep", undefined, "Versions to keep")}
        </label>
        <input
          id="saves-auto-keep"
          type="number"
          min={1}
          max={50}
          value={keep}
          onChange={(e) => set({ keep: clampKeep(Number(e.target.value)) })}
          className="w-20 justify-self-start rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-sm text-[var(--color-text)] sm:justify-self-end"
        />
      </div>
      <label className="mt-3 flex items-center gap-2 border-t border-[var(--color-border)] pt-3 text-sm">
        <input
          type="checkbox"
          checked={enabled}
          disabled={!dir.trim()}
          onChange={(e) => set({ enabled: e.target.checked })}
        />
        {tr("saves_auto_enable", undefined, "Back up changed saves automatically")}
      </label>
      {(running || last) && (
        <p className="mt-2 text-xs text-[var(--color-muted)]">
          {running
            ? tr("saves_auto_running", undefined, "Backing up now…")
            : tr(
                "saves_auto_last",
                { ok: last?.backedUp ?? 0, failed: last?.failed ?? 0 },
                `Last round: ${last?.backedUp ?? 0} backed up, ${last?.failed ?? 0} failed.`,
              )}
        </p>
      )}
    </section>
  );
}
