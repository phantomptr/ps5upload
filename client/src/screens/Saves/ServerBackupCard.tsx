import { useEffect, useState } from "react";
import { FolderOpen, Play, Server } from "lucide-react";

import { Button, Checkbox, Select } from "../../components";
import type { ServerBackupMode } from "../../api/saveBackup";
import { pickPath } from "../../lib/pickPath";
import { displayPath, parseRemotePath } from "../../lib/remotePath";
import { formatDate } from "../../lib/formatDate";
import { formatBytes } from "../../lib/format";
import { hostOf } from "../../lib/addr";
import { useConnectionsStore } from "../../state/connections";
import { useRosterStore } from "../../state/roster";
import { clampKeep } from "../../state/autoSaveBackup";
import { clampHours, isChosen, toggleChosen, useServerBackupStore } from "../../state/serverSaveBackup";
import { useTr } from "../../state/lang";
import { userHex } from "./saveUsers";

/** Backups to a network share (#592): where, when (every few hours or after a game is closed),
 *  which consoles and users, and how the last one went. The engine runs them and verifies each
 *  copy by reading it back from the server; restores are on each game's page. */
export default function ServerBackupCard({
  owners,
}: {
  /** The users with saves on the console shown (named where the console names them). */
  owners: Array<{ id: number; name: string | null }>;
}) {
  const tr = useTr();
  const { settings, status, nextAt, error, saving, refresh, set, runNow } = useServerBackupStore();
  const connections = useConnectionsStore((s) => s.connections);
  const loadConnections = useConnectionsStore((s) => s.load);
  const profiles = useRosterStore((s) => s.profiles);
  const [server, setServer] = useState("");

  useEffect(() => {
    void refresh();
    void loadConnections().catch(() => {});
  }, [refresh, loadConnections]);
  const running = status?.running ?? false;
  useEffect(() => {
    if (!running) return;
    const id = window.setInterval(() => void refresh(), 3000);
    return () => window.clearInterval(id);
  }, [running, refresh]);
  const targetConn = settings?.target ? parseRemotePath(settings.target)?.connectionId : undefined;
  useEffect(() => {
    if (targetConn) setServer(targetConn);
    else if (!server && connections.length > 0) setServer(connections[0].id);
  }, [targetConn, connections, server]);

  if (!settings) return null;
  const target = settings.target;
  const nameOf = (id: string) => connections.find((c) => c.id === id)?.name;
  const consoles = [...new Set(profiles.map((p) => hostOf(p.host)).filter(Boolean))].sort();
  const consoleName = (h: string) => profiles.find((p) => hostOf(p.host) === h)?.name || h;
  const userIds = owners.map((o) => o.id >>> 0);

  async function chooseFolder() {
    if (!server) return;
    const picked = await pickPath({
      mode: "folder",
      source: { connectionId: server },
      title: tr("saves_server_pick", undefined, "Folder for save backups on the server"),
    });
    if (picked) await set({ target: picked });
  }

  const modeLabel = (m: ServerBackupMode) =>
    m === "interval"
      ? tr("saves_server_mode_interval", undefined, "Every few hours")
      : m === "after_close"
        ? tr("saves_server_mode_closed", undefined, "After a game is closed")
        : tr("saves_server_mode_off", undefined, "Off");
  const trigger =
    status?.last_trigger === "closed"
      ? tr("saves_server_trigger_closed", undefined, "a game was closed")
      : status?.last_trigger === "manual"
        ? tr("saves_server_trigger_manual", undefined, "Back up now")
        : tr("saves_server_trigger_interval", undefined, "on schedule");

  return (
    <section
      className="mb-4 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4"
      data-testid="server-save-backup"
    >
      <header className="mb-1 flex items-center gap-2">
        <Server size={14} />
        <h3 className="flex-1 text-sm font-semibold">
          {tr("saves_server_title", undefined, "Backups to a network share")}
        </h3>
        <Button
          variant="ghost"
          size="sm"
          leftIcon={<Play size={12} />}
          onClick={() => void runNow()}
          disabled={!target || running}
        >
          {tr("saves_auto_run_now", undefined, "Back up now")}
        </Button>
      </header>
      <p className="mb-3 text-xs text-[var(--color-muted)]">
        {tr(
          "saves_server_note",
          undefined,
          "The engine copies saves that changed to a folder on a saved server (SMB, or any server under Connections), reads each copy back to check it is complete, and keeps a few versions. Restore one from the game's page.",
        )}
      </p>
      {connections.length === 0 ? (
        <p className="text-xs text-[var(--color-muted)]">
          {tr("saves_server_no_servers", undefined, "Add a server under Connections first.")}
        </p>
      ) : (
        <div className="grid gap-2 text-sm sm:grid-cols-[minmax(0,1fr)_auto] sm:items-end">
          <div className="min-w-0">
            <Select
              label={tr("saves_server_server", undefined, "Server")}
              value={server}
              onChange={(e) => setServer(e.target.value)}
              options={connections.map((c) => ({ value: c.id, label: c.name }))}
            />
            <div className="mt-1 break-all font-mono text-xs">
              {target
                ? displayPath(target, nameOf)
                : tr("saves_server_no_folder", undefined, "Choose a folder on the server to turn this on.")}
            </div>
          </div>
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<FolderOpen size={12} />}
            onClick={() => void chooseFolder()}
            loading={saving}
            disabled={!server}
          >
            {tr("saves_auto_folder", undefined, "Choose folder")}
          </Button>
        </div>
      )}
      {target && (
        <>
          <div className="mt-3 grid gap-2 border-t border-[var(--color-border)] pt-3 text-sm sm:grid-cols-3">
            <Select
              label={tr("saves_server_when", undefined, "When")}
              value={settings.mode}
              onChange={(e) => void set({ mode: e.target.value as ServerBackupMode })}
              options={(["off", "interval", "after_close"] as const).map((m) => ({ value: m, label: modeLabel(m) }))}
            />
            {settings.mode === "interval" && (
              <label className="text-xs text-[var(--color-muted)]">
                {tr("saves_server_hours", undefined, "Every (hours)")}
                <input
                  type="number"
                  min={1}
                  max={168}
                  defaultValue={settings.every_hours}
                  key={settings.every_hours}
                  onBlur={(e) => {
                    const h = clampHours(Number(e.target.value));
                    if (h !== settings.every_hours) void set({ every_hours: h });
                  }}
                  className="mt-1 block w-24 rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-2 py-1 text-sm text-[var(--color-text)]"
                />
              </label>
            )}
            <label className="text-xs text-[var(--color-muted)]">
              {tr("saves_auto_keep", undefined, "Versions to keep")}
              <input
                type="number"
                min={1}
                max={50}
                defaultValue={settings.keep}
                key={settings.keep}
                onBlur={(e) => {
                  const keep = clampKeep(Number(e.target.value));
                  if (keep !== settings.keep) void set({ keep });
                }}
                className="mt-1 block w-24 rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-2 py-1 text-sm text-[var(--color-text)]"
              />
            </label>
          </div>
          {(consoles.length > 1 || userIds.length > 1) && (
            <div className="mt-3 grid gap-3 border-t border-[var(--color-border)] pt-3 sm:grid-cols-2">
              {consoles.length > 1 && (
                <fieldset>
                  <legend className="mb-1 text-xs text-[var(--color-muted)]">
                    {tr("saves_server_consoles", undefined, "Consoles")}
                  </legend>
                  {consoles.map((h) => (
                    <Checkbox
                      key={h}
                      label={consoleName(h)}
                      checked={isChosen(settings.hosts, h)}
                      onChange={(on) => void set({ hosts: toggleChosen(settings.hosts, h, on, consoles) })}
                    />
                  ))}
                </fieldset>
              )}
              {userIds.length > 1 && (
                <fieldset>
                  <legend className="mb-1 text-xs text-[var(--color-muted)]">
                    {tr("saves_server_users", undefined, "Users")}
                  </legend>
                  {owners.map((o) => (
                    <Checkbox
                      key={o.id}
                      label={o.name ?? tr("saves_user_other", { id: userHex(o.id) }, "Other user ({id})")}
                      checked={isChosen(settings.users, o.id >>> 0)}
                      onChange={(on) => void set({ users: toggleChosen(settings.users, o.id >>> 0, on, userIds) })}
                    />
                  ))}
                </fieldset>
              )}
            </div>
          )}
        </>
      )}
      {error && <p className="mt-2 text-xs text-[var(--color-bad)]">{error}</p>}
      {target && (
        <div className="mt-2 space-y-0.5 text-xs text-[var(--color-muted)]" data-testid="server-save-backup-status">
          {running ? (
            <p>{tr("saves_auto_running", undefined, "Backing up now…")}</p>
          ) : status && status.last_at > 0 ? (
            <p>
              {tr(
                "saves_server_last",
                {
                  when: formatDate(new Date(status.last_at)),
                  why: trigger,
                  ok: status.verified,
                  failed: status.failed,
                },
                "Last backup ({when}, {why}): {ok} verified, {failed} failed.",
              )}
              {status.last_error ? ` ${status.last_error}` : ""}
            </p>
          ) : (
            <p>{tr("saves_server_none_yet", undefined, "No backup yet.")}</p>
          )}
          {!running && status?.last_verified && (
            <p className="break-all" title={status.last_verified.sha256}>
              {tr(
                "saves_server_verified",
                {
                  file: status.last_verified.file,
                  size: formatBytes(status.last_verified.size),
                  n: status.last_verified.entries,
                  sha: status.last_verified.sha256.slice(0, 12),
                },
                "Newest verified: {file} ({size}, {n} files, SHA-256 {sha}…)",
              )}
            </p>
          )}
          {settings.mode === "interval" && nextAt !== null && nextAt > 0 && (
            <p>{tr("saves_server_next", { when: formatDate(new Date(nextAt)) }, "Next: {when}")}</p>
          )}
        </div>
      )}
    </section>
  );
}
