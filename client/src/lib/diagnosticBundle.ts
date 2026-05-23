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
}): DiagnosticBundle {
  const conn = useConnectionStore.getState();
  const roster = useRosterStore.getState().profiles;
  const schedules = useScheduleStore.getState().schedules;
  const playTimes = usePlayTimeStore.getState().seconds;
  const activity = useActivityHistoryStore.getState().entries;
  const notifications = useNotificationsStore.getState().entries;
  const logs = useLogsStore.getState().entries;

  return {
    schema: 1,
    generated_at: new Date().toISOString(),
    app_version: opts.appVersion,
    user_agent: typeof navigator !== "undefined" ? navigator.userAgent : "n/a",
    redacted: opts.redact,
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
    play_time_titles: Object.keys(playTimes).length,
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
    recent_logs: logs.slice(-100).map((l) => ({
      level: l.level,
      source: l.source,
      message: l.message,
      timestamp: l.timestamp,
    })),
  };
}
