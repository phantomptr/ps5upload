import { invoke } from "./invokeLogged";

import { useConnectionStore } from "../state/connection";
import { hostOf, mgmtAddr, transferAddr } from "./addr";
import { redactHost } from "./diagnosticBundle";
import { blackBoxFor, recordBlackBox } from "./payloadBlackBox";
import {
  fetchHwInfo,
  fetchHwTemps,
  fetchHwPower,
  fetchHwStorage,
  appListRunning,
  appsInstalled,
  listVolumes,
  procListGet,
  netInterfacesGet,
  klogChunk,
  fsReadPreview,
  fsListDir,
  smpStatus,
  smpCheckoutStatus,
  portProbe,
  type SmpStatus,
  type SmpCheckout,
  type HwInfo,
  type HwTemps,
  type HwPower,
  type HwStorage,
  type RunningApp,
  type ProcEntry,
  type NetInterface,
  type Volume,
} from "../api/ps5";

/**
 * Compose a best-effort diagnostic snapshot of the connected PS5 for a bug
 * report. Every field is optional: each probe is guarded so a single failing
 * RPC (or an old payload that lacks one) never sinks the whole snapshot, and
 * the whole thing short-circuits to `{ connected: false }` when there's no PS5
 * to talk to.
 *
 * This is pure renderer glue over the *existing* diagnostic Tauri commands —
 * no payload change — so it works on already-deployed helpers. The raw kernel
 * logs come back as separate strings (not embedded in the JSON) so the bundle
 * can write them as their own `.txt` files.
 *
 * Redaction (default on, mirrors `diagnosticBundle`): host IP → /16, console
 * serial + interface MAC/IPv4 → length-only placeholders. The user can untick
 * it before generating if a maintainer needs the raw values.
 */

export interface Ps5Snapshot {
  connected: boolean;
  /** ISO time the snapshot was taken. */
  captured_at: string;
  redacted: boolean;
  host: string | null;
  payload_version: string | null;
  ps5_kernel: string | null;
  ucred_elevated: boolean | null;
  prior_instance: string | null;
  hw_info: HwInfo | null;
  hw_temps: HwTemps | null;
  hw_power: HwPower | null;
  hw_storage: HwStorage | null;
  /** App ids of foreground apps/games the payload reports running. */
  running_apps: RunningApp[] | null;
  /** Full running-process list (pid + name), capped. This is the rich
   *  "what's running" — the allproc walk, not just foreground apps. */
  processes: ProcEntry[] | null;
  processes_total: number | null;
  net_interfaces: NetInterface[] | null;
  /** Per-mount capacity, INCLUDING `safety_reserve_bytes` /
   *  `allocatable_bytes`. `hw_storage` only carries the console-wide
   *  aggregate, which cannot show why an upload was refused. A 5.17.0 report
   *  ("only 300mb available, 80gb reserved") took a payload-log grep to
   *  diagnose because this was missing; with it the cause is one glance. */
  volumes: Volume[] | null;
  /** Registered title ids. `running_apps` covers what is running; this covers
   *  what is INSTALLED, which is what "the game shows a broken tile" and
   *  "my update did not apply" reports actually turn on. */
  installed_apps: { title_id: string; title_name?: string }[] | null;
  installed_apps_total: number | null;
  /** ShadowMount+ state. It owns mounting and registration for disk images,
   *  so whether it is running (and what it has mounted) explains a whole
   *  class of "my game didn't appear" reports our own logs cannot. */
  smp_status: SmpStatus | null;
  /** Open edit checkout, if any. While one is open the image is moved OUT of
   *  SMP's scan roots on purpose, so mount behaviour legitimately differs. */
  smp_checkout: SmpCheckout | null;
  /** Which app currently owns the screen, plus which focus symbols this
   *  firmware actually exports. Availability is reported separately from the
   *  value so "not foreground" is never confused with "cannot tell" — on
   *  FW 9.60 no direct foreground-app-id getter exists at all. */
  focus: unknown | null;
  /** Names of the payload's on-PS5 black-box files we pulled (the file bodies
   *  ride alongside in `Ps5SnapshotResult.payload_logs`, not in this JSON). */
  payload_log_files: string[];
  /** Where `payload_log_files` came from. "live" = read just now. "cached" =
   *  the helper could not answer at report time, so these are the copy taken
   *  when it last came up (see payloadBlackBox.ts) — for a helper that keeps
   *  dying, that copy is the only one that exists. "none" = neither. */
  payload_logs_source: "live" | "cached" | "none";
  /** When a cached copy was read. Null for live or none. */
  payload_logs_captured_at: string | null;
  /** The probe during which the helper stopped answering, if it did. Probes
   *  run one at a time, so this names the request in flight when it went —
   *  the first thing to suspect, though not proof of cause. */
  helper_lost_during: string | null;
  /** The last probe the helper answered before that. */
  helper_last_answered: string | null;
  /** Which of the PS5's service ports are accepting connections, and why a
   *  probe failed when one isn't.
   *
   *  Added after a report where an update install failed with "ps5upload
   *  couldn't start the PS5's update installer": the cause was that the
   *  console's ELF loader had stopped answering on :9021, so the installer
   *  image could not be delivered. Nothing in the bundle said that — it had
   *  to be inferred from one line of an app log. The loader port is the
   *  single dependency the whole install fallback rests on, and it isn't
   *  ours, so a bundle should always state whether it was up. */
  ports: PortState[] | null;
  /** Per-probe failures, keyed by probe name, so a maintainer can see what
   *  couldn't be collected and why. */
  errors: Record<string, string>;
}

