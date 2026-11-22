import { useEffect, useState, type ReactNode } from "react";
import { FolderOpen, History, Play } from "lucide-react";

import { Button } from "../../components";
import { saveBackupApi, type ConsoleCopyStatus, type ConsoleDrive } from "../../api/saveBackup";
import { hostOf } from "../../lib/addr";
import { pickPath } from "../../lib/pickPath";
import { isTauriEnv } from "../../lib/tauriEnv";
import { formatDate } from "../../lib/formatDate";
import {
  DEFAULT_INTERVAL_MINUTES,
  clampKeep,
  consoleCopySupported,
  intervalChoices,
  intervalSupported,
  numberBoxLeft,
  useAutoSaveBackupStore,
} from "../../state/autoSaveBackup";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";
import { BackupLayoutChoice, layoutSupported } from "./BackupLayoutChoice";
import { BackupProgressModal, BackupRunBar, useBackupRuns } from "./BackupProgress";
import { FolderBackupChoice, foldersSupported } from "./FolderBackupChoice";

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

/** "M.2 drive (/mnt/ext0)", "USB drive (/mnt/usb0)". */
export function consoleDriveLabel(path: string, tr: Tr): string {
  return path.startsWith("/mnt/usb")
    ? tr("saves_auto_console_drive_usb", { path }, "USB drive ({path})")
    : tr("saves_auto_console_drive_ext", { path }, "M.2 drive ({path})");
}

/** The drive to start with when the copy is switched on: the first M.2, else the first USB. */
export function firstConsoleDrive(drives: readonly ConsoleDrive[]): string | null {
  return (drives.find((d) => d.kind === "ext") ?? drives[0])?.path ?? null;
}

/** "Also keep a copy on a PS5 drive": each backup is copied to the console's M.2 or USB drive
 *  too, so saves outlive a reset of its internal storage. `drives` is null while it is asked
 *  (or with no console connected); `drive` is the engine's setting ("" for off). Used by the
 *  automatic backups and the network-share backups (#885): `testId` keeps their ids apart, and
 *  `children` shows under the drive picker while the copy is on. */
export function ConsoleCopyChoice({
  drive,
  drives,
  connected,
  loadError,
  status,
  onChange,
  testId = "auto-save-backup-console",
  children,
}: {
  drive: string;
  drives: readonly ConsoleDrive[] | null;
  connected: boolean;
  loadError: string | null;
  status: ConsoleCopyStatus | null | undefined;
  onChange: (drive: string) => void;
  testId?: string;
  children?: ReactNode;
}) {
  const tr = useTr();
  const on = drive.trim() !== "";
  const first = drives ? firstConsoleDrive(drives) : null;
  const listed = drives?.some((d) => d.path === drive) ?? false;
  return (
    <div className="mt-3 border-t border-[var(--color-border)] pt-3 text-sm" data-testid={`${testId}`}>
      <label className="flex items-center gap-2">
        <input
          type="checkbox"
          checked={on}
          disabled={!on && !first}
          onChange={(e) => onChange(e.target.checked ? (first ?? "") : "")}
          data-testid={`${testId}-toggle`}
        />
        {tr("saves_auto_console_copy", undefined, "Also keep a copy on a PS5 drive")}
      </label>
      <p className="mt-1 text-xs text-[var(--color-muted)]">
        {tr(
          "saves_auto_console_note",
          undefined,
          "Each backup is also copied to ps5upload/save-backups on the PS5's M.2 or USB drive, so your saves survive a reset that wipes the internal storage.",
        )}
      </p>
      {!connected ? (
        <p className="mt-1 text-xs text-[var(--color-muted)]" data-testid={`${testId}-no-console`}>
          {tr("saves_auto_console_no_console", undefined, "Connect to a PS5 to choose its drive.")}
        </p>
      ) : loadError ? (
        <p className="mt-1 text-xs text-[var(--color-bad)]">{loadError}</p>
      ) : drives && drives.length === 0 ? (
        <p className="mt-1 text-xs text-[var(--color-muted)]" data-testid={`${testId}-none`}>
          {tr("saves_auto_console_no_drive", undefined, "This PS5 has no M.2 or USB drive to copy to.")}
        </p>
      ) : null}
      {on && (
        <div className="mt-2 grid gap-2 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
          <label htmlFor={`${testId}-select`} className="text-xs text-[var(--color-muted)]">
            {tr("saves_auto_console_drive_label", undefined, "PS5 drive")}
          </label>
          <select
            id={`${testId}-select`}
            data-testid={`${testId}-drive`}
            value={drive}
            onChange={(e) => onChange(e.target.value)}
            className="select w-auto! justify-self-start text-sm sm:justify-self-end"
          >
            {!listed && (
              <option value={drive}>
                {tr("saves_auto_console_drive_missing", { path: drive }, "{path} (not plugged in)")}
              </option>
            )}
            {(drives ?? []).map((d) => (
              <option key={d.path} value={d.path}>
                {consoleDriveLabel(d.path, tr)}
              </option>
            ))}
          </select>
          {children}
        </div>
      )}
      {status && status.at > 0 && (
        <p className="mt-2 text-xs text-[var(--color-muted)]" data-testid={`${testId}-status`}>
          {tr(
            "saves_auto_console_last",
            {
              when: formatDate(new Date(status.at)),
              ok: status.copied,
              skipped: status.skipped,
              failed: status.failed,
            },
            "Last copy to the PS5 drive ({when}): {ok} copied, {skipped} skipped, {failed} failed.",
          )}
          {status.note ? ` ${status.note}` : ""}
        </p>
      )}
    </div>
  );
}

