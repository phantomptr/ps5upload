/**
 * What a bug report says about where the app runs and what the console has loaded, beyond the
 * diagnostic bundle (report spec §4.1):
 *
 *  - `system`: the OS's real name and version (WebKit's user agent is frozen at
 *    "Mac OS X 10_15_7", so the desktop and phone apps ask the OS through `host_system_info`),
 *    the machine's model, CPU and memory, the web view's own version, and how long the computer
 *    and the app have been up. The browser build reports what the page can see, and the engine's
 *    computer comes separately in engine.features.host.
 *  - `media`: what this web view can decode (lib/mediaProbe): H.264 and HEVC through WebCodecs in
 *    hardware and software, Opus, the media player (MSE and ManagedMediaSource), WebGL 2.
 *  - `remote_play_client`: the page's last Remote Play sessions as it saw them
 *    (lib/rpClientSessions): the decoder path chosen, each failure with its config, fallbacks.
 *  - `payloads`: what runs on the console as a payload (the process list's payloads), the
 *    helper's version, and what Payload Manager stores, with versions.
 *
 * Nothing here is an address, a name or a key; every probe is bounded and never throws.
 */
import { invoke } from "./invokeLogged";
import { isTauriEnv } from "./tauriEnv";
import { probeMedia, type MediaProbe } from "./mediaProbe";
import { rpClientSessions, type RpClientSession } from "./rpClientSessions";
import { mgmtAddr } from "./addr";
import { processList } from "../api/ps5";
import { pmOverview } from "../api/payloadManager";
import { useConnectionStore } from "../state/connection";

export interface SystemInfo {
  source: "app" | "browser";
  os: string | null;
  arch: string | null;
  os_name: string | null;
  os_version: string | null;
  kernel: string | null;
  machine_model: string | null;
  cpu_model: string | null;
  cpu_threads: number | null;
  memory_bytes: number | null;
  system_uptime_s: number | null;
  /** The web view's engine and version: WebKit, WebView2 (Chromium) or WebKitGTK. */
  webview_version: string | null;
  /** Since the app (desktop/phone) or the page (browser) started. */
  app_uptime_s: number | null;
  /** The page's own view, for comparison: its user agent and platform hints. */
  user_agent: string | null;
  ua_platform: string | null;
  ua_mobile: boolean | null;
  screen: { width: number; height: number; dpr: number } | null;
}

interface NativeInfo {
  os?: string;
  arch?: string;
  os_name?: string | null;
  os_version?: string | null;
  kernel?: string | null;
  machine_model?: string | null;
  cpu_model?: string | null;
  cpu_threads?: number | null;
  memory_bytes?: number | null;
  system_uptime_s?: number | null;
  webview_version?: string | null;
  app_uptime_s?: number;
}

/** The page's own view: user agent, its platform hints, screen. Exported for tests. */
export function pageView(): Pick<SystemInfo, "user_agent" | "ua_platform" | "ua_mobile" | "screen" | "cpu_threads"> {
  const nav = (typeof navigator !== "undefined" ? navigator : {}) as Navigator & {
    userAgentData?: { platform?: string; mobile?: boolean };
  };
  const scr = typeof screen !== "undefined" ? screen : null;
  return {
    user_agent: nav.userAgent ?? null,
    ua_platform: nav.userAgentData?.platform ?? nav.platform ?? null,
    ua_mobile: nav.userAgentData?.mobile ?? null,
    screen: scr
      ? { width: scr.width, height: scr.height, dpr: typeof devicePixelRatio === "number" ? devicePixelRatio : 1 }
      : null,
    cpu_threads: nav.hardwareConcurrency ?? null,
  };
}

/** The page's age in seconds (the browser build's "app uptime"). */
function pageUptimeS(): number | null {
  return typeof performance !== "undefined" ? Math.round(performance.now() / 1000) : null;
}

export async function collectSystem(native: () => Promise<NativeInfo> = () => invoke<NativeInfo>("host_system_info")): Promise<SystemInfo> {
  const page = pageView();
  const deviceMemGb = (typeof navigator !== "undefined" ? (navigator as { deviceMemory?: number }).deviceMemory : undefined) ?? null;
  const base: SystemInfo = {
    source: "browser",
    os: null,
    arch: null,
    os_name: null,
    os_version: null,
    kernel: null,
    machine_model: null,
    cpu_model: null,
    memory_bytes: deviceMemGb ? deviceMemGb * 1024 ** 3 : null,
    system_uptime_s: null,
    webview_version: null,
    app_uptime_s: pageUptimeS(),
    ...page,
  };
  if (!isTauriEnv()) return base;
  const n = await native();
  return {
    ...base,
    source: "app",
    os: n.os ?? null,
    arch: n.arch ?? null,
    os_name: n.os_name ?? null,
    os_version: n.os_version ?? null,
    kernel: n.kernel ?? null,
    machine_model: n.machine_model ?? null,
    cpu_model: n.cpu_model ?? null,
    cpu_threads: n.cpu_threads ?? base.cpu_threads,
    memory_bytes: n.memory_bytes ?? base.memory_bytes,
    system_uptime_s: n.system_uptime_s ?? null,
    webview_version: n.webview_version ?? null,
    app_uptime_s: n.app_uptime_s ?? base.app_uptime_s,
  };
}

export function collectMedia(): Promise<MediaProbe> {
  return probeMedia();
}

export function collectRpClientSessions(): RpClientSession[] {
  return rpClientSessions();
}

export interface ConsolePayloads {
  /** The PS5Upload helper's version as it last said. */
  helper_version: string | null;
  /** Payload processes running now (the process list's "payload" kind): name and memory. */
  running: { name: string; comm: string; memory_mib: number }[] | null;
  running_error: string | null;
  /** Payload Manager: whether it answers, its version, and what it stores with versions. */
  payload_manager: {
    present: boolean;
    version: string | null;
    stored: { name: string; version: string; source: string }[];
    error: string | null;
  } | null;
}

export async function collectPayloads(host: string): Promise<ConsolePayloads> {
  const out: ConsolePayloads = {
    helper_version: useConnectionStore.getState().payloadVersion ?? null,
    running: null,
    running_error: null,
    payload_manager: null,
  };
  const [procs, pm] = await Promise.allSettled([processList(mgmtAddr(host)), pmOverview(host)]);
  if (procs.status === "fulfilled") {
    out.running = procs.value.processes
      .filter((p) => p.kind === "payload")
      .map((p) => ({ name: p.name, comm: p.comm, memory_mib: p.memory_mib }));
  } else {
    out.running_error = procs.reason instanceof Error ? procs.reason.message : String(procs.reason);
  }
  if (pm.status === "fulfilled") {
    const v = pm.value;
    out.payload_manager = v.present
      ? {
          present: true,
          version: v.version || null,
          stored: v.payloads.map((p) => ({ name: p.name || p.filename, version: p.version, source: p.source })),
          error: v.payloadsError,
        }
      : { present: false, version: null, stored: [], error: v.error ?? null };
  } else {
    out.payload_manager = {
      present: false,
      version: null,
      stored: [],
      error: pm.reason instanceof Error ? pm.reason.message : String(pm.reason),
    };
  }
  return out;
}
