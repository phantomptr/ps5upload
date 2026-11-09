import type { ReactNode } from "react";
import { Gamepad2, Laptop, Loader2, Plus, RefreshCw, Smartphone } from "lucide-react";
import { Button } from "../../components";
import { useTr } from "../../state/lang";
import { formatAgo } from "../../lib/formatDate";
import { consoleAddr } from "../../lib/addr";
import type { TailnetPeer } from "../../api/tailnet";

/** A peer as the device lists show it (Settings › Tailscale, and the PS5's "Who can reach"). */
export interface DeviceRowData {
  name: string;
  ip: string;
  online: boolean;
  path: string;
  latency_ms: number;
  /** When it was last online (unix ms), if the list knows it. */
  last_seen_ms?: number;
}

export type DeviceKind = "ps5" | "phone" | "computer";

const PHONE = /(iphone|ipad|pixel|android|galaxy|phone|oneplus|xiaomi|redmi|huawei|^sm-)/i;

/** What kind of device a tailnet name is: a PS5 (a console PS5Upload knows at that address, or a
 *  "-ps5u" name that names a PS5: every PS5Upload device gets that suffix), a phone, a computer. */
export function deviceKind(name: string, isConsole = false): DeviceKind {
  const label = name.split(".")[0].toLowerCase();
  if (isConsole) return "ps5";
  const base = label.replace(/-ps5u(-\d+)?$/, "");
  if (base !== label && /ps5|playstation/.test(base)) return "ps5";
  if (PHONE.test(base)) return "phone";
  return "computer";
}

/** Where a peer stands, in one of four states: never "idle". Connected means PS5Upload has a
 *  path to it in use (direct or through Tailscale's relays). */
export function deviceStatus(
  p: Pick<DeviceRowData, "online" | "path" | "latency_ms">,
): { kind: "direct" | "relayed" | "online" | "offline"; ms: number | null } {
  if (p.path === "direct") return { kind: "direct", ms: p.latency_ms >= 0 ? Math.round(p.latency_ms) : null };
  if (p.path === "relayed") return { kind: "relayed", ms: null };
  return { kind: p.online ? "online" : "offline", ms: null };
}

/** One listed device, with how it sorts and what it is called. */
export interface ListedDevice extends DeviceRowData {
  kind: DeviceKind;
  self: boolean;
  /** The console's name in this app's list, for a PS5 it knows. */
  rosterName: string | null;
  /** The roster entry id, when the PS5 is on the list. */
  rosterId: string | null;
}

/**
 * The devices to show, in order: PS5s first (online before offline), then this device, then the
 * other online devices, then the offline ones. A PS5 takes its name from this app's list when the
 * console is on it (by its home address remembered with that tailnet address, or by the address).
 */
export function orderDevices(
  peers: readonly DeviceRowData[],
  opts: {
    self?: { name: string; ip: string } | null;
    /** The engine's known consoles: home address and tailnet address. */
    consoles?: readonly { host: string; ip: string }[];
    profiles?: readonly { id: string; name: string; host: string }[];
  } = {},
): ListedDevice[] {
  const consoles = opts.consoles ?? [];
  const profiles = opts.profiles ?? [];
  const profileFor = (ip: string) => {
    const hosts = new Set([ip, ...consoles.filter((c) => c.ip === ip).map((c) => c.host)].map(consoleAddr));
    return profiles.find((p) => hosts.has(consoleAddr(p.host))) ?? null;
  };
  const rows: ListedDevice[] = peers
    .filter((p) => p.ip && p.ip !== opts.self?.ip)
    .map((p) => {
      const profile = profileFor(p.ip);
      const isConsole = !!profile || consoles.some((c) => c.ip === p.ip);
      return {
        ...p,
        kind: deviceKind(p.name, isConsole),
        self: false,
        rosterName: profile?.name ?? null,
        rosterId: profile?.id ?? null,
      };
    });
  if (opts.self?.ip) {
    rows.push({
      name: opts.self.name,
      ip: opts.self.ip,
      online: true,
      path: "",
      latency_ms: -1,
      kind: deviceKind(opts.self.name),
      self: true,
      rosterName: null,
      rosterId: null,
    });
  }
  const rank = (d: ListedDevice) => (d.kind === "ps5" ? (d.online ? 0 : 1) : d.self ? 2 : d.online ? 3 : 4);
  return rows
    .map((d, i) => ({ d, i }))
    .sort((a, b) => rank(a.d) - rank(b.d) || a.i - b.i)
    .map((x) => x.d);
}

/** The engine's peers as rows (`last_seen_ms` when a list carries it). */
export function peerRows(peers: readonly TailnetPeer[]): DeviceRowData[] {
  return peers.map((p) => ({
    name: p.name,
    ip: p.ip,
    online: p.online,
    path: p.path,
    latency_ms: p.latency_ms,
    last_seen_ms: (p as { last_seen_ms?: number }).last_seen_ms,
  }));
}

