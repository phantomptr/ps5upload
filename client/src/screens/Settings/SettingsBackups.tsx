// The settings backups the app keeps by itself (8.0.1, lib/settingsBackup): the engine's copies
// and every app's copy on the connected console. Restoring asks first, keeps a copy of the
// settings it replaces (or says it could not), then reloads: the running stores would otherwise
// write their in-memory state back over what was restored.

import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { ChevronDown, History, HardDrive, Monitor, RotateCcw, Save } from "lucide-react";

import { Button } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { formatBytes } from "../../lib/format";
import { syncId } from "../../lib/meshShare";
import {
  applyBundle,
  backupBeforeChange,
  buildBackupBundle,
  getEngineBackup,
  listConsoleBackups,
  listEngineBackups,
  saveConsoleBackup,
  saveEngineBackup,
  type BackupEntry,
  type Bundle,
  type ConsoleBackup,
} from "../../lib/settingsBackup";
import { backupLabel } from "../../layout/useMeshSync";
import { useConnectionStore } from "../../state/connection";
import { useTr, type Translator } from "../../state/lang";
import { meshFor, useMeshStore } from "../../state/mesh";
import { useToastStore } from "../../state/toasts";
import { BACKUP_ROWS_SHOWN, groupBackups, type BackupDeviceGroup } from "./backupGroups";
import { formatDate } from "../../lib/formatDate";

function reasonText(tr: Translator, r: string): string {
  switch (r) {
    case "daily":
      return tr("backup_reason_daily", undefined, "Daily");
    case "before-sync":
      return tr("backup_reason_before_sync", undefined, "Before a sync from your other devices");
    case "before-restore":
      return tr("backup_reason_before_restore", undefined, "Before a restore");
    default:
      return tr("backup_reason_manual", undefined, "Made by you");
  }
}

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

