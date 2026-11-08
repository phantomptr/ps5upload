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

// ---- this device's own node (Settings › Tailscale; engine /api/tailnet) ----

export interface AppTailnetConsole {
  host: string;
  ip: string;
  name: string;
  /** "lan" | "direct" | "relayed" */
  via: string;
  latency_ms: number;
}

export interface AppTailnetStatus {
  ok: boolean;
  enabled: boolean;
  state: TailnetState;
  login_url: string;
  ip: string;
  name: string;
  control_url: string;
  hostname: string;
  home_derp: number;
  derp_connected: boolean;
  home_latency_ms: number;
  key_expiry: number;
  error: string;
  /** Something to know while it runs (the key expires soon); absent from an older engine. */
  warning?: string;
  peers: TailnetPeer[];
  consoles: AppTailnetConsole[];
}

export function appTailnetStatus(): Promise<AppTailnetStatus> {
  return call("/api/tailnet") as unknown as Promise<AppTailnetStatus>;
}

export function appTailnetOp(
  op: "enable" | "disable" | "logout",
  opts: TailnetEnable = {},
): Promise<AppTailnetStatus> {
  return call("/api/tailnet", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ op, ...opts }),
  }) as unknown as Promise<AppTailnetStatus>;
}

export interface TailnetVia {
  /** "lan" | "direct" | "relayed" */
  via: string;
  latency_ms: number;
}

// ---- problems, in words (Settings › Tailscale and the console's Tailscale panel) ----

/** One thing wrong, as a translation key, its English, and what it says (`why`: the server's own
 *  words for a refused sign-in). `bad` is an error; otherwise a warning. */
export interface TailnetProblem {
  key: string;
  fallback: string;
  vars?: Record<string, string | number>;
  bad: boolean;
}

/** Waiting this long to connect or for the sign-in is no longer normal: say so. */
export const TAILNET_SLOW_MS = 60_000;

/** The core's words for a network failure (tailnet/src/tailnet.c tn_strerror). */
const UNREACHABLE = /^(connection error|timeout|TLS error|DNS lookup failed|unexpected HTTP status|protocol error)$/i;

/**
 * What to tell the user about a node, from the engine's or helper's `state`, `error` and `warning`,
 * instead of a spinner that never ends: a refused sign-in, a node signed out (key expired or
 * removed), a control server out of reach, a key expired or expiring, and a sign-in pending longer
 * than {@link TAILNET_SLOW_MS} (`pendingMs`: how long it has been connecting or waiting for the
 * sign-in). Approval needed has its own line already ("wait-approval").
 */
export function tailnetProblems(
  st: { state: TailnetState; error?: string; warning?: string; key_expiry?: number },
  pendingMs: number,
): TailnetProblem[] {
  const out: TailnetProblem[] = [];
  const error = (st.error ?? "").trim();
  const warning = (st.warning ?? "").trim();
  if (st.state === "signed-out" || /signed out at control/i.test(error)) {
    out.push({
      key: "tn_err_signed_out",
      fallback: "Signed out of the tailnet: its key expired or the device was removed. Sign out, then sign in again.",
      bad: true,
    });
  } else if (error && st.state !== "running") {
    if (UNREACHABLE.test(error)) {
      out.push({
        key: "tn_err_unreachable",
        fallback: "Can't reach the Tailscale server. Check the internet connection; it keeps trying.",
        bad: true,
      });
    } else if (/^(control refused registration|authentication failed)$/i.test(error)) {
      out.push({ key: "tn_err_refused", fallback: "Tailscale refused the sign-in.", bad: true });
    } else {
      out.push({
        key: "tn_err_refused_why",
        fallback: "Tailscale refused the sign-in: {why}",
        vars: { why: error },
        bad: true,
      });
    }
  }
  if (/expired/i.test(warning)) {
    out.push({ key: "tn_warn_expired", fallback: "The Tailscale key has expired. Sign out, then sign in again.", bad: true });
  } else if (/expires/i.test(warning) && expiryDays(st.key_expiry ?? 0) === null) {
    // (Within a week the days are said already, from key_expiry.)
    out.push({
      key: "tn_warn_expiring",
      fallback: "The Tailscale key expires soon. Sign out, then sign in again to renew it.",
      bad: false,
    });
  }
  if ((st.state === "connecting" || st.state === "needs-login") && pendingMs >= TAILNET_SLOW_MS && !out.some((p) => p.bad)) {
    out.push({
      key: "tn_slow",
      fallback:
        "Still not connected after a minute. If you finished signing in, check that your tailnet admin approved the device, or turn Tailscale off and on again.",
      bad: false,
    });
  }
  return out;
}

/** How the engine reaches this console right now (the status-bar chip). */
export async function tailnetVia(host: string): Promise<TailnetVia> {
  try {
    const res = await fetch(`${getEngineUrl()}/api/tailnet/via?addr=${encodeURIComponent(consoleAddr(host))}`, {
      signal: AbortSignal.timeout(10_000),
    });
    if (!res.ok) return { via: "lan", latency_ms: -1 };
    return (await res.json()) as TailnetVia;
  } catch {
    return { via: "lan", latency_ms: -1 };
  }
}