/** One probed PS5 service port. */
export interface PortState {
  port: number;
  /** What listens there, so the reader doesn't need the port table. */
  role: string;
  open: boolean;
  /** Why the probe failed, when it did. */
  error: string | null;
}

/** The ports a diagnosis actually turns on, with who owns each. `loader` is
 *  the console's own jailbreak loader — not ours — which is exactly why its
 *  state has to be recorded rather than assumed. */
const PROBED_PORTS: { port: number; role: string }[] = [
  { port: 9021, role: "ELF loader (console's, not ps5upload's)" },
  { port: 9040, role: "DPI install daemon" },
  { port: 9113, role: "ps5upload transfer" },
  { port: 9114, role: "ps5upload management" },
  { port: 2121, role: "ps5upload FTP" },
];

/** One on-PS5 diagnostic file fetched for the bundle. */
export interface PayloadLogFile {
  /** Zip-entry leaf name (e.g. "startup.log", "tx_events.log"). */
  name: string;
  text: string;
}

export interface Ps5SnapshotResult {
  snapshot: Ps5Snapshot;
  /** Raw /dev/klog tail (separate file in the bundle). */
  klog: string | null;
  /** Raw kern.msgbuf tail (separate file in the bundle). */
  syslog: string | null;
  /** The payload's own on-PS5 logs (startup trace, tx event log, tx-state,
   *  any per-tx journal, a crash marker if present) — fetched via FS_READ, no
   *  payload change. This is the helper's black box: when the helper crashes,
   *  this is where the "what was it doing" lives. Each becomes a file under
   *  `ps5/payload-logs/` in the zip. */
  payload_logs: PayloadLogFile[];
}

/** Cap the embedded process list so a busy console can't bloat report.json;
 *  the full count is preserved in `processes_total`. */
const PROC_CAP = 400;
/** Installed-title cap. A full console runs to a few hundred titles; the ids
 *  matter, the tail does not, and the bundle is posted to a chat channel. */
const INSTALLED_APP_CAP = 500;

function placeholder(v: string | null | undefined): string | null {
  if (!v) return v ?? null;
  return `<redacted:${v.length}-char>`;
}

