// Devices (8.0.0): the phones, computers and Docker engines that use the selected console, what
// each is doing now, and the owner's controls (rename, remove, block, approval for new devices,
// forgetting devices that stopped coming). Everything here lives on the console, so every device
// sees the same list.

import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router";
import {
  Ban,
  Container,
  HelpCircle,
  Monitor,
  MonitorSmartphone,
  Pencil,
  RefreshCw,
  Smartphone,
  Trash2,
  type LucideIcon,
} from "lucide-react";

import {
  Badge,
  Button,
  Callout,
  Card,
  PageHeader,
  Select,
  Toggle,
} from "../../components";
import { useConfirm, usePrompt } from "../../components/ConfirmDialog";
import {
  meshDevice,
  meshSettings,
  setMeshName,
  type MeshDevice,
} from "../../api/mesh";
import { useConnectionStore } from "../../state/connection";
import { useTr, type Translator } from "../../state/lang";
import {
  deviceName,
  loadMeshMe,
  meshFor,
  pollMesh,
  resetMeshCursor,
  useMeshStore,
} from "../../state/mesh";
import { humanizeCaught } from "../../lib/humanizeError";

export const KIND_ICONS: Record<string, LucideIcon> = {
  android: Smartphone,
  ios: Smartphone,
  docker: Container,
  desktop: Monitor,
};

function KindIcon({ kind }: { kind: string }) {
  const Icon = KIND_ICONS[kind] ?? HelpCircle;
  return <Icon size={22} />;
}

/** "just now", "5m ago", "3h ago", "12 days ago". */
export function agoText(tr: Translator, ms: number): string {
  const min = Math.floor(ms / 60_000);
  if (min < 1) return tr("activity_just_now", undefined, "just now");
  if (min < 60)
    return tr("activity_minutes_ago", { count: min }, `${min}m ago`);
  const h = Math.floor(min / 60);
  if (h < 48) return tr("activity_hours_ago", { count: h }, `${h}h ago`);
  const d = Math.floor(h / 24);
  return tr("mesh_days_ago", { count: d }, `${d} days ago`);
}

function kindLabel(tr: Translator, d: MeshDevice): string {
  switch (d.kind) {
    case "desktop":
      return tr("mesh_kind_desktop", undefined, "Computer");
    case "android":
      return tr("mesh_kind_android", undefined, "Android");
    case "ios":
      return tr("mesh_kind_ios", undefined, "iPhone or iPad");
    case "docker":
      return tr("mesh_kind_docker", undefined, "Docker");
    default:
      return tr("mesh_kind_unknown", undefined, "Device");
  }
}

function ActivityLines({ d }: { d: MeshDevice }) {
  const tr = useTr();
  const items = d.activity.filter((a) => a.kind !== "browsers");
  const browsers = d.activity.find((a) => a.kind === "browsers")?.count ?? 0;
  if (!items.length && !browsers) {
    return d.online ? (
      <p className="text-xs text-[var(--color-muted)]">
        {tr("mesh_idle", undefined, "Not doing anything right now")}
      </p>
    ) : null;
  }
  return (
    <ul className="space-y-0.5 text-xs">
      {items.map((a, i) => (
        <li key={i} className="truncate">
          <span className="font-medium">{a.kind}</span>
          {a.title ? ` · ${a.title}` : ""}
          {typeof a.pct === "number" ? ` · ${a.pct}%` : ""}
          {a.stage ? ` · ${a.stage}` : ""}
        </li>
      ))}
      {browsers > 0 && (
        <li className="text-[var(--color-muted)]">
          {tr(
            "mesh_browsers",
            { count: browsers },
            `${browsers} browser(s) using it`,
          )}
        </li>
      )}
    </ul>
  );
}

