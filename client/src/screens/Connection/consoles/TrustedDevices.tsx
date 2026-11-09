// Who may use one console: the devices on its list (the console's own mesh list, the same one
// Connection › Devices and the console's page show), how each was let in, Revoke and Block; the
// devices asking to connect now (Allow / Decline); and the console's "ask first" switch.

import { useState } from "react";
import { Ban, Container, HelpCircle, Monitor, ShieldCheck, Smartphone, Trash2, type LucideIcon } from "lucide-react";

import { Badge, Button, Toggle } from "../../../components";
import { useConfirm } from "../../../components/ConfirmDialog";
import { meshDevice, meshSettings, type MeshAsk, type MeshDevice, type MeshState } from "../../../api/mesh";
import { deviceName, pollMesh, resetMeshCursor } from "../../../state/mesh";
import { humanizeCaught } from "../../../lib/humanizeError";
import { useTr, type Translator } from "../../../state/lang";
import { deviceVia, type DeviceVia } from "./model";

const KIND: Record<string, LucideIcon> = { android: Smartphone, ios: Smartphone, docker: Container, desktop: Monitor };

export function viaText(tr: Translator, v: DeviceVia): string {
  switch (v) {
    case "network":
      return tr("cs_trust_via_network", undefined, "Let in on the home network");
    case "tailscale":
      return tr("cs_trust_via_tailscale", undefined, "Let in over Tailscale");
    case "invite":
      return tr("cs_trust_via_invite", undefined, "Invited, and approved");
    case "paired":
      return tr("cs_trust_via_paired", undefined, "Paired with a code, or sent the helper");
  }
}

/** "Pixel wants to use Living-room PS5: Allow / Decline", one row per device asking. */
export function AskRows({
  asks,
  consoleName,
  busy,
  onAnswer,
}: {
  asks: readonly MeshAsk[];
  consoleName: string;
  busy: boolean;
  onAnswer: (op: "allow" | "decline", id: string) => void;
}) {
  const tr = useTr();
  if (asks.length === 0) return null;
  return (
    <ul className="flex flex-col gap-2" data-testid="consoles-asks">
      {asks.map((a) => (
        <li
          key={a.id}
          className="flex flex-wrap items-center gap-2 rounded-[var(--radius-field)] border border-[color-mix(in_srgb,var(--color-accent)_40%,transparent)] bg-[var(--color-accent-soft)] px-3 py-2 text-sm"
          data-testid="consoles-ask"
        >
          <ShieldCheck size={16} className="shrink-0 text-[var(--color-accent-bright)]" aria-hidden />
          <span className="min-w-[10rem] flex-1">
            {tr(
              "cs_ask_line",
              { device: a.name || a.ip || "A device", console: consoleName },
              "{device} wants to use {console}",
            )}
            {a.ip ? <span className="ms-1 font-mono text-xs text-[var(--color-muted)]">{a.ip}</span> : null}
          </span>
          <Button size="sm" variant="primary" disabled={busy} onClick={() => onAnswer("allow", a.id)} data-testid="consoles-ask-allow">
            {tr("cs_ask_allow", undefined, "Allow")}
          </Button>
          <Button size="sm" variant="ghost" disabled={busy} onClick={() => onAnswer("decline", a.id)} data-testid="consoles-ask-decline">
            {tr("cs_ask_decline", undefined, "Decline")}
          </Button>
        </li>
      ))}
    </ul>
  );
}

/** One trusted device: its name and kind, how it was let in, online, and Revoke / Block. */
function DeviceLine({
  d,
  me,
  busy,
  onRevoke,
  onBlock,
}: {
  d: MeshDevice;
  me: boolean;
  busy: boolean;
  onRevoke: () => void;
  onBlock: () => void;
}) {
  const tr = useTr();
  const Icon = KIND[d.kind] ?? HelpCircle;
  return (
    <li className="flex flex-wrap items-center gap-x-3 gap-y-1 py-2" data-testid="consoles-trusted-device">
      <Icon
        size={18}
        className={`shrink-0 ${d.online ? "text-[var(--color-good)]" : "text-[var(--color-muted)]"}`}
        aria-hidden
      />
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-1.5 text-sm">
          <span className="truncate font-medium">{deviceName(d)}</span>
          {me ? <Badge tone="accent">{tr("mesh_this_device", undefined, "This device")}</Badge> : null}
          {d.online ? (
            <Badge tone="good" dot>
              {tr("mesh_online", undefined, "Online")}
            </Badge>
          ) : null}
        </div>
        <div className="break-words text-xs text-[var(--color-muted)]">
          {viaText(tr, deviceVia(d))}
          {d.ip ? ` · ${d.ip}` : ""}
        </div>
      </div>
      {!me ? (
        <div className="flex shrink-0 gap-1">
          <Button size="sm" variant="ghost" leftIcon={<Trash2 size={13} />} disabled={busy} onClick={onRevoke}>
            {tr("cs_trust_revoke", undefined, "Revoke")}
          </Button>
          <Button size="sm" variant="ghost" leftIcon={<Ban size={13} />} disabled={busy} onClick={onBlock}>
            {tr("mesh_block", undefined, "Block")}
          </Button>
        </div>
      ) : null}
    </li>
  );
}

