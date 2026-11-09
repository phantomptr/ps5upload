// The Consoles page's logic, apart from React so it tests on its own: what one console's card
// says at a glance, which setup step it is on, the Add sheet's merged scan (one row per console,
// however many ways it was found), invite links, and how a trusted device was let in.

import type { HostRuntime } from "../../../state/connection";
import type { ConsoleOnTailnet } from "../../../state/useTailnet";
import type { DiscoveredHost } from "../../../api/ps5";
import type { FoundConsole } from "../../../api/tailnet";
import type { MeshDevice } from "../../../api/mesh";
import { compareVersions } from "../../../lib/semver";
import { parsePS5Firmware } from "../../../lib/ps5Firmware";
import { consoleAddr, hostOf } from "../../../lib/addr";

// ---- one console's card ----

/** On and answering, in rest mode, not answering, or not probed yet. */
export type CardPower = "online" | "rest" | "offline" | "unknown";

/** How the app reaches the console right now. */
export type CardPath =
  | { kind: "home"; addr: string }
  | { kind: "tailscale"; addr: string; via: "direct" | "relayed" | "system" }
  | { kind: "none" };

/** The helper as this app sees it: the same version as the app, older (update), newer, sending,
 *  waiting for this device to be let in, turned off by the user, not running, or not known yet. */
export type HelperState =
  | "match"
  | "update"
  | "newer"
  | "sending"
  | "pairing"
  | "stopped"
  | "missing"
  | "unknown";

/** Where the setup checklist stands: reach the console, run the helper, or nothing to do. */
export type SetupStep = "address" | "helper" | "ready";

export interface CardStatus {
  power: CardPower;
  path: CardPath;
  helper: HelperState;
  helperVersion: string | null;
  firmware: string | null;
  /** Kernel read/write for installs; null when the helper has not said. */
  kernelRw: boolean | null;
  step: SetupStep;
  /** Ready for uploads: the helper answers and this device is let in. */
  ready: boolean;
}

export interface CardInput {
  host: string;
  runtime: HostRuntime;
  /** The console on Tailscale (useTailnet), when it is. */
  tailnet: ConsoleOnTailnet | null;
  appVersion: string | null;
  /** The user turned the helper off (Stop helper). */
  stopped: boolean;
  /** A helper send to this console is running. */
  sending: boolean;
  /** The last firmware seen for it (the roster keeps it while it is away). */
  lastSeenKernel?: string | null;
}

export function cardStatus(i: CardInput): CardStatus {
  const rt = i.runtime;
  const host = i.host.trim();
  const up = rt.payloadStatus === "up";
  const pairing = rt.session === "needs_pairing";
  const power: CardPower = !host
    ? "unknown"
    : rt.power === "rest"
      ? "rest"
      : up || pairing || rt.power === "awake"
        ? "online"
        : rt.payloadStatus === "down" || rt.power === "unreachable"
          ? "offline"
          : "unknown";
  const path: CardPath =
    power !== "online"
      ? { kind: "none" }
      : i.tailnet && i.tailnet.via !== "lan"
        ? { kind: "tailscale", addr: i.tailnet.ip, via: i.tailnet.via }
        : { kind: "home", addr: hostOf(host) };
  let helper: HelperState;
  if (i.sending) helper = "sending";
  else if (pairing) helper = "pairing";
  else if (up) {
    const cmp = (rt.payloadVersion && i.appVersion ? compareVersions(rt.payloadVersion, i.appVersion) : 0) ?? 0;
    helper = cmp < 0 ? "update" : cmp > 0 ? "newer" : "match";
  } else if (i.stopped) helper = "stopped";
  else if (rt.payloadStatus === "down") helper = "missing";
  else helper = "unknown";
  const ready = up && !pairing;
  const step: SetupStep = ready ? "ready" : !host || power === "offline" ? "address" : "helper";
  return {
    power,
    path,
    helper,
    helperVersion: rt.payloadVersion,
    firmware: parsePS5Firmware(rt.ps5Kernel ?? i.lastSeenKernel ?? null),
    kernelRw: rt.ucredElevated,
    step,
    ready,
  };
}

/** Whether the setup checklist shows on the card: only when something is not ready. A console in
 *  rest mode is not "not ready" in a way setup fixes, and one not probed yet is still loading. */
export function showChecklist(s: CardStatus): boolean {
  return !s.ready && s.power !== "rest" && s.power !== "unknown";
}

// ---- addresses ----

/** Tailscale's addresses: 100.64.0.0/10. */
export function isTailnetIp(addr: string): boolean {
  const m = /^100\.(\d{1,3})\./.exec(hostOf(addr));
  if (!m) return false;
  const b = Number(m[1]);
  return b >= 64 && b <= 127;
}

// ---- how a trusted device was let in ----

export type DeviceVia = "network" | "tailscale" | "invite" | "paired";