function DeviceCard({
  d,
  me,
  now,
  onRename,
  onRemove,
  onBlock,
  onUnblock,
}: {
  d: MeshDevice;
  me: boolean;
  now: number;
  onRename: () => void;
  onRemove: () => void;
  onBlock: () => void;
  onUnblock: () => void;
}) {
  const tr = useTr();
  const older = !d.app;
  return (
    <div className="flex flex-col gap-2 rounded-[var(--radius-card)] border border-[var(--color-border)] p-3 sm:flex-row sm:items-start">
      <div className="flex min-w-0 flex-1 items-start gap-3">
        <span
          className={`mt-0.5 shrink-0 ${d.online ? "text-[var(--color-good)]" : "text-[var(--color-muted)]"}`}
          aria-hidden
        >
          <KindIcon kind={d.kind} />
        </span>
        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="truncate font-medium">{deviceName(d)}</span>
            {me && (
              <Badge tone="accent">
                {tr("mesh_this_device", undefined, "This device")}
              </Badge>
            )}
            {d.blocked ? (
              <Badge tone="bad">
                {tr("mesh_blocked", undefined, "Blocked")}
              </Badge>
            ) : d.online ? (
              <Badge tone="good" dot>
                {tr("mesh_online", undefined, "Online")}
              </Badge>
            ) : (
              <Badge tone="neutral">
                {tr(
                  "mesh_last_seen",
                  { ago: agoText(tr, now - d.last) },
                  `Seen ${agoText(tr, now - d.last)}`,
                )}
              </Badge>
            )}
          </div>
          <p className="text-xs text-[var(--color-muted)]">
            {older
              ? tr(
                  "mesh_older_app",
                  undefined,
                  "An app from before 8.0.0: update it to see more about it here",
                )
              : [kindLabel(tr, d), d.os, d.app ? `v${d.app}` : "", d.ip]
                  .filter(Boolean)
                  .join(" · ")}
            {d.nick && d.name && d.nick !== d.name ? ` · ${d.name}` : ""}
          </p>
          {!d.blocked && <ActivityLines d={d} />}
        </div>
      </div>
      <div className="flex shrink-0 flex-wrap gap-1.5 sm:justify-end">
        <Button size="sm" variant="ghost" onClick={onRename}>
          <Pencil size={14} />
          {tr("mesh_rename", undefined, "Rename")}
        </Button>
        {!me && !d.online && !d.blocked && (
          <Button size="sm" variant="ghost" onClick={onRemove}>
            <Trash2 size={14} />
            {tr("mesh_remove", undefined, "Remove")}
          </Button>
        )}
        {!me &&
          (d.blocked ? (
            <Button size="sm" variant="secondary" onClick={onUnblock}>
              {tr("mesh_unblock", undefined, "Unblock")}
            </Button>
          ) : (
            <Button size="sm" variant="ghost" onClick={onBlock}>
              <Ban size={14} />
              {tr("mesh_block", undefined, "Block")}
            </Button>
          ))}
      </div>
    </div>
  );
}