function KindIcon({ kind }: { kind: DeviceKind }) {
  const Icon = kind === "ps5" ? Gamepad2 : kind === "phone" ? Smartphone : Laptop;
  return <Icon size={13} className="shrink-0 text-[var(--color-muted)]" aria-hidden="true" />;
}

/** One device: its kind, its name (a PS5 by its name in the app: "Pro · yunpengs-ps5pro-ps5u"),
 *  and a dot with its state. `lead` goes first (the PS5 panel's tick); `action` last. */
export function DeviceRow({ d, lead, action }: { d: ListedDevice; lead?: ReactNode; action?: ReactNode }) {
  const tr = useTr();
  const st = deviceStatus(d);
  const label = d.name.split(".")[0] || d.ip;
  const dot =
    st.kind === "offline"
      ? "bg-[var(--color-muted)]"
      : st.kind === "online"
        ? "bg-[var(--color-good)]"
        : "bg-[var(--color-accent-bright)]";
  const text = d.self
    ? tr("tn_dev_this_device", undefined, "This device")
    : st.kind === "direct"
      ? st.ms !== null
        ? tr("tn_dev_connected_direct", { ms: st.ms }, "Connected · direct {ms} ms")
        : tr("tn_dev_connected_direct_nms", undefined, "Connected · direct")
      : st.kind === "relayed"
        ? tr("tn_dev_connected_relayed", undefined, "Connected · relayed")
        : st.kind === "online"
          ? tr("tn_dev_online", undefined, "Online")
          : d.last_seen_ms
            ? tr("tn_dev_offline_seen", { when: formatAgo(d.last_seen_ms) }, "Offline · last seen {when}")
            : tr("tn_dev_offline", undefined, "Offline");
  return (
    <li className="flex items-center gap-2 py-1 text-xs" data-testid="tn-device" data-kind={d.kind} data-state={d.self ? "self" : st.kind}>
      {lead}
      <KindIcon kind={d.kind} />
      <span className="min-w-0 flex-1 truncate">
        {d.rosterName ? (
          <>
            <span className="font-medium">{d.rosterName}</span>
            <span className="text-[var(--color-muted)]"> · {label}</span>
          </>
        ) : (
          label
        )}
      </span>
      <span className="flex shrink-0 items-center gap-1.5 text-[var(--color-muted)]">
        <span className={`inline-block h-2 w-2 rounded-full ${dot}`} aria-hidden="true" />
        {text}
      </span>
      {action}
    </li>
  );
}

/**
 * "Devices on your Tailscale network (N · M online)", with Refresh, as a list of {@link DeviceRow}.
 * A PS5 that is not on this app's list gets Add (`onAdd`, the Tailscale add flow).
 */
export function DeviceList({
  devices,
  refreshing,
  onRefresh,
  onAdd,
  adding,
}: {
  devices: ListedDevice[];
  refreshing: boolean;
  onRefresh: () => void;
  onAdd?: (d: ListedDevice) => void;
  /** The address being added. */
  adding?: string | null;
}) {
  const tr = useTr();
  const others = devices.filter((d) => !d.self);
  const online = others.filter((d) => d.online).length;
  return (
    <div className="flex flex-col gap-1" data-testid="app-tailnet-peers">
      <div className="flex items-center gap-2 text-xs font-semibold">
        <span className="flex-1" data-testid="tn-devices-header">
          {tr(
            "tn_dev_header",
            { n: others.length, m: online },
            "Devices on your Tailscale network ({n} · {m} online)",
          )}
        </span>
        <Button
          size="sm"
          variant="ghost"
          leftIcon={refreshing ? <Loader2 size={12} className="animate-spin" /> : <RefreshCw size={12} />}
          disabled={refreshing}
          onClick={onRefresh}
          data-testid="tn-devices-refresh"
        >
          {tr("tn_dev_refresh", undefined, "Refresh")}
        </Button>
      </div>
      {others.length === 0 ? (
        <p className="text-xs text-[var(--color-muted)]">
          {tr("tn_no_peers", undefined, "No other devices on your Tailscale network yet.")}
        </p>
      ) : (
        <ul className="flex flex-col divide-y divide-[var(--color-border)]">
          {devices.map((d) => (
            <DeviceRow
              key={d.ip}
              d={d}
              action={
                onAdd && d.kind === "ps5" && !d.rosterId && !d.self ? (
                  <Button
                    size="sm"
                    variant="secondary"
                    leftIcon={<Plus size={11} />}
                    loading={adding === d.ip}
                    disabled={!!adding}
                    onClick={() => onAdd(d)}
                    data-testid="tn-device-add"
                  >
                    {tr("tn_found_add", undefined, "Add")}
                  </Button>
                ) : null
              }
            />
          ))}
        </ul>
      )}
    </div>
  );
}

/** Re-read every this often while the screen is visible (and at once on focus or when shown). */
export const DEVICES_READ_MS = 5_000;