export function SettingsBackups() {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const host = useConnectionStore((s) => s.host);
  const up = useConnectionStore((s) => s.payloadStatus === "up");
  const engineName = useMeshStore((s) => s.me?.name);
  const supported = useMeshStore((s) => meshFor(s, host).supported === true);
  const [local, setLocal] = useState<BackupEntry[] | null>(null);
  const [onConsole, setOnConsole] = useState<ConsoleBackup[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const consoleOk = !!host && up && supported;
  const me = syncId();
  const label = backupLabel(engineName);
  const seq = useRef(0);

  // Only reads: an error from an action stays on screen until the next action.
  const load = useCallback(async () => {
    const mine = ++seq.current;
    const [l, c] = await Promise.all([
      listEngineBackups().catch((e: unknown) => {
        if (mine === seq.current) setError(errText(e));
        return null;
      }),
      consoleOk ? listConsoleBackups(host).catch(() => null) : Promise.resolve(null),
    ]);
    if (mine !== seq.current) return;
    setLocal(l);
    setOnConsole(c);
  }, [consoleOk, host]);

  useEffect(() => {
    void load();
  }, [load]);

  const backUpNow = async () => {
    setBusy(true);
    setError(null);
    try {
      const b = buildBackupBundle("manual", label);
      await saveEngineBackup(b);
      if (consoleOk) await saveConsoleBackup(host, me, label, b);
      useToastStore.getState().push({ tone: "success", message: tr("backup_saved", undefined, "Settings backed up") });
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
    await load();
  };

  const restore = async (get: () => Promise<Bundle>, what: string) => {
    const ok = await confirm({
      title: tr("backup_restore_title", undefined, "Restore these settings?"),
      message: tr(
        "backup_restore_body",
        { label: what },
        `The settings from ${what} replace this app's. A copy of the current ones is kept first, so this can be undone from the same list.`,
      ),
      confirmLabel: tr("backup_restore", undefined, "Restore"),
    });
    if (!ok) return;
    setBusy(true);
    setError(null);
    try {
      const bundle = await get();
      try {
        await backupBeforeChange("before-restore", label);
      } catch (e) {
        const go = await confirm({
          title: tr("backup_no_copy_title", undefined, "No copy of the current settings could be kept"),
          message: tr(
            "backup_no_copy_body",
            { reason: errText(e) },
            `${errText(e)}. Restore anyway? The current settings cannot be brought back afterwards.`,
          ),
          confirmLabel: tr("backup_restore_anyway", undefined, "Restore anyway"),
          destructive: true,
        });
        if (!go) return;
      }
      applyBundle(bundle);
      // Reload at once: the stores in memory would write their state back over the restore.
      window.location.reload();
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const when = (ms: number) => formatDate(ms);

  const engineGroups = groupBackups(
    (local ?? []).map((b) => ({
      device: b.device || tr("backup_device_unnamed", undefined, "Unnamed device"),
      at: b.at,
      sig: `${b.reason}|${b.keys}|${b.bytes}`,
      item: b,
    })),
  );
  const consoleGroups = groupBackups(
    (onConsole ?? []).map((c) => ({
      device: c.name || tr("mesh_another_device", undefined, "Another device"),
      at: c.at,
      sig: `${c.app === me}|${c.keys}|${JSON.stringify(c.bundle.local_storage).length}`,
      item: c,
    })),
  );

  return (
    <div className="space-y-3 text-sm">
      {dialog}
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "backup_auto_hint_v2",
          undefined,
          "Backed up daily and before a sync or restore. This app keeps its last 14; each console keeps every device's latest.",
        )}
      </p>
      <Button size="sm" variant="secondary" loading={busy} leftIcon={<Save size={12} />} onClick={() => void backUpNow()}>
        {tr("backup_now", undefined, "Back up now")}
      </Button>

      <BackupSection
        testId="backups-engine"
        icon={<Monitor size={12} aria-hidden />}
        title={tr("backup_on_engine", undefined, "Kept by this app")}
        groups={list(engineGroups, local)}
        busy={busy}
        when={when}
        detail={(b) => (
          <>
            {reasonText(tr, b.reason)} · {tr("backup_keys", { count: b.keys }, `Settings: ${b.keys}`)} · {formatBytes(b.bytes)}
          </>
        )}
        onRestore={(b, device) => void restore(() => getEngineBackup(b.id), `${device}, ${when(b.at)}`)}
      />

      {consoleOk && (
        <BackupSection
          testId="backups-console"
          icon={<HardDrive size={12} aria-hidden />}
          title={tr("backup_on_console", undefined, "Kept by this console")}
          groups={list(consoleGroups, onConsole)}
          busy={busy}
          when={when}
          detail={(c) => (
            <>
              {c.app === me && (
                <span className="text-[var(--color-accent-bright)]">{tr("drive_you", undefined, "you")} · </span>
              )}
              {tr("backup_keys", { count: c.keys }, `Settings: ${c.keys}`)}
              {c.bundle.meta?.dropped?.length
                ? ` · ${tr("backup_trimmed", { count: c.bundle.meta.dropped.length }, `Large items left out: ${c.bundle.meta.dropped.length}`)}`
                : ""}
            </>
          )}
          onRestore={(c, device) => void restore(() => Promise.resolve(c.bundle), `${device}, ${when(c.at)}`)}
        />
      )}
      {error && <p className="text-xs text-[var(--color-bad)]">{error}</p>}
    </div>
  );
}

/** null while loading (nothing shown), [] once known to be empty. */
function list<T>(groups: BackupDeviceGroup<T>[], raw: unknown[] | null): BackupDeviceGroup<T>[] | null {
  return raw === null ? null : groups;
}

/** One list of copies: the newest few rows, grouped under the device that made them, and
 *  "Show all" for the rest. A row that stands for several look-alike copies says how many;
 *  its Restore uses the newest, and its ×N badge unfolds the others, each with its own Restore
 *  (look-alike is not identical, so every copy must stay restorable). */
function BackupSection<T>({
  testId,
  icon,
  title,
  groups,
  busy,
  when,
  detail,
  onRestore,
}: {
  testId: string;
  icon: ReactNode;
  title: string;
  groups: BackupDeviceGroup<T>[] | null;
  busy: boolean;
  when: (ms: number) => string;
  detail: (item: T) => ReactNode;
  onRestore: (item: T, device: string) => void;
}) {
  const tr = useTr();
  const [all, setAll] = useState(false);
  const [unfolded, setUnfolded] = useState<ReadonlySet<string>>(() => new Set());
  const toggleRow = (key: string) =>
    setUnfolded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  const short = (ms: number) => formatDate(ms);
  const rowCount = (groups ?? []).reduce((n, g) => n + g.rows.length, 0);
  let budget = all ? Infinity : BACKUP_ROWS_SHOWN;
  const shown = (groups ?? [])
    .map((g) => {
      const rows = g.rows.slice(0, Math.max(0, budget));
      budget -= rows.length;
      return { ...g, rows };
    })
    .filter((g) => g.rows.length > 0);
  return (
    <section className="space-y-1" data-testid={testId}>
      <p className="flex items-center gap-1.5 text-xs font-medium">
        {icon}
        {title}
      </p>
      {groups?.length === 0 && (
        <p className="text-xs text-[var(--color-muted)]">{tr("backup_none", undefined, "None yet.")}</p>
      )}
      {shown.length > 0 && (
        <div className="divide-y divide-[var(--color-border)] rounded-[var(--radius-input,0.5rem)] border border-[var(--color-border)]">
          {shown.map((g) => (
            <div key={g.device}>
              <p className="flex items-center gap-1.5 truncate px-3 pt-1.5 text-[11px] font-medium text-[var(--color-muted)]">
                {g.device}
              </p>
              <ul>
                {g.rows.map((r) => {
                  const key = `${g.device}|${r.newest.at}|${r.newest.sig}`;
                  const open = unfolded.has(key);
                  const restoreButton = (item: T) => (
                    <Button
                      size="sm"
                      variant="secondary"
                      className="shrink-0"
                      disabled={busy}
                      leftIcon={<RotateCcw size={12} />}
                      aria-label={tr("backup_restore", undefined, "Restore")}
                      onClick={() => onRestore(item, g.device)}
                    >
                      {/* Icon-only on a phone: the label would squeeze the date out. */}
                      <span className="max-sm:hidden">{tr("backup_restore", undefined, "Restore")}</span>
                    </Button>
                  );
                  return (
                    <li key={key} data-testid="backup-row">
                      <div className="flex items-center gap-2 px-3 py-1.5">
                        <History size={12} className="shrink-0 text-[var(--color-muted)]" aria-hidden />
                        <div className="min-w-0 flex-1 text-xs">
                          <p className="flex min-w-0 items-center gap-1.5">
                            <span className="truncate">{short(r.newest.at)}</span>
                            {r.count > 1 && (
                              <button
                                type="button"
                                data-testid="backup-alike-toggle"
                                aria-expanded={open}
                                onClick={() => toggleRow(key)}
                                className="inline-flex shrink-0 items-center gap-0.5 rounded-full border border-[var(--color-border)] px-1.5 text-[10px] text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)] max-md:min-h-6"
                                title={tr(
                                  "backup_alike_toggle",
                                  { count: r.count, since: when(r.oldestAt) },
                                  `${r.count} alike copies since ${when(r.oldestAt)}. Show each to restore an older one.`,
                                )}
                              >
                                ×{r.count}
                                <ChevronDown
                                  size={10}
                                  aria-hidden
                                  className={open ? "rotate-180 transition-transform" : "transition-transform"}
                                />
                              </button>
                            )}
                          </p>
                          <p className="truncate text-[var(--color-muted)]">{detail(r.newest.item)}</p>
                        </div>
                        {restoreButton(r.newest.item)}
                      </div>
                      {open && (
                        <ul className="border-t border-[var(--color-border)] bg-[var(--color-surface-2)]">
                          {r.copies.slice(1).map((c) => (
                            <li
                              key={`${c.at}|${c.sig}`}
                              data-testid="backup-copy"
                              className="flex items-center gap-2 py-1 pe-3 ps-8"
                            >
                              <div className="min-w-0 flex-1 text-xs">
                                <p className="truncate">{short(c.at)}</p>
                                <p className="truncate text-[var(--color-muted)]">{detail(c.item)}</p>
                              </div>
                              {restoreButton(c.item)}
                            </li>
                          ))}
                        </ul>
                      )}
                    </li>
                  );
                })}
              </ul>
            </div>
          ))}
        </div>
      )}
      {rowCount > BACKUP_ROWS_SHOWN && (
        <Button size="sm" variant="ghost" onClick={() => setAll((v) => !v)}>
          {all
            ? tr("backup_show_fewer", undefined, "Show fewer")
            : tr("backup_show_all", { count: rowCount }, `Show all (${rowCount})`)}
        </Button>
      )}
    </section>
  );
}
