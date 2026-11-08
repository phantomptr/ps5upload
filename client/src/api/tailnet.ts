// "Put this PS5 on Tailscale": the console's own tailnet node (engine /api/ps5/tailnet,
// helper method `tailnet`). A plain fetch against the engine origin, so the desktop app and
// the web UI take the same path.

import { getEngineUrl } from "../state/engine";
import { consoleAddr } from "../lib/addr";

export type TailnetState =
  | "off"
  | "connecting"
  | "needs-login"
  | "wait-approval"
  | "running"
  | "paused"
  | "backoff"
  | "signed-out"
  | "stopped";

export interface TailnetPeer {
  name: string;
  ip: string;
  online: boolean;
  /** "direct" | "relayed" | "idle" */
  path: string;
  /** "ip:port" when direct */
  direct: string;
  derp_region: number;
  /** -1 when unknown */
  latency_ms: number;
  last_rx_ms: number;
}

export interface TailnetStatus {
  ok: boolean;
  enabled: boolean;
  state: TailnetState;
  login_url: string;
  control_url: string;
  hostname: string;
  ip: string;
  name: string;
  home_derp: number;
  derp_connected: boolean;
  home_latency_ms: number;
  udp_port: number;
  endpoints: string[];
  key_expiry: number;
  warning: string;
  error: string;
  serving: { udp_flows: number; tcp_conns: number };
  peers: TailnetPeer[];
}

/** The helper predates Tailscale support (the engine answers 501 `unsupported`). */
export class TailnetUnsupportedError extends Error {
  constructor() {
    super("unsupported");
    this.name = "TailnetUnsupportedError";
  }
}

async function call(route: string, init?: RequestInit): Promise<TailnetStatus> {
  const res = await fetch(`${getEngineUrl()}${route}`, { ...init, signal: AbortSignal.timeout(20_000) });
  const body = (await res.json().catch(() => ({}))) as Record<string, unknown>;
  if (res.status === 501 || res.status === 404 || body.reason === "unsupported") throw new TailnetUnsupportedError();
  if (!res.ok) {
    throw new Error(typeof body.error === "string" && body.error ? body.error : `request failed (${res.status})`);
  }
  return body as unknown as TailnetStatus;
}

export function tailnetStatus(host: string): Promise<TailnetStatus> {
  return call(`/api/ps5/tailnet?addr=${encodeURIComponent(consoleAddr(host))}`);
}

export interface TailnetEnable {
  control_url?: string;
  hostname?: string;
  authkey?: string;
}

export function tailnetOp(
  host: string,
  op: "enable" | "disable" | "logout",
  opts: TailnetEnable = {},
): Promise<TailnetStatus> {
  return call("/api/ps5/tailnet", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ addr: consoleAddr(host), op, ...opts }),
  });
}

/** One line for a peer's path: "direct, 3.2 ms", "relayed", "idle". */
export function peerPathKind(p: TailnetPeer): "direct" | "relayed" | "idle" {
  return p.path === "direct" ? "direct" : p.path === "relayed" ? "relayed" : "idle";
}

/** Days until the node key expires, when that is within a week (Tailscale's default is 180
 *  days, Headscale's none); null otherwise. */
export function expiryDays(keyExpiry: number, nowS = Date.now() / 1000): number | null {
  if (!keyExpiry || keyExpiry <= 0) return null;
  const left = keyExpiry - nowS;
  if (left > 7 * 86400) return null;
  return Math.max(0, Math.round(left / 86400));
}
