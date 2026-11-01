import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { FolderOpen, Gamepad2, History } from "lucide-react";

import { Button } from "../../components";
import { saveBackupApi, type Ps2Save } from "../../api/saveBackup";
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

/** The PlayStation 2 emulators' memory cards and saved states (Crux2, PS5SX2), and the PlayStation
 *  emulator PSSwanStation's, with their backups: a group for each kind, shown only when the console
 *  has one. Each emulator's two saves are kept under its own title ID, the user number telling them
 *  apart (0 the cards, 1 the saved states). */
export default function Ps2Saves({
  host,
  refreshKey,
}: {
  host: string;
  refreshKey: number;
}) {
  const tr = useTr();
  const [saves, setSaves] = useState<Ps2Save[]>([]);
  // Per emulator: a backup takes both its saves, so both rows' backup lists load again.
  const [reloads, setReloads] = useState<Record<string, number>>({});
  const backedUp = useCallback(
    (app: string) => setReloads((r) => ({ ...r, [app]: (r[app] ?? 0) + 1 })),
    [],
  );

  useEffect(() => {
    setSaves([]);
    if (!host.trim()) return;
    const probe = captureHostProbe();
    // An older engine, or a console that does not answer: no PS2 group.
    saveBackupApi
      .ps2Saves(hostOf(host))
      .then((r) => {
        if (!probe.isStale()) setSaves(r.saves ?? []);
      })
      .catch(() => {});
  }, [host, refreshKey]);

  if (saves.length === 0) return null;
  const ps1 = saves.filter((s) => s.kind === "ps1");
  const ps2 = saves.filter((s) => s.kind !== "ps1");

  return (
    <>
      {ps2.length > 0 && (
        <EmulatorGroup
          host={host}
          saves={ps2}
          title={tr("saves_ps2_title", undefined, "PS2 emulators (Crux2, PS5SX2)")}
          testId="ps2-saves"
          reloads={reloads}
          onBackedUp={backedUp}
        />
      )}
      {ps1.length > 0 && (
        <EmulatorGroup
          host={host}
          saves={ps1}
          title={tr("saves_ps1_title", undefined, "PS1 emulator (PSSwanStation)")}
          testId="ps1-saves"
          reloads={reloads}
          onBackedUp={backedUp}
        />
      )}
    </>
  );
}

/** One kind of emulator's saves: each emulator's memory cards and saved states. */
function EmulatorGroup({
  host,
  saves,
  title,
  testId,
  reloads,
  onBackedUp,
}: {
  host: string;
  saves: Ps2Save[];
  title: string;
  testId: string;
  reloads: Record<string, number>;
  onBackedUp: (app: string) => void;
}) {
  const tr = useTr();
  const apps = [...new Set(saves.map((s) => s.title_id))].sort();

  return (
    <section className="surface-panel mt-3 p-5" data-testid={testId}>
      <header className="mb-3 flex items-center gap-2">
        <Gamepad2 size={16} aria-hidden />
        <h3 className="text-sm font-semibold">{title}</h3>
      </header>
      <div className="space-y-3">
        {apps.map((app) => {
          const mine = saves
            .filter((s) => s.title_id === app)
            .sort((a, b) => a.user_id - b.user_id);
          const name = mine[0]?.emulator || app;
          const running = mine.some((s) => s.running === true);
          return (
            <div key={app} data-title-id={app}>
              <div className="mb-1 flex flex-wrap items-center gap-2">
                <span className="text-sm font-semibold">{name}</span>
                <code className="text-xs text-[var(--color-muted)]">{app}</code>
              </div>
              <p
                className={
                  "mb-2 text-xs " +
                  (running
                    ? "text-[var(--color-warn)]"
                    : "text-[var(--color-muted)]")
                }
                data-testid="ps2-saves-note"
              >
                {running
                  ? tr(
                      "saves_ps2_running",
                      { name },
                      "{name} is running. Close it before restoring.",
                    )
                  : tr(
                      "saves_ps2_close_note",
                      { name },
                      "Close {name} before restoring.",
                    )}
              </p>
              <ul className="space-y-2">
                {mine.map((s) => (
                  <Ps2SaveRow
                    key={s.path}
                    host={host}
                    save={s}
                    reload={reloads[app] ?? 0}
                    onBackedUp={onBackedUp}
                  />
                ))}
              </ul>
            </div>
          );
        })}
      </div>
    </section>
  );
}

function Ps2SaveRow({
  host,
  save,
  reload,
  onBackedUp,
}: {
  host: string;
  save: Ps2Save;
  reload: number;
  onBackedUp: (app: string) => void;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const dir = useAutoSaveBackupStore((s) => s.settings?.dir ?? "");
  const [busy, setBusy] = useState(false);
  const entry: SaveEntry = {
    title_id: save.title_id,
    user_id: save.user_id,
    path: save.path,
    size: save.size,
    mtime: save.mtime,
    kind: save.kind,
  };
  const label =
    save.user_id === 0
      ? tr("saves_ps2_cards", undefined, "Memory cards")
      : tr("saves_ps2_states", undefined, "Saved states");

  // A snapshot takes every save of the title: the emulator's cards and its saved states.
  const backUp = useCallback(async () => {
    setBusy(true);
    try {
      await saveBackupApi.snapshot({
        addr: hostOf(host),
        title_id: save.title_id,
      });
      pushNotification(
        "success",
        tr(
          "saves_switch_backed_up",
          { title: save.emulator },
          "Backed up {title}",
        ),
      );
      onBackedUp(save.title_id);
    } catch (e) {
      pushNotification(
        "error",
        tr(
          "saves_switch_backup_failed",
          { title: save.emulator },
          "Couldn't back up {title}",
        ),
        { body: e instanceof Error ? e.message : String(e) },
      );
    } finally {
      setBusy(false);
    }
  }, [host, save.title_id, save.emulator, tr, onBackedUp]);

  return (
    <li
      className="rounded-[var(--radius-field)] bg-[var(--color-surface)] px-2 py-1.5 text-xs"
      data-testid="ps2-save"
    >
      <div className="flex flex-wrap items-center gap-3">
        <div className="min-w-0 flex-1">
          <div className="font-medium">{label}</div>
          <div className="text-[var(--color-muted)]">
            {tr(
              "saves_switch_changed",
              { when: formatDate(save.mtime * 1000, "datetime") },
              "Last changed {when}",
            )}{" "}
            · {formatBytes(save.size)}
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
                  "saves_ps2_backup_hint",
                  undefined,
                  "Back up this emulator's memory cards and saved states now, into the folder for automatic backups.",
                )
              : tr(
                  "saves_switch_need_dir",
                  undefined,
                  "Choose a folder for automatic backups first.",
                )
          }
          data-testid="ps2-save-backup"
        >
          {tr("saves_switch_backup_now", undefined, "Back up now")}
        </Button>
        <Button
          variant="ghost"
          size="sm"
          leftIcon={<FolderOpen size={11} />}
          onClick={() => openInFileSystem(navigate, save.folder || save.path)}
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
