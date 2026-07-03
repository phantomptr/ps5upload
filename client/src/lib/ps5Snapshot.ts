import { invoke } from "./invokeLogged";

import { useConnectionStore } from "../state/connection";
import { mgmtAddr, transferAddr } from "./addr";
import { redactHost } from "./diagnosticBundle";
import {
  fetchHwInfo,
  fetchHwTemps,
  fetchHwPower,
  fetchHwStorage,
  appListRunning,
  procListGet,
  netInterfacesGet,
  klogChunk,
  fsReadPreview,
  fsListDir,
  type HwInfo,
  type HwTemps,
  type HwPower,
  type HwStorage,
  type RunningApp,
  type ProcEntry,
  type NetInterface,
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
  /** Names of the payload's on-PS5 black-box files we pulled (the file bodies
   *  ride alongside in `Ps5SnapshotResult.payload_logs`, not in this JSON). */
  payload_log_files: string[];
  /** Per-probe failures, keyed by probe name, so a maintainer can see what
   *  couldn't be collected and why. */
  errors: Record<string, string>;
}

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
  const fixed: Array<[string, string]> = [
    ["/data/ps5upload_startup.log", "startup_early.log"],
    ["/data/ps5upload/startup.log", "startup.log"],
    ["/data/ps5upload/tx/events.log", "tx_events.log"],
    ["/data/ps5upload/tx/runtime_tx_state.txt", "tx_state.txt"],
    ["/data/ps5upload/runtime/active_instance.txt", "active_instance.txt"],
    // The helper's own captured stderr — its rich per-failure diagnostics
    // (payload ≥ this build). Most valuable for a misbehaving/crashing helper.
    ["/data/ps5upload/stderr.log", "stderr.log"],
    ["/data/ps5upload/stderr.log.old", "stderr_old.log"],
    // Only present after a crash (the async-signal-safe marker); harmless miss.
    ["/data/ps5upload/crash.log", "crash.log"],
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
    hw_info: null,
    hw_temps: null,
    hw_power: null,
    hw_storage: null,
    running_apps: null,
    processes: null,
    processes_total: null,
    net_interfaces: null,
    payload_log_files: [],
    errors: {},
  };

  // No reachable PS5 → host-only snapshot. The rest of the bundle (renderer
  // stores, app logs) is still useful, so this is a normal path, not an error.
  if (!host || conn.payloadStatus !== "up") {
    return { snapshot: base, klog: null, syslog: null, payload_logs: [] };
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

  const [
    hwInfo,
    hwTemps,
    hwPower,
    hwStorage,
    apps,
    procs,
    nets,
    klog,
    syslogRes,
    payloadLogsRes,
  ] = await Promise.all([
      probe("hw_info", () => fetchHwInfo(taddr)),
      // Extended read pulls SoC power / fan / usage — fine for an explicit,
      // user-initiated bug report (not the always-on Dashboard poll).
      probe("hw_temps", () => fetchHwTemps(taddr, true)),
      probe("hw_power", () => fetchHwPower(taddr)),
      probe("hw_storage", () => fetchHwStorage(taddr)),
      probe("running_apps", () => appListRunning(maddr)),
      probe("processes", () => procListGet(maddr)),
      probe("net_interfaces", () => netInterfacesGet(maddr)),
      probe("klog", () => klogChunk(maddr, 64 * 1024)),
      probe("syslog", () =>
        invoke<{ text?: string }>("ps5_syslog_tail", { addr: taddr }),
      ),
      // The helper's own on-PS5 black box (startup trace, tx events, crash
      // marker). Returns its own per-file errors rather than throwing.
      fetchPayloadLogs(host),
    ]);

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
  base.payload_log_files = payloadLogsRes.files.map((f) => f.name);
  Object.assign(errors, payloadLogsRes.errors);

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
    payload_logs: payloadLogsRes.files,
  };
}