/** How the console let a device in: on the home network, over Tailscale, by an invite someone
 *  allowed, or paired another way (a code on the TV, or the app that sent the helper). A helper
 *  from before 12.2 does not say: the address tells Tailscale from the home network. */
export function deviceVia(d: Pick<MeshDevice, "via" | "ip">): DeviceVia {
  if (d.via === "approved") return "invite";
  if (d.via === "tailscale") return "tailscale";
  if (d.via === "network") return "network";
  // A 12.2 helper says "" for a device it did not let in itself (paired with a code, or the app
  // that sent the helper); an older one says nothing, and most of those came in on the network.
  if (d.via === "") return "paired";
  return d.ip && isTailnetIp(d.ip) ? "tailscale" : "network";
}

// ---- invites ----

/** What an invite link carries: enough to reach the console and know it is the right one.
 *  Never a key that lets anyone in: the console still asks before the new device connects. */
export interface Invite {
  /** The console's name on the inviting device. */
  name: string;
  /** Its home address, when known. */
  home: string | null;
  /** Its Tailscale address, when it is on Tailscale. */
  tailnet: string | null;
  /** Its AVA1 key (hex): the console's identity at every address. */
  key: string | null;
}

/** A ps5upload://console link (a QR code, or pasted), which this app opens at Consoles. */
export function inviteLink(inv: Invite): string {
  const q = new URLSearchParams();
  q.set("n", inv.name);
  if (inv.home) q.set("h", inv.home);
  if (inv.tailnet) q.set("t", inv.tailnet);
  if (inv.key) q.set("k", inv.key);
  return `ps5upload://console?${q}`;
}

const ADDR = /^[A-Za-z0-9.:[\]-]{1,253}$/;

/** The invite in a link, or null for anything else. Pasted text may carry spaces around it. */
export function parseInvite(text: string): Invite | null {
  let u: URL;
  try {
    u = new URL(text.trim());
  } catch {
    return null;
  }
  if (u.protocol !== "ps5upload:") return null;
  const what = (u.host || u.pathname.replace(/^\/+/, "")).replace(/\/+$/, "").toLowerCase();
  if (what !== "console") return null;
  const addr = (v: string | null) => {
    const s = (v ?? "").trim();
    return s && ADDR.test(s) ? s : null;
  };
  const key = (u.searchParams.get("k") ?? "").trim().toLowerCase();
  const inv: Invite = {
    name: (u.searchParams.get("n") ?? "").trim().slice(0, 64),
    home: addr(u.searchParams.get("h")),
    tailnet: addr(u.searchParams.get("t")),
    key: /^[0-9a-f]{64}$/.test(key) ? key : null,
  };
  return inv.home || inv.tailnet ? inv : null;
}

// ---- the Add sheet's merged scan ----

export type FoundSource = "home" | "tailscale" | "invite";

/** One console in the Add sheet, however many ways it was found. */
export interface FoundRow {
  /** Stable React key: the console's identity when known, else its first address. */
  id: string;
  /** The console's identity (AVA1 key, hex), when a scan or an invite gave it. */
  key: string | null;
  name: string;
  /** Its home address, when found there (or known for it). */
  home: string | null;
  /** Its Tailscale address, when found there. */
  tailnet: string | null;
  sources: FoundSource[];
  /** The home scan's confidence that this is a PS5 (0-100); 100 for Tailscale and invites, which
   *  only name PS5Upload consoles. */
  confidence: number;
  /** Its helper answers (the home scan saw port 9120 open, or Tailscale reached it). */
  helperRunning: boolean;
  /** Already on this app's list: that entry's id. */
  listedId: string | null;
}

interface Part {
  key: string | null;
  name: string;
  addrs: string[];
  home: string | null;
  tailnet: string | null;
  source: FoundSource;
  confidence: number;
  helper: boolean;
  listedId: string | null;
}

const norm = (a: string) => consoleAddr(a).toLowerCase();

/**
 * One row per console. Two finds are the same console when they share its identity (the key from
 * a Tailscale probe, an invite, or the home address's own answer) or any address (a Tailscale
 * entry knows the home address it was met at). Rows keep every way they were found, in the order
 * home, Tailscale, invite; already-listed consoles come last.
 */