/** The console's drives for the copy, asked while the engine can make one and a console is
 *  connected. */
export function useConsoleDrives(enabled: boolean) {
  const host = useConnectionStore((s) => s.host);
  const addr = host?.trim() ? hostOf(host.trim()) : "";
  const [drives, setDrives] = useState<ConsoleDrive[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    setDrives(null);
    setError(null);
    if (!enabled || !addr) return;
    let live = true;
    saveBackupApi.consoleDrives(addr).then(
      (d) => {
        if (live) setDrives(d);
      },
      (e) => {
        if (live) setError(e instanceof Error ? e.message : String(e));
      },
    );
    return () => {
      live = false;
    };
  }, [enabled, addr]);
  return { drives, error, connected: !!addr };
}

/** The automatic save backup's switch, folder, how often and how many versions it keeps. The
 *  engine runs the backups (with no window open in Docker); this card changes its settings. */
export default function AutoBackupCard({
  owners = [],
}: {
  /** The users with saves on the console shown (named where the console names them). */
  owners?: ReadonlyArray<{ id: number; name: string | null }>;
} = {}) {
  const tr = useTr();
  const { settings, status, error, refresh, set, runNow } = useAutoSaveBackupStore();
  const [progressOpen, setProgressOpen] = useState(false);
  const [dirDraft, setDirDraft] = useState("");
  const desktop = isTauriEnv();
  const copySupported = consoleCopySupported(settings);
  const consoleDrives = useConsoleDrives(copySupported);

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
  // The last run's record (#895): asked again whenever a run starts or ends.
  const runs = useBackupRuns("local", false, `${running}-${status?.last_at ?? 0}`);

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
            const left = numberBoxLeft(e.target.value, settings.keep, clampKeep);
            e.target.value = left.show;
            if (left.save !== null) void set({ keep: left.save });
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
      {copySupported && (
        <ConsoleCopyChoice
          drive={settings.console_drive ?? ""}
          drives={consoleDrives.drives}
          connected={consoleDrives.connected}
          loadError={consoleDrives.error}
          status={status?.console_copy}
          onChange={(console_drive) => {
            if (console_drive !== settings.console_drive) void set({ console_drive });
          }}
        />
      )}
      {layoutSupported(settings) && (
        <BackupLayoutChoice
          testId="auto-save-backup-layout"
          settings={settings}
          status={status?.decrypted}
          onChange={(patch) => void set(patch)}
        />
      )}
      {foldersSupported(settings) && settings.dir && (
        <FolderBackupChoice
          testId="auto-save-backup-folders"
          folders={settings.folders ?? []}
          onChange={(folders) => void set({ folders })}
          from="local"
          driveCopies={!!settings.console_drive}
        />
      )}
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
      {settings.dir && (
        <BackupRunBar
          which="local"
          runs={runs}
          running={running}
          onOpen={() => setProgressOpen(true)}
          onStarted={() => void refresh()}
          testId="auto-save-backup-run"
        />
      )}
      {progressOpen && (
        <BackupProgressModal which="local" open onClose={() => setProgressOpen(false)} owners={owners} />
      )}
    </section>
  );
}
