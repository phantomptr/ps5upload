import { useActivityHistoryStore } from "../state/activityHistory";
import { useNotificationsStore } from "../state/notifications";
import { useLogsStore } from "../state/logs";
import { useRosterStore } from "../state/roster";
import { useScheduleStore } from "../state/schedules";
import { useConnectionStore } from "../state/connection";
import { usePlayTimeStore } from "../state/playTime";

/**
 * Build a single-file diagnostic bundle for sharing on bug reports.
 *
 * Format: a JSON document. Single-file > zip because:
 *   - Maintainers can paste-inspect without an extraction step
 *   - No new dependency (we don't bundle a zip writer)
 *   - Schema-evolves cleanly — adding a field doesn't break old reports
 *
 * Privacy:
 *   - Host IPs are redacted to /16 by default (`192.168.X.X`).
 *     The user can untick "redact" before exporting, but the default
 *     errs on the side of not publishing LAN topology to GitHub.
 *   - Per-PS5 notes are kept (the user wrote them; they own them and
 *     can review the JSON before posting).
 */

export interface DiagnosticBundle {
  schema: number;
  generated_at: string;
  app_version: string;
  user_agent: string;
  redacted: boolean;
  /** What triggered this report. `null` for a manual export; a short
   *  description (e.g. "error: Upload failed", "frontend-error: …") for an
   *  auto-captured crash report. */
  trigger: string | null;
  /** Host environment — surfaces the "crappy computer / low memory" cases
   *  (deviceMemory, JS heap) that matter for OOM/perf triage. */
  platform: {
    os: string | null;
    language: string | null;
    cpu_cores: number | null;
    device_memory_gb: number | null;
    js_heap_used_mb: number | null;
    js_heap_limit_mb: number | null;
  };
  connection: {
    host: string | null;
    engine_status: string;
    payload_status: string;
    payload_version: string | null;
    ps5_kernel: string | null;
    ucred_elevated: boolean | null;
  };
  roster: Array<{
    id: string;
    name: string;
    host: string;
    last_seen_at: number | null | undefined;
    last_seen_kernel: string | null | undefined;
    last_seen_payload: string | null | undefined;
    notes: string | null | undefined;
  }>;
  schedules_count: number;
  play_time_titles: number;
  recent_activity: Array<{
    id: string;
    label: string;
    outcome: string;
    started_at: number;
    ended_at: number | null;
  }>;
  recent_notifications: Array<{
    id: string;
    level: string;
    title: string;
    body?: string;
    ts: number;
  }>;
  recent_logs: Array<{
    level: string;
    source: string;
    message: string;
    timestamp: number;
  }>;
}

/**
 * Exported for testing. Redacts the last two octets of an IPv4
 * address; preserves IPv6/hostnames as a length-only placeholder.
 */
export function redactHost(host: string | null | undefined, redact: boolean): string {
  if (!host) return "";
  if (!redact) return host;
  // IPv4: keep first two octets, redact last two.
  const m = host.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
  if (m) return `${m[1]}.${m[2]}.X.X`;
  // IPv6 / hostname: hash by length only — preserves shape without
  // revealing the actual address.
  return `<host:${host.length}-char>`;
}

export function buildDiagnosticBundle(opts: {
  appVersion: string;
  redact: boolean;
  /** Set for auto-captured crash reports; omit for manual exports. */
  trigger?: string | null;
  /** How many recent log lines to include. Crash reports pass a larger
   *  value than the manual export so a maintainer sees more context. */
  logLimit?: number;
}): DiagnosticBundle {
  const conn = useConnectionStore.getState();
  const roster = useRosterStore.getState().profiles;
  const schedules = useScheduleStore.getState().schedules;
  const playTimesByHost = usePlayTimeStore.getState().byHost;
  const activity = useActivityHistoryStore.getState().entries;
  const notifications = useNotificationsStore.getState().entries;
  const logs = useLogsStore.getState().entries;

  const nav: any = typeof navigator !== "undefined" ? navigator : {};
  const mem: any =
    typeof performance !== "undefined" ? (performance as any).memory : undefined;
  const toMb = (b: number | undefined) =>
    typeof b === "number" ? Math.round(b / 1048576) : null;

  return {
    schema: 2,
    generated_at: new Date().toISOString(),
    app_version: opts.appVersion,
    user_agent: typeof navigator !== "undefined" ? navigator.userAgent : "n/a",
    redacted: opts.redact,
    trigger: opts.trigger ?? null,
    platform: {
      os: nav.platform ?? null,
      language: nav.language ?? null,
      cpu_cores: nav.hardwareConcurrency ?? null,
      device_memory_gb: nav.deviceMemory ?? null,
      js_heap_used_mb: toMb(mem?.usedJSHeapSize),
      js_heap_limit_mb: toMb(mem?.jsHeapSizeLimit),
    },
    connection: {
      host: redactHost(conn.host, opts.redact),
      engine_status: conn.engineStatus,
      payload_status: conn.payloadStatus,
      payload_version: conn.payloadVersion,
      ps5_kernel: conn.ps5Kernel,
      ucred_elevated: conn.ucredElevated,
    },
    roster: roster.map((p) => ({
      id: p.id,
      name: p.name,
      host: redactHost(p.host, opts.redact),
      last_seen_at: p.last_seen_at,
      last_seen_kernel: p.last_seen_kernel,
      last_seen_payload: p.last_seen_payload,
      notes: p.notes,
    })),
    schedules_count: schedules.length,
    // Count distinct (host, title) pairs tracked across all consoles.
    play_time_titles: Object.values(playTimesByHost).reduce(
      (n, titles) => n + Object.keys(titles).length,
      0,
    ),
    // Activity is stored newest-first (activityHistory prepends), so the
    // NEWEST 50 are slice(0, 50) — slice(-50) grabbed the OLDEST 50,
    // defeating the point of a "recent activity" diagnostic field.
    recent_activity: activity.slice(0, 50).map((e) => ({
      id: e.id,
      label: e.label,
      outcome: e.outcome,
      started_at: e.startedAtMs,
      ended_at: e.endedAtMs,
    })),
    recent_notifications: notifications.slice(0, 30).map((n) => ({
      id: n.id,
      level: n.level,
      title: n.title,
      body: n.body,
      ts: n.ts,
    })),
    recent_logs: logs.slice(-(opts.logLimit ?? 100)).map((l) => ({
      level: l.level,
      source: l.source,
      message: l.message,
      timestamp: l.timestamp,
    })),
  };
}
