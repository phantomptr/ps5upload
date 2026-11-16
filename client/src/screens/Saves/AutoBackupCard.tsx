import { useEffect, useState } from "react";
import { FolderOpen, History, Play } from "lucide-react";

import { Button } from "../../components";
import { pickPath } from "../../lib/pickPath";
import { isTauriEnv } from "../../lib/tauriEnv";
import { formatDate } from "../../lib/formatDate";
import {
  DEFAULT_INTERVAL_MINUTES,
  clampKeep,
  intervalChoices,
  intervalSupported,
  useAutoSaveBackupStore,
} from "../../state/autoSaveBackup";
import { useTr } from "../../state/lang";

type Tr = ReturnType<typeof useTr>;

/** "5 min", "1 hour", "6 hours", "1 day" … */
export function intervalLabel(minutes: number, tr: Tr): string {
  if (minutes === 1440) return tr("saves_auto_every_day", undefined, "1 day");
  if (minutes === 60) return tr("saves_auto_every_hour", undefined, "1 hour");
  if (minutes > 60 && minutes % 60 === 0)
    return tr("saves_auto_every_hours", { n: minutes / 60 }, "{n} hours");
  return tr("saves_auto_every_minutes", { n: minutes }, "{n} min");
}

/** How often the engine backs up, from 5 minutes to a day. */
export function IntervalChoice({
  minutes,
  onChange,
}: {
  minutes: number | undefined;
  onChange: (minutes: number) => void;
}) {
  const tr = useTr();
  return (
    <>
      <label htmlFor="saves-auto-every" className="text-xs text-[var(--color-muted)]">
        {tr("saves_auto_every", undefined, "Back up every")}
      </label>
      <select
        id="saves-auto-every"
        data-testid="auto-save-backup-every"
        value={minutes ?? DEFAULT_INTERVAL_MINUTES}
        onChange={(e) => onChange(Number(e.target.value))}
        className="select w-auto! justify-self-start text-sm sm:justify-self-end"
      >
        {intervalChoices(minutes).map((m) => (
          <option key={m} value={m}>
            {intervalLabel(m, tr)}
          </option>
        ))}
      </select>
    </>
  );
}

/** The automatic save backup's switch, folder, how often and how many versions it keeps. The
 *  engine runs the backups (with no window open in Docker); this card changes its settings. */