/** Decode base64 (what fs_read_preview returns) to a UTF-8 string. */
function decodeB64Utf8(b64: string): string {
  const bin = atob(b64);
  const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

/**
 * Pull the payload's on-PS5 black-box files via the existing FS_READ RPC (no
 * payload change). Paths are HARDWARE-VERIFIED against live 2.26.1 helpers —
 * the on-disk layout differs from a naive code read (e.g. the tx-state file is
 * `tx/runtime_tx_state.txt`, not `runtime/state`). Each fetch is best-effort;
 * a missing file is normal (e.g. `crash.log` only exists after a crash).
 */
async function fetchPayloadLogs(host: string): Promise<{
  files: PayloadLogFile[];
  errors: Record<string, string>;
}> {
  const maddr = mgmtAddr(host);
  const out: PayloadLogFile[] = [];
  const errors: Record<string, string> = {};

  // path → zip-friendly leaf name. Fixed, always-worth-trying files.
  // Ordered by what explains a dying helper. A capture taken right after a
  // relaunch may be cut short by the next death, so the previous instance's
  // last words come first: its stderr (rotated to .old by the new instance),
  // the fatal breadcrumb, and the startup trace of whichever run is current.
  const fixed: Array<[string, string]> = [
    ["/data/ps5upload/stderr.log.old", "stderr_old.log"],
    // Only present after a crash (the async-signal-safe marker); harmless miss.
    ["/data/ps5upload/crash.log", "crash.log"],
    // The helper's own captured stderr — its rich per-failure diagnostics.
    ["/data/ps5upload/stderr.log", "stderr.log"],
    ["/data/ps5upload/startup.log", "startup.log"],
    ["/data/ps5upload_startup.log", "startup_early.log"],
    ["/data/ps5upload/runtime/active_instance.txt", "active_instance.txt"],
    ["/data/ps5upload/tx/events.log", "tx_events.log"],
    ["/data/ps5upload/tx/runtime_tx_state.txt", "tx_state.txt"],
    // ShadowMount+ is third-party but owns mounting and registration for
    // disk images, so its log and config explain a whole class of "my game
    // didn't mount / didn't appear" reports that our own logs cannot. Its
    // config also records the kstuff auto-toggle settings that affect a
    // running game. Both were read by hand repeatedly during a mount and a
    // focus-drop investigation before being collected here.
    ["/data/shadowmount/debug.log", "smp_debug.log"],
    ["/data/shadowmount/config.ini", "smp_config.ini"],
    // Open edit-checkout journal. While a checkout is open the image is
    // deliberately moved out of SMP's scan roots, so mount/registration
    // behaviour legitimately differs — without this a report filed mid-edit
    // looks like a bug.
    ["/data/ps5upload/editing/checkout.json", "edit_checkout.json"],
  ];

  // Discover any per-transaction journal/shard logs in the tx dir (present
  // when an upload was in flight — exactly the helper-crash case).
  try {
    const entries = await fsListDir(transferAddr(host), "/data/ps5upload/tx");
    for (const e of entries) {
      if (e.kind !== "file") continue;
      if (/^tx_.*\.json$/.test(e.name) || /^shards_.*\.log$/.test(e.name)) {
        fixed.push([`/data/ps5upload/tx/${e.name}`, e.name]);
      }
    }
  } catch (e) {
    errors["payload_logs_listdir"] = e instanceof Error ? e.message : String(e);
  }

  // Bound total work/size: up to 14 files, 256 KB each.
  for (const [path, name] of fixed.slice(0, 14)) {
    try {
      const r = await fsReadPreview(maddr, path);
      const text = decodeB64Utf8(r.base64);
      if (text.length > 0) out.push({ name, text });
    } catch {
      // Missing/unreadable file is the common case (fresh install, no crash).
      // Don't record per-file misses as errors — too noisy; the dir-list error
      // above already flags a real connectivity problem.
    }
  }
  return { files: out, errors };
}

/**
 * Read the helper's logs now and keep the copy (see payloadBlackBox.ts).
 * Called on every arrival at "up", so that a helper which dies again before
 * anyone files a report has still left its predecessor's last words behind.
 * Best-effort and silent: this is evidence gathering, never a user action.
 */
export async function capturePayloadBlackBox(host: string): Promise<void> {
  try {
    const { files } = await fetchPayloadLogs(host);
    recordBlackBox(host, files);
  } catch {
    // The helper went away mid-read. Whatever was recorded before stands.
  }
}

/** Fill the snapshot's payload logs from the kept copy, when there is one. */
function fillFromBlackBox(
  base: Ps5Snapshot,
  host: string,
): PayloadLogFile[] {
  const cached = blackBoxFor(host);
  if (!cached) return [];
  base.payload_logs_source = "cached";
  base.payload_logs_captured_at = cached.capturedAt;
  base.payload_log_files = cached.files.map((f) => f.name);
  return cached.files;
}

export async function buildPs5Snapshot(opts: {
  redact: boolean;
}): Promise<Ps5SnapshotResult> {
  const conn = useConnectionStore.getState();
  const host = conn.host?.trim() ?? "";
  const captured_at = new Date().toISOString();

  const base: Ps5Snapshot = {
    connected: false,
    captured_at,
    redacted: opts.redact,
    host: host ? redactHost(host, opts.redact) : null,
    payload_version: conn.payloadVersion ?? null,
    ps5_kernel: conn.ps5Kernel ?? null,
    ucred_elevated: conn.ucredElevated ?? null,
    prior_instance: conn.priorInstance ?? null,
    hw_info: null,
    hw_temps: null,
    hw_power: null,
    hw_storage: null,
    running_apps: null,
    processes: null,
    processes_total: null,
    net_interfaces: null,
    volumes: null,
    installed_apps: null,
    installed_apps_total: null,
    ports: null,
    smp_status: null,
    smp_checkout: null,
    focus: null,
    payload_log_files: [],
    payload_logs_source: "none",
    payload_logs_captured_at: null,
    helper_lost_during: null,
    helper_last_answered: null,
    errors: {},
  };

  // No reachable PS5 → host-only snapshot. The rest of the bundle (renderer
  // stores, app logs) is still useful, so this is a normal path, not an error.
  if (!host || conn.payloadStatus !== "up") {
    const cached = host ? fillFromBlackBox(base, host) : [];
    return { snapshot: base, klog: null, syslog: null, payload_logs: cached };
  }
  base.connected = true;

  const taddr = transferAddr(host);
  const maddr = mgmtAddr(host);

  // Run a probe, recording any failure into `errors[name]` instead of throwing.
  const errors = base.errors;
  async function probe<T>(name: string, fn: () => Promise<T>): Promise<T | null> {
    try {
      return await fn();
    } catch (e) {
      errors[name] = e instanceof Error ? e.message : String(e);
      return null;
    }
  }

  // Collect ONE probe at a time, the helper's own logs first and the
  // hardware reads last.
  //
  // This used to fire all sixteen probes at once. On a FW 12.70 console the
  // report's own burst was followed 107 ms later by every connection
  // resetting and the helper's listener disappearing (generated_at
  // 15:51:41.895, resets at 15:51:42.029) — after the helper had been up for
  // 7.5 s, so a coincidence that tight is roughly 1 in 200. Four of the
  // probes (focus, hw_info, hw_storage, syslog) had never once been run
  // against that console while it was healthy, and hw_temps runs here in its
  // EXTENDED form (SoC power), which the dashboard's 109 clean reads never
  // exercised. The tool meant to explain a dying helper was plausibly what
  // killed it, and with everything in flight at once there was no telling
  // which probe did it.
  //
  // Sequential costs a second or two on a healthy console and buys two
  // things: the logs are in hand before anything risky is asked, and if the
  // helper does go away, the report names the probe it went away on.
  let lostDuring: string | null = null;
  let lastAnswered: string | null = null;
  const HELPER_GONE =
    /connection reset|connection refused|read frame header|broken pipe|unexpected eof|failed to fill whole buffer/i;
  async function step<T>(name: string, fn: () => Promise<T>): Promise<T | null> {
    if (lostDuring) {
      // Every remaining probe would fail the same way; asking again only adds
      // noise and, if the helper is restarting, load.
      errors[name] = `skipped: the helper stopped answering during "${lostDuring}"`;
      return null;
    }
    try {
      const v = await fn();
      lastAnswered = name;
      return v;
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      errors[name] = msg;
      if (HELPER_GONE.test(msg)) lostDuring = name;
      return null;
    }
  }

  // 1. The black box. Everything after this is optional by comparison.
  const payloadLogsRes = await fetchPayloadLogs(host);
  if (payloadLogsRes.files.length > 0) lastAnswered = "payload_logs";

  // 2. Ordinary state reads, all proven on the consoles we have logs from.
  const apps = await step("running_apps", () => appListRunning(maddr));
  const procs = await step("processes", () => procListGet(maddr));
  const volumes = await step("volumes", () => listVolumes(taddr));
  const installedApps = await step("installed_apps", () => appsInstalled(taddr));
  const nets = await step("net_interfaces", () => netInterfacesGet(maddr));
  const smp = await step("smp_status", () => smpStatus(taddr));
  const checkout = await step("smp_checkout", () => smpCheckoutStatus(taddr));

  // 3. Kernel log tails.
  const klog = await step("klog", () => klogChunk(maddr, 64 * 1024));
  const syslogRes = await step("syslog", () =>
    invoke<{ text?: string }>("ps5_syslog_tail", { addr: taddr }),
  );

  // 4. Hardware and Sony-API reads last, least-proven last of all.
  const hwStorage = await step("hw_storage", () => fetchHwStorage(taddr));
  const hwPower = await step("hw_power", () => fetchHwPower(taddr));
  // Extended read pulls SoC power / fan / usage — fine for an explicit,
  // user-initiated bug report (not the always-on Dashboard poll).
  const hwTemps = await step("hw_temps", () => fetchHwTemps(taddr, true));
  const hwInfo = await step("hw_info", () => fetchHwInfo(taddr));
  // Newer helpers only; an older payload rejects the frame and this records
  // as a normal per-probe error rather than sinking the report.
  const focus = await step("focus", () =>
    invoke<unknown>("ps5_focus", { addr: maddr }),
  );

  // 5. Plain TCP connects, never an RPC, so they run whatever happened above
  // — and after a helper death, which ports are still open is the point.
  // Deliberately a connect and nothing more: an ELF loader that is handed a
  // connection and no bytes can try to execute an empty image, so we must
  // never write to :9021 here.
  const ports = await probe("ports", () =>
    Promise.all(
      PROBED_PORTS.map(async ({ port, role }) => {
        const r = await portProbe(hostOf(host), port);
        return { port, role, open: r.open, error: r.error };
      }),
    ),
  );

  base.helper_lost_during = lostDuring;
  base.helper_last_answered = lastAnswered;

  base.hw_info = hwInfo;
  base.hw_temps = hwTemps;
  base.hw_power = hwPower;
  base.hw_storage = hwStorage;
  base.running_apps = apps?.apps ?? null;

  if (procs?.procs) {
    base.processes_total = procs.procs.length;
    base.processes = procs.procs.slice(0, PROC_CAP);
  }

  base.net_interfaces = nets?.interfaces ?? null;
  base.volumes = volumes;
  if (installedApps?.titles) {
    base.installed_apps_total = installedApps.titles.length;
    // Ids + names only: origin/source add bulk without changing a diagnosis,
    // and the list is capped for the same reason `processes` is.
    base.installed_apps = installedApps.titles
      .slice(0, INSTALLED_APP_CAP)
      .map((t) => ({ title_id: t.titleId, title_name: t.titleName }));
  }
  base.ports = ports;
  base.smp_status = smp;
  base.smp_checkout = checkout;
  base.focus = focus;
  base.payload_log_files = payloadLogsRes.files.map((f) => f.name);
  Object.assign(errors, payloadLogsRes.errors);
  let payloadLogs = payloadLogsRes.files;
  if (payloadLogs.length > 0) {
    base.payload_logs_source = "live";
    recordBlackBox(host, payloadLogs);
  } else {
    // "Up" by the debounced status, but it could not answer the reads: the
    // report raced the helper's death. Ship the kept copy instead of nothing.
    payloadLogs = fillFromBlackBox(base, host);
  }

  // Redact identifiers the user might not want in a public channel.
  if (opts.redact) {
    if (base.hw_info && base.hw_info.serial) {
      base.hw_info.serial = placeholder(base.hw_info.serial) ?? base.hw_info.serial;
    }
    if (base.net_interfaces) {
      base.net_interfaces = base.net_interfaces.map((n) => ({
        ...n,
        mac: placeholder(n.mac) ?? n.mac,
        ipv4: redactHost(n.ipv4, true),
      }));
    }
  }

  return {
    snapshot: base,
    klog: klog ?? null,
    syslog: syslogRes?.text ?? null,
    payload_logs: payloadLogs,
  };
}