/** The view, from a console's mesh picture (render-tested on its own). */
export function TrustedDevicesView({
  state,
  consoleName,
  busy,
  error,
  onAnswer,
  onRevoke,
  onBlock,
  onAskFirst,
}: {
  state: MeshState | null;
  consoleName: string;
  busy: boolean;
  error: string | null;
  onAnswer: (op: "allow" | "decline", id: string) => void;
  onRevoke: (d: MeshDevice) => void;
  onBlock: (d: MeshDevice) => void;
  onAskFirst: (on: boolean) => void;
}) {
  const tr = useTr();
  if (!state) {
    return (
      <p className="text-xs text-[var(--color-muted)]">
        {tr("cs_trust_need_helper", undefined, "The list is kept on the PS5: it shows while its helper runs.")}
      </p>
    );
  }
  const list = state.devices.filter((d) => !d.blocked);
  const rank = (d: MeshDevice) => (d.id === state.you ? 0 : d.online ? 1 : 2);
  list.sort((a, b) => rank(a) - rank(b) || b.last - a.last);
  return (
    <div className="flex flex-col gap-3" data-testid="consoles-trusted">
      <AskRows asks={state.asks ?? []} consoleName={consoleName} busy={busy} onAnswer={onAnswer} />
      <ul className="divide-y divide-[var(--color-border)]">
        {list.map((d) => (
          <DeviceLine
            key={d.id}
            d={d}
            me={d.id === state.you}
            busy={busy}
            onRevoke={() => onRevoke(d)}
            onBlock={() => onBlock(d)}
          />
        ))}
      </ul>
      <Toggle
        checked={state.settings.require_approval}
        disabled={busy}
        onChange={onAskFirst}
        label={tr("mesh_require_approval", undefined, "Ask before a new device can connect")}
        hint={tr(
          "cs_trust_ask_hint",
          undefined,
          "On: a new device waits until you allow it here, on any device that uses this PS5, or on the PS5's own page. Off: any device on your home network gets in. Devices already listed keep working either way.",
        )}
      />
      {error ? (
        <p role="alert" className="text-xs text-[var(--color-bad)]">
          {error}
        </p>
      ) : null}
    </div>
  );
}

/** The live list for `host`, with its actions. */
export function TrustedDevices({ host, consoleName, state }: { host: string; consoleName: string; state: MeshState | null }) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await f();
      resetMeshCursor(host);
      await pollMesh(host);
    } catch (e) {
      setError(humanizeCaught(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      {dialog}
      <TrustedDevicesView
        state={state}
        consoleName={consoleName}
        busy={busy}
        error={error}
        onAnswer={(op, id) => void run(() => meshDevice(host, op, id))}
        onAskFirst={(on) => void run(() => meshSettings(host, { require_approval: on }))}
        onRevoke={async (d) => {
          const ok = await confirm({
            title: tr("cs_trust_revoke_title", { name: deviceName(d) }, "Revoke {name}?"),
            message: tr(
              "cs_trust_revoke_body",
              undefined,
              "It leaves this PS5's list and its trust goes with it. With Ask before a new device can connect on, it has to be allowed again; with it off, it can come back on the home network. To keep it out, block it.",
            ),
            confirmLabel: tr("cs_trust_revoke", undefined, "Revoke"),
            destructive: true,
          });
          if (ok) await run(() => meshDevice(host, "remove", d.id));
        }}
        onBlock={async (d) => {
          const ok = await confirm({
            title: tr("mesh_block_title", { name: deviceName(d) }, `Block ${deviceName(d)}?`),
            message: tr(
              "mesh_block_body",
              undefined,
              "It is disconnected now and can't connect to this console again until you unblock it. Anything it was doing on the console stops.",
            ),
            confirmLabel: tr("mesh_block", undefined, "Block"),
            destructive: true,
          });
          if (ok) await run(() => meshDevice(host, "block", d.id));
        }}
      />
    </>
  );
}
