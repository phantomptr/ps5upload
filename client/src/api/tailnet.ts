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
  /** Has a sign-in kept on the PS5 (off is then a pause). Absent from an older helper. */
  signed_in?: boolean;
  /** Who may reach the PS5 over Tailscale: "listed" (only `allow`) or "all". Absent from an older
   *  helper (which lets every device of the tailnet in). */
  allow_mode?: "listed" | "all";
  /** The allowed devices' Tailscale addresses, comma-separated. */
  allow?: string;
  /** Devices the list turned away lately, newest first (empty while every device is let in).
   *  Absent from an older helper. */
  blocked_recent?: TailnetBlocked[];
}

/** A device that tried to reach the PS5 over Tailscale and was not on its list. */
export interface TailnetBlocked {
  ip: string;
  /** Its name in Tailscale ("" when the PS5 has no name for it). */
  name: string;
  port: number;
  proto: "tcp" | "udp";
  count: number;
  /** Unix seconds. */
  first: number;
  last: number;
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
  /** op "allow": "listed" or "all", and the list ("100.a.b.c,100.d.e.f"). */
  allow_mode?: "listed" | "all";
  allow?: string;
  /** op "allow_add": one device's Tailscale address. */
  ip?: string;
}

export type TailnetConsoleOp = "enable" | "disable" | "logout" | "rename" | "renew" | "allow" | "allow_add" | "blocked_ignore";

export function tailnetOp(host: string, op: TailnetConsoleOp, opts: TailnetEnable = {}): Promise<TailnetStatus> {
  return call("/api/ps5/tailnet", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ addr: consoleAddr(host), op, ...opts }),
  });
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
  /** "lan" | "direct" | "relayed" | "system" */
  via: string;
  latency_ms: number;
  /** When "Try Tailscale now" last reached it through Tailscale (unix ms; 0 or absent: never). */
  tested_at?: number;
}

/** The system's own Tailscale app on this device, as the engine sees it. */
export interface SystemTailscale {
  detected: boolean;
  ip: string;
  iface: string;
  /** "" not asked yet, "system" use it, "own" sign PS5Upload in itself. */
  choice: "" | "system" | "own";
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
  /** Has a sign-in kept on this device: off is then a pause. Absent from an older engine. */
  signed_in?: boolean;
  peers: TailnetPeer[];
  consoles: AppTailnetConsole[];
  /** This device's own name + "-ps5u": the name asked for when none is set. */
  default_hostname?: string;
  /** Tailscale gave another name (the asked one was taken): that name, else null. */
  renamed_to?: string | null;
  /** How this device is on Tailscale: "builtin", "system" or "none". */
  backend?: "builtin" | "system" | "none";
  system?: SystemTailscale;
}

export function appTailnetStatus(): Promise<AppTailnetStatus> {
  return call("/api/tailnet") as unknown as Promise<AppTailnetStatus>;
}

export type AppTailnetOp = "enable" | "disable" | "logout" | "rename" | "renew" | "system_choice" | "test_path";

export interface AppTailnetOpts extends TailnetEnable {
  /** op "system_choice". */
  choice?: "system" | "own";
  /** op "test_path": the console, and how long (0 ends it). */
  host?: string;
  secs?: number;
}

export function appTailnetOp(op: AppTailnetOp, opts: AppTailnetOpts = {}): Promise<AppTailnetStatus> {
  return call("/api/tailnet", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ op, ...opts }),
  }) as unknown as Promise<AppTailnetStatus>;
}

export interface TailnetVia {
  /** "lan" | "direct" | "relayed" | "system" */
  via: string;
  latency_ms: number;
}

/** One port checked on one path. */
export interface PathCheck {
  port: number;
  open: boolean;
  error: string | null;
}

/** The Connection check by path (engine GET /api/tailnet/check). */
export interface TailnetCheck {
  home: PathCheck[];
  /** Only when nothing answered at home and the PS5 has a Tailscale address. */
  tailnet: { backend: "builtin" | "system"; ip: string; online: boolean | null; checks: PathCheck[] } | null;
  tailnet_ip: string | null;
  /** The path that answered: "lan", "builtin", "system", or "none". */
  used: "lan" | "builtin" | "system" | "none";
  device: {
    backend: "builtin" | "system" | "none";
    enabled: boolean;
    signed_in: boolean;
    state: TailnetState;
    system_detected: boolean;
  };
}

/** The check by path; null from an engine without it (or one that is away). */
export async function tailnetCheck(host: string): Promise<TailnetCheck | null> {
  try {
    const res = await fetch(`${getEngineUrl()}/api/tailnet/check?addr=${encodeURIComponent(consoleAddr(host))}`, {
      signal: AbortSignal.timeout(15_000),
    });
    if (!res.ok) return null;
    const body = (await res.json()) as TailnetCheck;
    return Array.isArray(body.home) ? body : null;
  } catch {
    return null;
  }
}

/** This device's Tailscale in one word, for the Connection check: the built-in node on, paused
 *  or signed out, the system's Tailscale app, or nothing. */
export function deviceTailscale(
  d: TailnetCheck["device"] | null | undefined,
): "builtin-on" | "builtin-connecting" | "builtin-paused" | "system" | "signed-out" | "none" {
  if (!d) return "none";
  if (d.backend === "builtin") return d.state === "running" ? "builtin-on" : "builtin-connecting";
  if (d.backend === "system") return "system";
  if (d.signed_in) return "builtin-paused";
  return d.system_detected ? "system" : "signed-out";
}