export default function AutoBackupCard() {
  const tr = useTr();
  const { settings, status, error, refresh, set, runNow } = useAutoSaveBackupStore();
  const [dirDraft, setDirDraft] = useState("");
  const desktop = isTauriEnv();

  useEffect(() => {
    void refresh();
  }, [refresh]);
  useEffect(() => {
    setDirDraft(settings?.dir ?? "");
  }, [settings?.dir]);
  // While a pass runs, follow it.
  const running = status?.running ?? false;
  useEffect(() => {
    if (!running) return;
    const id = window.setInterval(() => void refresh(), 3000);
    return () => window.clearInterval(id);
  }, [running, refresh]);

  // The first read failed (the engine is down or refused): say so, with a way to try again.
  if (!settings) {
    if (!error) return null;
    return (
      <section
        className="mb-4 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4"
        data-testid="auto-save-backup-error"
      >
        <header className="mb-1 flex flex-wrap items-center gap-2">
          <History size={14} />
          <h3 className="flex-1 basis-[8rem] text-sm font-semibold">
            {tr("saves_auto_title", undefined, "Automatic backups")}
          </h3>
          <Button variant="ghost" size="sm" onClick={() => void refresh()}>
            {tr("saves_auto_retry", undefined, "Try again")}
          </Button>
        </header>
        <p className="text-xs text-[var(--color-bad)]">
          {tr("saves_auto_load_failed", { error }, "Couldn't read the backup settings: {error}")}
        </p>
      </section>
    );
  }
  const dir = settings.dir;

  async function chooseFolder() {
    const picked = await pickPath({
      mode: "folder",
      title: tr("saves_auto_pick", undefined, "Folder for automatic save backups"),
    });
    if (picked) await set({ dir: picked });
  }

  return (
    <section
      className="mb-4 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4"
      data-testid="auto-save-backup"
    >
      <header className="mb-1 flex flex-wrap items-center gap-2">
        <History size={14} />
        <h3 className="flex-1 basis-[8rem] text-sm font-semibold">
          {tr("saves_auto_title", undefined, "Automatic backups")}
        </h3>
        <Button
          variant="ghost"
          size="sm"
          leftIcon={<Play size={12} />}
          onClick={() => void runNow()}
          disabled={!settings.enabled || running}
        >
          {tr("saves_auto_run_now", undefined, "Back up now")}
        </Button>
      </header>
      <p className="mb-3 text-xs text-[var(--color-muted)]">
        {desktop
          ? tr(
              "saves_auto_note_engine_v2",
              undefined,
              "While PS5Upload runs, saves that changed on your consoles are copied to this folder as often as chosen below. A save still being written is left for the next round.",
            )
          : tr(
              "saves_auto_note_server_v2",
              undefined,
              "The engine copies saves that changed on your consoles to this folder as often as chosen below, even with no browser open. In Docker, use a folder inside a mounted volume, such as /data/save-backups.",
            )}
      </p>
      <div className="grid gap-2 text-sm sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
        {desktop ? (
          <>
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
          </>
        ) : (
          <>
            <label className="min-w-0">
              <span className="text-xs text-[var(--color-muted)]">
                {tr("saves_auto_folder_label", undefined, "Backup folder")}
              </span>
              <input
                type="text"
                value={dirDraft}
                placeholder="/data/save-backups"
                onChange={(e) => setDirDraft(e.target.value)}
                className="mt-1 w-full rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-3 py-1 font-mono text-xs text-[var(--color-text)]"
              />
            </label>
            <Button
              variant="secondary"
              size="sm"
              onClick={() => void set({ dir: dirDraft.trim() })}
              disabled={dirDraft.trim() === dir}
            >
              {tr("save", undefined, "Save")}
            </Button>
          </>
        )}
        {intervalSupported(settings) && (
          <IntervalChoice
            minutes={settings.interval_minutes}
            onChange={(interval_minutes) => {
              if (interval_minutes !== settings.interval_minutes) void set({ interval_minutes });
            }}
          />
        )}
        <label htmlFor="saves-auto-keep" className="text-xs text-[var(--color-muted)]">
          {tr("saves_auto_keep", undefined, "Versions to keep")}
        </label>
        <input
          id="saves-auto-keep"
          type="number"
          min={1}
          max={50}
          defaultValue={settings.keep}
          key={settings.keep}
          onBlur={(e) => {
            const keep = clampKeep(Number(e.target.value));
            if (keep !== settings.keep) void set({ keep });
          }}
          className="w-20 justify-self-start rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-2 py-1 text-sm text-[var(--color-text)] sm:justify-self-end"
        />
      </div>
      {desktop && dir.trim() && (
        <p className="mt-2 text-xs text-[var(--color-muted)]" data-testid="auto-save-backup-manual">
          {tr("saves_auto_manual_note", undefined, "Backups you make by hand go to this folder too.")}
        </p>
      )}
      {(settings.old_dirs?.length ?? 0) > 0 && (
        <p className="mt-2 break-all text-xs text-[var(--color-muted)]" data-testid="auto-save-backup-old-dirs">
          {tr(
            "saves_auto_old_dirs",
            { dirs: settings.old_dirs!.join(", ") },
            "Backups in folders used before still show and can be restored: {dirs}",
          )}
        </p>
      )}
      <label className="mt-3 flex items-center gap-2 border-t border-[var(--color-border)] pt-3 text-sm">
        <input
          type="checkbox"
          checked={settings.enabled}
          disabled={!dir.trim()}
          onChange={(e) => void set({ enabled: e.target.checked })}
        />
        {tr("saves_auto_enable", undefined, "Back up changed saves automatically")}
      </label>
      {error && <p className="mt-2 text-xs text-[var(--color-bad)]">{error}</p>}
      {(running || (status && status.last_at > 0)) && (
        <p className="mt-2 text-xs text-[var(--color-muted)]">
          {running
            ? tr("saves_auto_running", undefined, "Backing up now…")
            : tr(
                "saves_auto_last_at",
                {
                  when: formatDate(new Date(status!.last_at)),
                  ok: status!.backed_up,
                  failed: status!.failed,
                },
                "Last round ({when}): {ok} backed up, {failed} failed.",
              )}
          {!running && status?.last_error ? ` ${status.last_error}` : ""}
        </p>
      )}
    </section>
  );
}
