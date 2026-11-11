import { useCallback, useEffect, useMemo, useState } from "react";
import { ArchiveRestore, FolderOpen, RefreshCw } from "lucide-react";
import {
  Button,
  Card,
  Checkbox,
  ErrorCard,
  PageHeader,
  ProgressBar,
  Select,
  SuccessCard,
  WarningCard,
} from "../../components";
import { profileInfo, type ProfileUser } from "../../api/ps5";
import {
  consoleBackupApi,
  groupsInBackup,
  matchUser,
  usersWithSaves,
  type BackupGroupId,
  type BackupManifest,
  type BackupStatus,
} from "../../api/consoleBackup";
import { mgmtAddr } from "../../lib/addr";
import { formatBytes } from "../../lib/format";
import { humanizePs5Error } from "../../lib/humanizeError";
import { pickPath } from "../../lib/pickPath";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";
import { useConfirm } from "../../components/ConfirmDialog";
import { ResetGuide } from "./Guide";
import { restoreAfterConfirm, restoreConfirmOptions } from "./restoreConfirm";

type Tr = ReturnType<typeof useTr>;
type View = "backup" | "restore" | "guide";

function groupName(tr: Tr, id: BackupGroupId): string {
  switch (id) {
    case "saves":
      return tr("cb_group_saves", undefined, "Save data");
    case "captures":
      return tr("cb_group_captures", undefined, "Screenshots and clips");
    case "settings":
      return tr("cb_group_settings", undefined, "Payload settings");
    case "homebrew":
      return tr("cb_group_homebrew", undefined, "Homebrew");
    default:
      return tr("cb_group_other", undefined, "Other folders in /data");
  }
}

function groupWhy(tr: Tr, id: BackupGroupId): string {
  switch (id) {
    case "saves":
      return tr(
        "cb_why_saves",
        undefined,
        "Every game's saves, for every user. They only open on this PS5, so they can't move to another console this way.",
      );
    case "captures":
      return tr(
        "cb_why_captures",
        undefined,
        "Your photos and videos. After a restore they're back in Screenshots & Clips here, but the PS5's own Media Gallery may not list them.",
      );
    case "settings":
      return tr(
        "cb_why_settings",
        undefined,
        "Settings for etaHEN, ShadowMount+, the autoloader and Payload Manager. Small, and the quickest way to get your setup back.",
      );
    case "homebrew":
      return tr(
        "cb_why_homebrew",
        undefined,
        "/data/homebrew: apps and game images kept there. This can be very large.",
      );
    default:
      return tr(
        "cb_why_other",
        undefined,
        "Everything else in /data that payloads and homebrew keep, which the app can't identify one by one.",
      );
  }
}

function useBackupStatus() {
  const [status, setStatus] = useState<BackupStatus | null>(null);
  const refresh = useCallback(async () => {
    try {
      setStatus(await consoleBackupApi.status());
    } catch {
      // An older engine, or one restarting: the next poll tells.
    }
  }, []);
  const busy = !!status && ["scanning", "backing_up", "restoring"].includes(status.state);
  useEffect(() => {
    void refresh();
  }, [refresh]);
  useEffect(() => {
    if (!busy) return;
    const id = window.setInterval(() => void refresh(), 700);
    return () => window.clearInterval(id);
  }, [busy, refresh]);
  return { status, refresh, busy };
}

/** How a run is getting on, and what it found. */
function Progress({ status, onCancel }: { status: BackupStatus; onCancel: () => void }) {
  const tr = useTr();
  const fraction = status.total_bytes > 0 ? status.bytes / status.total_bytes : null;
  return (
    <Card>
      <div className="flex items-center gap-3">
        <div className="min-w-0 flex-1">
          <div className="text-sm font-medium">
            {tr("cb_progress", { step: status.step, steps: status.steps }, `Step ${status.step} of ${status.steps}`)}
          </div>
          <div className="truncate text-xs text-[var(--color-muted)]">{status.current}</div>
        </div>
        <Button variant="secondary" size="sm" onClick={onCancel} data-testid="cb-cancel">
          {tr("cb_cancel", undefined, "Stop")}
        </Button>
      </div>
      <div className="mt-2">
        <ProgressBar value={fraction ?? (status.steps ? status.step / status.steps : 0)} />
      </div>
      <p className="mt-2 text-xs text-[var(--color-muted)]">
        {tr("cb_keep_open", undefined, "You can use other screens. Keep the PS5 awake until this finishes.")}
      </p>
    </Card>
  );
}