/** Should the app ask "use the system's Tailscale app or sign PS5Upload in?": the app is there,
 *  PS5Upload's own Tailscale is not set up, and the user has not answered. */
export function shouldAskSystemChoice(st: AppTailnetStatus | null): boolean {
  if (!st?.system?.detected) return false;
  if (st.system.choice) return false;
  return !st.enabled && !st.signed_in;
}

/** The comma-separated allow-list as addresses. */
export function allowList(st: Pick<TailnetStatus, "allow"> | null | undefined): string[] {
  return (st?.allow ?? "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

/** The list with `ip` ticked on or off. */
export function toggleAllowed(list: string[], ip: string, on: boolean): string[] {
  const rest = list.filter((x) => x !== ip);
  return on ? [...rest, ip] : rest;
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
      fallback: "Signed out of the tailnet: its key expired or the device was removed. Use Renew to sign in again.",
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
    out.push({ key: "tn_warn_expired", fallback: "The Tailscale key has expired. Use Renew to sign in again; the address and name stay.", bad: true });
  } else if (/expires/i.test(warning) && expiryDays(st.key_expiry ?? 0) === null) {
    // (Within a week the days are said already, from key_expiry.)
    out.push({
      key: "tn_warn_expiring",
      fallback: "The Tailscale key expires soon. Use Renew to sign in again; the address and name stay.",
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

/** Is the path a Tailscale one? */
export function viaIsTailnet(via: string): boolean {
  return via === "direct" || via === "relayed" || via === "system";
}

/** A notice when a console switches path: "tailnet" (now through Tailscale), "home" (back on the
 *  home network), or null (no switch, or the first read). */
export function pathSwitch(prev: string | null, next: string): "tailnet" | "home" | null {
  if (prev === null) return null;
  const was = viaIsTailnet(prev);
  const is = viaIsTailnet(next);
  if (was === is) return null;
  return is ? "tailnet" : "home";
}

/** Should a large upload ask first: over a relayed Tailscale path, or on phone data, a big
 *  upload is slow (and may cost money). */
export const SLOW_PATH_UPLOAD_BYTES = 1024 * 1024 * 1024;
export function slowPathWarning(
  totalBytes: number,
  via: string,
  onCellular: boolean,
): "relayed" | "cellular" | null {
  if (totalBytes <= SLOW_PATH_UPLOAD_BYTES) return null;
  if (onCellular) return "cellular";
  if (via === "relayed") return "relayed";
  return null;
}

/** Whether this device is on phone data, where the browser can tell (Android Chrome and the
 *  Android app's WebView); false where it cannot. */
export function onCellularData(): boolean {
  const c = (globalThis.navigator as unknown as { connection?: { type?: string } } | undefined)?.connection;
  return c?.type === "cellular";
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

// ---- consoles found on the Tailscale network (engine /api/tailnet/consoles) ----

/** The console list's entry for a found console. */
export interface ListedConsole {
  id: string;
  name: string;
  host: string;
}

/** A PS5Upload console on this device's Tailscale network, met at home or not. */
export interface FoundConsole {
  /** Its tailnet name without "-ps5u". */
  name: string;
  tailnet_name: string;
  tailnet_ip: string;
  /** Its home address, when this device knows one. */
  lan_host: string | null;
  /** Its AVA1 key (hex): the same console at every address. Null when it did not answer. */
  id: string | null;
  /** Its helper answered over Tailscale. */
  reachable: boolean;
  via: "builtin" | "system" | null;
  online: boolean;
  /** This device is trusted by it already. */
  paired: boolean;
  hosts: string[];
  /** On the console list already: that entry. */
  listed: ListedConsole | null;
  error: string | null;
}

export interface FoundConsoles {
  backend: "builtin" | "system" | "none";
  /** A peer list could be read; false on a phone with only the Tailscale app (type the address). */
  can_list: boolean;
  consoles: FoundConsole[];
}

async function readJson<T>(res: Response): Promise<T> {
  const body = (await res.json().catch(() => ({}))) as Record<string, unknown>;
  if (!res.ok) {
    throw new Error(typeof body.error === "string" && body.error ? body.error : `request failed (${res.status})`);
  }
  return body as T;
}

/** The consoles on the Tailscale network (the engine keeps the last minute's look unless `refresh`). */
export async function tailnetConsoles(refresh = false): Promise<FoundConsoles> {
  const res = await fetch(`${getEngineUrl()}/api/tailnet/consoles${refresh ? "?refresh=1" : ""}`, {
    signal: AbortSignal.timeout(30_000),
  });
  if (res.status === 404) return { backend: "none", can_list: false, consoles: [] };
  const body = await readJson<Partial<FoundConsoles>>(res);
  return {
    backend: body.backend ?? "none",
    can_list: !!body.can_list,
    consoles: Array.isArray(body.consoles) ? body.consoles : [],
  };
}

export interface AddedConsole {
  /** "added", "linked" (moved to its home address), "merged" (two entries became one), "exists". */
  action: "added" | "linked" | "merged" | "exists";
  console: ListedConsole;
  found: FoundConsole;
}

/** Puts a found console (or a typed 100.x address / MagicDNS name) on the console list, once. */
export async function addTailnetConsole(addr: string, name = ""): Promise<AddedConsole> {
  const res = await fetch(`${getEngineUrl()}/api/tailnet/consoles/add`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ addr: addr.trim(), name }),
    signal: AbortSignal.timeout(30_000),
  });
  return readJson<AddedConsole>(res);
}