export default function DevicesScreen() {
  const tr = useTr();
  const navigate = useNavigate();
  const host = useConnectionStore((s) => s.host);
  const up = useConnectionStore((s) => s.payloadStatus === "up");
  const mesh = useMeshStore((s) => meshFor(s, host));
  const me = useMeshStore((s) => s.me);
  const { confirm, dialog: confirmDialog } = useConfirm();
  const { prompt, dialog: promptDialog } = usePrompt();
  const [error, setError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(id);
  }, []);

  const refresh = async () => {
    resetMeshCursor(host);
    await Promise.all([pollMesh(host), loadMeshMe()]);
  };

  const run = async (f: () => Promise<unknown>) => {
    setError(null);
    try {
      await f();
      await refresh();
    } catch (e) {
      setError(humanizeCaught(e));
    }
  };

  const state = mesh.state;
  // The console's clock decides "seen 5 minutes ago": this device's may be off.
  const consoleNow = state ? state.now + (now - mesh.at) : now;
  const devices = useMemo(() => {
    const list = state?.devices ?? [];
    const rank = (d: MeshDevice) =>
      d.id === state?.you ? 0 : d.blocked ? 3 : d.online ? 1 : 2;
    return [...list].sort((a, b) => rank(a) - rank(b) || b.last - a.last);
  }, [state]);

  const rename = async (d: MeshDevice) => {
    const name = await prompt({
      title: tr("mesh_rename_title", undefined, "Rename this device"),
      message: tr(
        "mesh_rename_body",
        undefined,
        "Every device that uses this console sees the new name.",
      ),
      defaultValue: deviceName(d),
    });
    if (name === null) return;
    if (d.id === state?.you) await run(() => setMeshName(name));
    await run(() => meshDevice(host, "rename", d.id, name.trim()));
  };

  const remove = async (d: MeshDevice) => {
    const ok = await confirm({
      title: tr(
        "mesh_remove_title",
        { name: deviceName(d) },
        `Remove ${deviceName(d)}?`,
      ),
      message: tr(
        "mesh_remove_body",
        undefined,
        "It leaves this console's list. If it connects again it comes back as a new device. To keep it out, block it instead.",
      ),
      confirmLabel: tr("mesh_remove", undefined, "Remove"),
      destructive: true,
    });
    if (ok) await run(() => meshDevice(host, "remove", d.id));
  };

  const block = async (d: MeshDevice) => {
    const ok = await confirm({
      title: tr(
        "mesh_block_title",
        { name: deviceName(d) },
        `Block ${deviceName(d)}?`,
      ),
      message: tr(
        "mesh_block_body",
        undefined,
        "It is disconnected now and can't connect to this console again until you unblock it. Anything it was doing on the console stops.",
      ),
      confirmLabel: tr("mesh_block", undefined, "Block"),
      destructive: true,
    });
    if (ok) await run(() => meshDevice(host, "block", d.id));
  };

  const header = (
    <PageHeader
      icon={MonitorSmartphone}
      title={tr("mesh_devices", undefined, "Devices")}
      count={state ? state.devices.filter((d) => d.online).length : undefined}
      description={tr(
        "mesh_devices_subtitle",
        undefined,
        "The phones, computers and Docker setups that use this console. Any of them can connect; this list is kept on the console, so they all see the same one.",
      )}
      right={
        <Button variant="ghost" onClick={() => void refresh()}>
          <RefreshCw size={16} />
          {tr("refresh", undefined, "Refresh")}
        </Button>
      }
    />
  );

  if (!up || mesh.supported === false) {
    return (
      <div className="app-page">
        {header}
        <Callout
          tone="info"
          title={
            mesh.engineOld
              ? tr(
                  "mesh_old_engine",
                  undefined,
                  "This app's engine is older than 8.0.0",
                )
              : !up
                ? tr(
                    "mesh_need_helper",
                    undefined,
                    "Connect to a console to see its devices",
                  )
                : tr(
                    "mesh_old_helper",
                    undefined,
                    "Update the helper to see devices",
                  )
          }
        >
          <p className="mb-2">
            {mesh.engineOld
              ? tr(
                  "mesh_old_engine_body",
                  undefined,
                  "The engine this app talks to has no device list yet. Restart the app; if it uses an engine elsewhere (a Docker container), update that to 8.0.0.",
                )
              : !up
                ? tr(
                    "mesh_need_helper_body",
                    undefined,
                    "The list lives on the console, so its helper has to be running.",
                  )
                : tr(
                    "mesh_old_helper_body",
                    undefined,
                    "This console runs a helper from before 8.0.0. Send this app's helper from Connection, and every device that uses this console shows up here.",
                  )}
          </p>
          <Button
            size="sm"
            variant="secondary"
            onClick={() => navigate("/connection")}
          >
            {tr("connect", undefined, "Connection")}
          </Button>
        </Callout>
      </div>
    );
  }

  return (
    <div className="app-page space-y-4">
      {confirmDialog}
      {promptDialog}
      {header}
      {error && (
        <Callout
          tone="error"
          title={tr("mesh_failed", undefined, "That didn't work")}
        >
          {error}
        </Callout>
      )}
      {me?.kind === "docker" && me.docker_volume === false && (
        <Callout
          tone="warn"
          title={tr(
            "mesh_docker_volume_title",
            undefined,
            "This Docker setup forgets who it is",
          )}
        >
          {tr(
            "mesh_docker_volume_body",
            undefined,
            "/data isn't a folder or named volume the host keeps, so every update starts a new device and the consoles list it again. Map /data to a folder (-v /your/folder:/data) to keep it.",
          )}
        </Callout>
      )}

      <Card title={tr("mesh_list", undefined, "Devices on this console")}>
        {!state ? (
          <p className="text-sm text-[var(--color-muted)]">
            {tr("loading", undefined, "Loading…")}
          </p>
        ) : (
          <div className="space-y-2">
            {devices.map((d) => (
              <DeviceCard
                key={d.id}
                d={d}
                me={d.id === state.you}
                now={consoleNow}
                onRename={() => void rename(d)}
                onRemove={() => void remove(d)}
                onBlock={() => void block(d)}
                onUnblock={() =>
                  void run(() => meshDevice(host, "unblock", d.id))
                }
              />
            ))}
          </div>
        )}
      </Card>

      {state && (
        <Card title={tr("mesh_settings", undefined, "Who can connect")}>
          <div className="space-y-4">
            <Toggle
              checked={state.settings.require_approval}
              onChange={(v) =>
                void run(() => meshSettings(host, { require_approval: v }))
              }
              label={tr(
                "mesh_require_approval",
                undefined,
                "Ask before a new device can connect",
              )}
              hint={tr(
                "mesh_require_approval_hint",
                undefined,
                "Off: any device on your network connects. On: a new device shows a code on the TV to type in, or a connected device lets it in from Connection. Devices already listed keep working either way.",
              )}
            />
            <Select
              label={tr("mesh_prune", undefined, "Remove devices not seen for")}
              value={String(state.settings.prune_days)}
              onChange={(e) =>
                void run(() =>
                  meshSettings(host, { prune_days: Number(e.target.value) }),
                )
              }
              options={[
                {
                  value: "0",
                  label: tr("mesh_prune_never", undefined, "Never"),
                },
                {
                  value: "30",
                  label: tr("mesh_prune_days", { count: 30 }, "30 days"),
                },
                {
                  value: "90",
                  label: tr("mesh_prune_days", { count: 90 }, "90 days"),
                },
                {
                  value: "180",
                  label: tr("mesh_prune_days", { count: 180 }, "180 days"),
                },
              ]}
            />
          </div>
        </Card>
      )}

      {state && state.events.length > 0 && (
        <Card title={tr("mesh_recent", undefined, "Recently")}>
          <ul className="space-y-1 text-sm">
            {[...state.events]
              .reverse()
              .slice(0, 12)
              .map((e) => (
                <li key={e.seq} className="flex gap-2">
                  <span className="shrink-0 text-xs text-[var(--color-muted)] tabular-nums">
                    {agoText(tr, consoleNow - e.at)}
                  </span>
                  <span className="min-w-0">{e.text}</span>
                </li>
              ))}
          </ul>
        </Card>
      )}
    </div>
  );
}