function Results({ status }: { status: BackupStatus }) {
  const tr = useTr();
  const failed = status.outcomes.filter((o) => o.result === "failed");
  const waiting = status.outcomes.filter((o) => o.result === "needs_save");
  return (
    <div className="space-y-2" data-testid="cb-results">
      {status.state === "failed" && (
        <ErrorCard title={tr("cb_failed", undefined, "This didn't finish")} detail={status.error ?? undefined} />
      )}
      {status.state === "cancelled" && (
        <WarningCard title={tr("cb_stopped", undefined, "Stopped. What was already copied stays where it is.")} />
      )}
      {status.state === "done" && status.kind !== "scanning" && failed.length === 0 && waiting.length === 0 && (
        <SuccessCard
          title={tr("cb_all_done", undefined, "All done")}
          detail={status.dir || undefined}
        />
      )}
      {waiting.length > 0 && (
        <WarningCard
          title={tr(
            "cb_needs_save",
            { n: waiting.length },
            `${waiting.length} saves are waiting. That user needs their own save of the game first.`,
          )}
          detail={
            <>
              <span className="block break-words">{waiting.map((o) => o.item).join(", ")}</span>
              {tr(
                "cb_needs_save_hint",
                undefined,
                "Start each game once as that user and save, then run this restore again. Doing it again is safe.",
              )}
            </>
          }
        />
      )}
      {failed.length > 0 && (
        <ErrorCard
          title={tr("cb_some_failed", { n: failed.length }, `${failed.length} couldn't be copied`)}
          // A list, not one string: each line names what failed, whatever the reason says.
          detail={
            <ul className="space-y-1">
              {/* One game can fail twice (two users' saves, #729): the key is the position. */}
              {failed.map((o, i) => (
                <li key={`${i}-${o.item}`} className="break-words">
                  {o.item}: {o.detail ? humanizePs5Error(o.detail) : ""}
                </li>
              ))}
            </ul>
          }
        />
      )}
    </div>
  );
}

function BackupPane({
  addr,
  status,
  busy,
  refresh,
}: {
  addr: string;
  status: BackupStatus | null;
  busy: boolean;
  refresh: () => Promise<void>;
}) {
  const tr = useTr();
  const [picked, setPicked] = useState<Set<BackupGroupId>>(new Set(["saves", "settings"]));
  const [dir, setDir] = useState("");
  const [error, setError] = useState<string | null>(null);
  const groups = status?.host === addr ? (status?.groups ?? []) : [];
  const total = groups.filter((g) => picked.has(g.id)).reduce((n, g) => n + g.bytes, 0);

  const run = async (work: () => Promise<unknown>) => {
    setError(null);
    try {
      await work();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
    await refresh();
  };

  return (
    <div className="space-y-4">
      <Card>
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="min-w-0">
            <h2 className="text-sm font-semibold">{tr("cb_choose_title", undefined, "What to back up")}</h2>
            <p className="text-xs text-[var(--color-muted)]">
              {tr("cb_choose_hint", undefined, "Measure first to see how much room each takes.")}
            </p>
          </div>
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<RefreshCw size={14} />}
            disabled={busy}
            onClick={() => void run(() => consoleBackupApi.scan(addr))}
            data-testid="cb-scan"
          >
            {tr("cb_scan", undefined, "Measure")}
          </Button>
        </div>
        <ul className="mt-3 space-y-3">
          {(["saves", "settings", "captures", "homebrew", "other"] as BackupGroupId[]).map((id) => {
            const g = groups.find((x) => x.id === id);
            return (
              <li key={id}>
                <Checkbox
                  checked={picked.has(id)}
                  disabled={busy}
                  onChange={(on) => {
                    const next = new Set(picked);
                    if (on) next.add(id);
                    else next.delete(id);
                    setPicked(next);
                  }}
                  label={
                    <span data-testid={`cb-group-${id}`}>
                      {groupName(tr, id)}
                      {g && <span className="ms-2 text-xs font-normal text-[var(--color-muted)]">{formatBytes(g.bytes)}</span>}
                    </span>
                  }
                  hint={groupWhy(tr, id)}
                />
                {g && id === "other" && g.folders.length > 0 && (
                  <p className="mt-0.5 break-all ps-6 text-xs text-[var(--color-muted)]">
                    {g.folders.map((f) => f.path.replace("/data/", "")).join(", ")}
                  </p>
                )}
              </li>
            );
          })}
        </ul>
      </Card>
      <Card>
        <h2 className="text-sm font-semibold">{tr("cb_left_out_title", undefined, "What stays out, and why")}</h2>
        <ul className="mt-2 list-disc space-y-1 ps-5 text-xs text-[var(--color-muted)]">
          <li>
            {tr(
              "cb_left_out_games",
              undefined,
              "Installed games and updates: copying them back doesn't install them. Keep your packages instead and install them again.",
            )}
          </li>
          <li>
            {tr(
              "cb_left_out_system",
              undefined,
              "/system_data (the console's database, accounts and settings): putting an old copy back can stop a PS5 starting, so the app never writes there.",
            )}
          </li>
          <li>
            {tr(
              "cb_left_out_logs",
              undefined,
              "Logs and temporary files, and this app's own folder, which comes back with the helper.",
            )}
          </li>
        </ul>
      </Card>
      <Card>
        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="secondary"
            leftIcon={<FolderOpen size={16} />}
            disabled={busy}
            onClick={async () => {
              const p = await pickPath({
                mode: "folder",
                title: tr("cb_pick_dest", undefined, "Folder to keep the backup in"),
              });
              if (p) setDir(p);
            }}
            data-testid="cb-pick-dest"
          >
            {tr("cb_pick_dest_button", undefined, "Choose folder")}
          </Button>
          <span className="min-w-0 flex-1 truncate text-xs text-[var(--color-muted)]">{dir}</span>
          <Button
            disabled={busy || !dir || picked.size === 0}
            onClick={() => void run(() => consoleBackupApi.backup(addr, dir, [...picked]))}
            data-testid="cb-start-backup"
          >
            {total > 0
              ? tr("cb_start_size", { size: formatBytes(total) }, `Back up ${formatBytes(total)}`)
              : tr("cb_start", undefined, "Back up")}
          </Button>
        </div>
        {error && <p className="mt-2 text-xs text-[var(--color-bad)]">{error}</p>}
      </Card>
    </div>
  );
}