export function mergeFound(
  lan: readonly DiscoveredHost[],
  tailnet: readonly FoundConsole[],
  invite: Invite | null,
  /** The identity read for a home-scan address (consoleIdentity), keyed by its address. */
  lanKeys: Readonly<Record<string, string>>,
  profiles: readonly { id: string; host: string }[],
): FoundRow[] {
  const parts: Part[] = [];
  for (const h of lan) {
    const addrs = [h.ip, ...(h.all_ips ?? [])].filter(Boolean);
    parts.push({
      key: lanKeys[h.ip] ?? null,
      name: h.hostname ?? "",
      addrs,
      home: h.ip,
      tailnet: null,
      source: "home",
      confidence: h.confidence,
      helper: h.payload_port_open,
      listedId: null,
    });
  }
  for (const c of tailnet) {
    const addrs = [c.tailnet_ip, c.lan_host, ...(c.hosts ?? [])].filter((a): a is string => !!a);
    parts.push({
      key: c.id,
      name: c.name,
      addrs,
      home: c.lan_host,
      tailnet: c.tailnet_ip,
      source: "tailscale",
      confidence: 100,
      helper: c.reachable,
      listedId: c.listed?.id ?? null,
    });
  }
  if (invite) {
    parts.push({
      key: invite.key,
      name: invite.name,
      addrs: [invite.home, invite.tailnet].filter((a): a is string => !!a),
      home: invite.home,
      tailnet: invite.tailnet,
      source: "invite",
      confidence: 100,
      helper: false,
      listedId: null,
    });
  }

  // Union-find over the parts: same key or a shared address joins two.
  const parent = parts.map((_, i) => i);
  const find = (i: number): number => (parent[i] === i ? i : (parent[i] = find(parent[i])));
  const join = (a: number, b: number) => {
    const ra = find(a);
    const rb = find(b);
    if (ra !== rb) parent[Math.max(ra, rb)] = Math.min(ra, rb);
  };
  const byKey = new Map<string, number>();
  const byAddr = new Map<string, number>();
  parts.forEach((p, i) => {
    if (p.key) {
      const k = p.key.toLowerCase();
      const seen = byKey.get(k);
      if (seen === undefined) byKey.set(k, i);
      else join(seen, i);
    }
    for (const a of p.addrs) {
      const n = norm(a);
      const seen = byAddr.get(n);
      if (seen === undefined) byAddr.set(n, i);
      else join(seen, i);
    }
  });
  // Two different keys never merge, even at one address (a DHCP change gave the address to
  // another console): split those groups back by key.
  const groups = new Map<string, Part[]>();
  parts.forEach((p, i) => {
    const root = find(i);
    const keysInGroup = new Set(
      parts.filter((_, j) => find(j) === root && parts[j].key).map((q) => q.key!.toLowerCase()),
    );
    const id = keysInGroup.size > 1 && p.key ? `${root}:${p.key.toLowerCase()}` : String(root);
    groups.set(id, [...(groups.get(id) ?? []), p]);
  });

  const order: FoundSource[] = ["home", "tailscale", "invite"];
  const rows: FoundRow[] = [];
  for (const g of groups.values()) {
    const key = g.find((p) => p.key)?.key?.toLowerCase() ?? null;
    const home = g.find((p) => p.home)?.home ?? null;
    const tn = g.find((p) => p.tailnet)?.tailnet ?? null;
    // A name a person gave (Tailscale, an invite) beats an mDNS hostname.
    const name =
      g.find((p) => p.source === "invite" && p.name)?.name ??
      g.find((p) => p.source === "tailscale" && p.name)?.name ??
      g.find((p) => p.name)?.name ??
      "";
    const addrs = new Set(g.flatMap((p) => p.addrs.map(norm)));
    const listed =
      g.find((p) => p.listedId && profiles.some((x) => x.id === p.listedId))?.listedId ??
      profiles.find((x) => addrs.has(norm(x.host)))?.id ??
      null;
    rows.push({
      id: key ?? norm(home ?? tn ?? g[0].addrs[0] ?? String(rows.length)),
      key,
      name,
      home,
      tailnet: tn,
      sources: order.filter((s) => g.some((p) => p.source === s)),
      confidence: Math.max(...g.map((p) => p.confidence)),
      helperRunning: g.some((p) => p.helper),
      listedId: listed,
    });
  }
  // New consoles first (most likely a PS5 first), the ones on the list already after.
  return rows.sort(
    (a, b) =>
      Number(!!a.listedId) - Number(!!b.listedId) ||
      b.confidence - a.confidence ||
      (a.home ?? a.tailnet ?? "").localeCompare(b.home ?? b.tailnet ?? ""),
  );
}

/** The address to add a found console at: home first (always tried first), Tailscale otherwise. */
export function addAddress(r: Pick<FoundRow, "home" | "tailnet">): string {
  return r.home ?? r.tailnet ?? "";
}

/** Where Add puts a found console. Its home address when this device found it there, or when
 *  Tailscale is no way in (no Tailscale address, or this device is not on Tailscale). Otherwise its
 *  Tailscale address: an invite opened away from home names a home address this device can't
 *  reach, and the engine adds a console by its Tailscale address at the home one it knows. */
export function addTarget(r: Pick<FoundRow, "home" | "tailnet" | "sources">, onTailscale: boolean): string {
  if (r.home && (r.sources.includes("home") || !r.tailnet || !onTailscale)) return r.home;
  return r.tailnet ?? r.home ?? "";
}
