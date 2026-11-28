// A USB Ethernet adapter on the PS5 (EXPERIMENTAL): engine /api/ps5/usbnet, helper method
// `usbnet`. The helper drives the adapter and gives it its own address; the engine sends transfers
// to that address when this computer can reach it, else through the console's own network. The
// helper's answer is written down in payload/include/usbnet.h. A plain fetch against the engine
// origin, so the desktop app and the web UI take the same path (as api/tailnet.ts).

import { getEngineUrl } from "../state/engine";
import { consoleAddr } from "../lib/addr";

export { linkLong, linkShort } from "../lib/linkSpeed";

export type UsbNetState =
  | "off"
  | "none"
  | "paused"
  | "starting"
  | "no_link"
  | "no_address"
  | "ready"
  | "failed"
  | "unsupported";

export interface UsbNetAdapter {
  /** "vvvv:pppp" */
  usb_id: string;
  /** "RTL8156B", "RTL8157", "RTL8159" */
  chip: string;
  /** The chip's top speed (2500, 5000, 10000). */
  max_mbps: number;
}

/** Whether this computer reaches the adapter's address, as the engine last checked. */
export interface UsbNetRoute {
  reachable: boolean;
  /** Transfers to this console go to the adapter's address. */
  preferred: boolean;
  /** null: not checked yet. */
  checked_ms_ago: number | null;
}

export interface UsbNetStatus {
  ok: boolean;
  experimental: boolean;
  enabled: boolean;
  state: UsbNetState;
  error: string;
  adapter: UsbNetAdapter | null;
  link_mbps: number;
  ip: string;
  mask: string;
  gateway: string;
  /** How the address was got: "dhcp", "link-local", "static", or "" (none yet). */
  source: "dhcp" | "link-local" | "static" | "";
  /** The static address set, "" for DHCP. */
  static_ip: string;
  port: number;
  /** Minion answers on the adapter's address too. */
  minion: boolean;
  rx_bytes: number;
  tx_bytes: number;
  rx_frames: number;
  tx_frames: number;
  errors: number;
  link_changes: number;
  uptime_s: number;
  restarts: number;
  /** The other USB devices' vendor:product IDs (hubs left out). */
  seen: string[];
  /** The engine's own look (absent from the helper's answer, e.g. on the console page). */
  route?: UsbNetRoute;
}

/** The helper predates USB adapter support (the engine answers 501/404 `unsupported`). */
export class UsbNetUnsupportedError extends Error {
  constructor() {
    super("unsupported");
    this.name = "UsbNetUnsupportedError";
  }
}

async function call(route: string, init?: RequestInit): Promise<UsbNetStatus> {
  const res = await fetch(`${getEngineUrl()}${route}`, { ...init, signal: AbortSignal.timeout(20_000) });
  const body = (await res.json().catch(() => ({}))) as Record<string, unknown>;
  if (res.status === 501 || res.status === 404 || body.reason === "unsupported") throw new UsbNetUnsupportedError();
  if (!res.ok) {
    throw new Error(typeof body.error === "string" && body.error ? body.error : `request failed (${res.status})`);
  }
  return body as unknown as UsbNetStatus;
}

export function usbNetStatus(host: string): Promise<UsbNetStatus> {
  return call(`/api/ps5/usbnet?addr=${encodeURIComponent(consoleAddr(host))}`);
}

export type UsbNetOp = "enable" | "disable" | "address";

/** op "address": a static address, or `ip: ""` for DHCP. */
export interface UsbNetAddress {
  ip?: string;
  mask?: string;
  gateway?: string;
}

export function usbNetOp(host: string, op: UsbNetOp, opts: UsbNetAddress = {}): Promise<UsbNetStatus> {
  return call("/api/ps5/usbnet", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ addr: consoleAddr(host), op, ...opts }),
  });
}

/** A dotted IPv4 address ("192.168.1.50"). */
export function isIpv4(s: string): boolean {
  const parts = s.trim().split(".");
  return parts.length === 4 && parts.every((p) => /^\d{1,3}$/.test(p) && Number(p) <= 255);
}