function RestorePane({
  addr,
  busy,
  refresh,
}: {
  addr: string;
  busy: boolean;
  refresh: () => Promise<void>;
}) {
  const tr = useTr();
  const [dir, setDir] = useState("");
  const [manifest, setManifest] = useState<BackupManifest | null>(null);
  const [picked, setPicked] = useState<Set<BackupGroupId>>(new Set());
  const [here, setHere] = useState<ProfileUser[]>([]);
  const [map, setMap] = useState<Record<string, number>>({});
  const [error, setError] = useState<string | null>(null);

  const choose = async () => {
    setError(null);
    const p = await pickPath({
      mode: "folder",
      title: tr("cb_pick_source", undefined, "The backup folder (it has a backup.json)"),
    });
    if (!p) return;
    try {
      const m = await consoleBackupApi.inspect(p);
      const users = (await profileInfo(addr).catch(() => null))?.users ?? [];
      const guessed: Record<string, number> = {};
      for (const u of usersWithSaves(m)) {
        const to = matchUser(u, users);
        if (to !== null) guessed[String(u.uid)] = to;
      }
      setDir(p);
      setManifest(m);
      setPicked(new Set(groupsInBackup(m)));
      setHere(users);
      setMap(guessed);
    } catch (e) {
      setManifest(null);
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const fromUsers = useMemo(() => (manifest ? usersWithSaves(manifest) : []), [manifest]);
  const { confirm, dialog: confirmDialog } = useConfirm();
  const start = async () => {
    setError(null);
    try {
      // It replaces files on the PS5: asked first, like the snapshot restore.
      const ran = await restoreAfterConfirm(confirm, restoreConfirmOptions(tr), () =>
        consoleBackupApi.restore(addr, dir, [...picked], map),
      );
      if (!ran) return;
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
    await refresh();
  };

  return (
    <div className="space-y-4">
      <Card>
        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="secondary"
            leftIcon={<FolderOpen size={16} />}
            disabled={busy}
            onClick={() => void choose()}
            data-testid="cb-pick-source"
          >
            {tr("cb_pick_source_button", undefined, "Choose a backup")}
          </Button>
          <span className="min-w-0 flex-1 truncate text-xs text-[var(--color-muted)]">{dir}</span>
        </div>
        {error && <p className="mt-2 text-xs text-[var(--color-bad)]">{error}</p>}
      </Card>
      {manifest && (
        <>
          {!manifest.complete && (
            <WarningCard
              title={tr("cb_incomplete", undefined, "This backup was cut short, so it may not hold everything.")}
            />
          )}
          <Card>
            <h2 className="text-sm font-semibold">{tr("cb_restore_title", undefined, "What to put back")}</h2>
            <p className="text-xs text-[var(--color-muted)]">
              {tr(
                "cb_restore_hint",
                undefined,
                "Files with the same name on the PS5 are replaced. Nothing else there is deleted. Saves are backed up first.",
              )}
            </p>
            <ul className="mt-3 space-y-2">
              {groupsInBackup(manifest).map((id) => (
                <li key={id}>
                  <Checkbox
                    checked={picked.has(id)}
                    disabled={busy}
                    onChange={(on) => {
                      const next = new Set(picked);
                      if (on) next.add(id);
                      else next.delete(id);
                      setPicked(next);
                    }}
                    label={<span data-testid={`cb-restore-${id}`}>{groupName(tr, id)}</span>}
                  />
                </li>
              ))}
            </ul>
          </Card>
          {picked.has("saves") && fromUsers.length > 0 && (
            <Card>
              <h2 className="text-sm font-semibold">{tr("cb_users_title", undefined, "Whose saves go where")}</h2>
              <p className="text-xs text-[var(--color-muted)]">
                {tr(
                  "cb_users_hint",
                  undefined,
                  "A reset PS5 has new users, even with the same names. A save can only go into that user's own save of the game, so start each game once as them and save first.",
                )}
              </p>
              <ul className="mt-3 space-y-2">
                {fromUsers.map((u) => (
                  <li key={u.uid} className="flex flex-wrap items-center gap-3 text-sm">
                    <span className="min-w-0 flex-1 truncate">
                      {u.name || u.uid.toString(16).padStart(8, "0")}
                    </span>
                    <Select
                      aria-label={tr("cb_users_to", undefined, "Goes to")}
                      value={map[String(u.uid)] === undefined ? "" : String(map[String(u.uid)])}
                      onChange={(e) => {
                        const next = { ...map };
                        if (e.target.value) next[String(u.uid)] = Number(e.target.value);
                        else delete next[String(u.uid)];
                        setMap(next);
                      }}
                      options={[
                        { value: "", label: tr("cb_users_skip", undefined, "Skip these saves") },
                        ...here.map((h) => ({ value: String(h.uid), label: h.username })),
                      ]}
                    />
                  </li>
                ))}
              </ul>
            </Card>
          )}
          <Button
            disabled={busy || picked.size === 0}
            onClick={() => void start()}
            data-testid="cb-start-restore"
          >
            {tr("cb_restore_start", undefined, "Restore to this PS5")}
          </Button>
        </>
      )}
      {confirmDialog}
    </div>
  );
}

export default function ConsoleBackupScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const addr = host?.trim() ? mgmtAddr(host).split(":")[0] : "";
  const [view, setView] = useState<View>("backup");
  const { status, refresh, busy } = useBackupStatus();
  const tabs: { id: View; label: string }[] = [
    { id: "backup", label: tr("cb_tab_backup", undefined, "Back up") },
    { id: "restore", label: tr("cb_tab_restore", undefined, "Restore") },
    { id: "guide", label: tr("cb_tab_guide", undefined, "If your PS5 won't start") },
  ];
  const needsConsole = view !== "guide" && !addr;
  return (
    <div className="mx-auto w-full max-w-3xl space-y-4 p-4 md:p-6" data-testid="console-backup">
      <PageHeader
        icon={ArchiveRestore}
        title={tr("cb_title", undefined, "Backup & Restore")}
        description={tr(
          "cb_desc",
          undefined,
          "Keep what's worth keeping on this computer, and get it back after a reset.",
        )}
      />
      <div className="flex flex-wrap gap-2" role="tablist">
        {tabs.map((t) => (
          <Button
            key={t.id}
            size="sm"
            variant={view === t.id ? "primary" : "secondary"}
            role="tab"
            aria-selected={view === t.id}
            onClick={() => setView(t.id)}
            data-testid={`cb-tab-${t.id}`}
          >
            {t.label}
          </Button>
        ))}
      </div>
      {needsConsole && (
        <WarningCard title={tr("cb_no_console", undefined, "Connect to a PS5 first.")} />
      )}
      {status && busy && <Progress status={status} onCancel={() => void consoleBackupApi.cancel().then(refresh)} />}
      {status && !busy && view !== "guide" && status.host === addr && status.state !== "idle" && <Results status={status} />}
      {view === "backup" && addr && <BackupPane addr={addr} status={status} busy={busy} refresh={refresh} />}
      {view === "restore" && addr && <RestorePane addr={addr} busy={busy} refresh={refresh} />}
      {view === "guide" && <ResetGuide />}
    </div>
  );
}
