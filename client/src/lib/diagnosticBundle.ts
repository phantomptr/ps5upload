import { useActivityHistoryStore } from "../state/activityHistory";
import { useNotificationsStore } from "../state/notifications";
import { useLogsStore } from "../state/logs";
import { useRosterStore } from "../state/roster";
import { useScheduleStore } from "../state/schedules";
import { useConnectionStore } from "../state/connection";
import { useInstallSettingsStore } from "../state/installSettings";
import { usePlayTimeStore } from "../state/playTime";
import { useUploadSettingsStore } from "../state/uploadSettings";
import { useDiagSettingsStore } from "../state/diagSettings";
import { useThemeStore } from "../state/theme";
import { useEditSessionStore } from "../state/editSession";

/**
 * Build a single-file diagnostic bundle for sharing on bug reports.
 *
 * Format: a JSON document. Single-file > zip because:
 *   - Maintainers can paste-inspect without an extraction step
 *   - No new dependency (we don't bundle a zip writer)
 *   - Schema-evolves cleanly — adding a field doesn't break old reports
 *
 * Privacy:
 *   - Host IPs are fully redacted by default (`<IPv4>`).
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
  /** App vs. on-console helper versions, and whether they disagree.
   *
   *  Called out as its own field because a stale helper is the single most
   *  common cause of "this fix didn't work for me" reports: the app looks
   *  healthy while the console still runs an older payload, so a shipped fix
   *  genuinely never executes. This cost a full misdiagnosis round — a cover
   *  fix released in 5.6.0 was reported as a regression by someone running a
   *  5.7.1 helper against a 5.5.0 app, i.e. a build that never contained it.
   *  `helper_mismatch` makes that readable at a glance instead of requiring
   *  the triager to eyeball two version strings. */
  versions: {
    app: string;
    payload: string | null;
    helper_mismatch: boolean | null;
  };
  /** Host clock, so PS5-side log timestamps can be lined up against
   *  app-side ones. Correlating the two by hand ate real time during a
   *  focus-drop investigation; the offset makes it mechanical. */
  clock: {
    host_iso: string;
    timezone: string | null;
    utc_offset_min: number;
  };
  /** Open ShadowMount+ edit checkout, if any. While one is open the image is
   *  deliberately moved OUT of SMP's scan roots, which changes mount and
   *  registration behaviour — so a report filed mid-checkout looks broken in
   *  ways that are actually expected. */
  edit_session: {
    active: boolean;
    original_path: string;
    staged_path: string;
    mount_point: string;
    title_id: string;
    started_at_ms: number;
  } | null;
  /** Cheap triage counters over the retained log/activity buffers, so a
   *  maintainer sees "17 errors" without reading every line. */
  counters: {
    error_logs: number;
    warn_logs: number;
    failed_activity: number;
    total_logs_retained: number;
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
  /** User settings snapshot — the behaviour toggles. Essential for "the tool
   *  did X even though I had Y off" reports: captures the ACTUAL stored value
   *  so triage doesn't have to guess (a real Titanfall/Guardians report was
   *  unanswerable because these weren't captured). */
  settings: {
    auto_install_after_upload: boolean;
    auto_remove_after_install: boolean;
    auto_scan_external: boolean;
    always_overwrite: boolean;
    show_transfer_files: boolean;
    bandwidth_cap_mbps: number;
    upload_streams: number;
    auto_resume: boolean;
    auto_redeploy_on_wake: boolean;
    system_file_read: boolean;
    log_level: string;
    theme: string;
  };
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
 * Exported for testing. Hides the complete IPv4 address;
 * preserves IPv6/hostnames as a length-only placeholder.
 */
export function redactHost(host: string | null | undefined, redact: boolean): string {
  if (!host) return "";
  if (!redact) return host;
  // Do not retain the LAN prefix: a /16 still identifies a local network.
  const m = host.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
  if (m) return "<IPv4>";
  // IPv6 / hostname: hash by length only — preserves shape without
  // revealing the actual address.
  return `<host:${host.length}-char>`;
}

/**
 * Redact addresses embedded in free-form diagnostic text.
 *
 * Structured host fields go through `redactHost`, but bug-report logs also
 * contain addresses inside error strings (`connect 192.168.1.50:9021`, for
 * example). Those files used to be copied verbatim, so checking "Redact IPs"
 * only protected report fields and not the logs beside them.
 */
export function redactDiagnosticText(text: string, redact: boolean): string {
  if (!redact || !text) return text;

  // Avoid lookbehind so this remains compatible with older Android WebViews.
  // Redact the entire address, including its network prefix.
  const ipv4 = text.replace(
    /(^|[^0-9.])(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})(?=$|[^0-9.])/g,
    (_whole, prefix: string) => `${prefix}<IPv4>`,
  );

  // Socket errors render IPv6 hosts in brackets. Redact the host while
  // retaining a following port, e.g. [fe80::1]:9021 -> [<IPv6>]:9021.
  return ipv4.replace(/\[[0-9a-f:.]*:[0-9a-f:.]+\]/gi, "[<IPv6>]");
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
  const installSettings = useInstallSettingsStore.getState();
  const roster = useRosterStore.getState().profiles;
  const schedules = useScheduleStore.getState().schedules;
  const playTimesByHost = usePlayTimeStore.getState().byHost;
  const activity = useActivityHistoryStore.getState().entries;
  const notifications = useNotificationsStore.getState().entries;
  const logs = useLogsStore.getState().entries;
  const uploadSettings = useUploadSettingsStore.getState();
  const diagSettings = useDiagSettingsStore.getState();
  const theme = useThemeStore.getState();
  const editSession = useEditSessionStore.getState();

  const nav: any = typeof navigator !== "undefined" ? navigator : {};
  const mem: any =
    typeof performance !== "undefined" ? (performance as any).memory : undefined;
  const toMb = (b: number | undefined) =>
    typeof b === "number" ? Math.round(b / 1048576) : null;

  return {
    schema: 3,
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
    versions: {
      app: opts.appVersion,
      payload: conn.payloadVersion,
      // null (not false) when there is no helper to compare against, so
      // "no console connected" never reads as "versions agree".
      helper_mismatch: conn.payloadVersion
        ? conn.payloadVersion !== opts.appVersion
        : null,
    },
    clock: {
      host_iso: new Date().toISOString(),
      timezone:
        typeof Intl !== "undefined"
          ? (Intl.DateTimeFormat().resolvedOptions().timeZone ?? null)
          : null,
      // Negated so the sign reads the conventional way: UTC+2 -> +120.
      utc_offset_min: -new Date().getTimezoneOffset(),
    },
    edit_session: (() => {
      const slot = conn.host ? editSession.byHost?.[conn.host] : null;
      const co = slot?.checkout ?? null;
      if (!co) return null;
      return {
        active: true,
        original_path: co.original_path,
        staged_path: co.staged_path,
        mount_point: co.mount_point,
        title_id: co.title_id,
        started_at_ms: co.started_at_ms,
      };
    })(),
    counters: {
      error_logs: logs.filter((l) => l.level === "error").length,
      warn_logs: logs.filter((l) => l.level === "warn").length,
      failed_activity: activity.filter((e) => e.outcome === "failed").length,
      total_logs_retained: logs.length,
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
    settings: {
      auto_install_after_upload: installSettings.autoInstallAfterUpload,
      auto_remove_after_install: installSettings.autoRemoveAfterInstall,
      auto_scan_external: installSettings.autoScanExternal,
      always_overwrite: uploadSettings.alwaysOverwrite,
      show_transfer_files: uploadSettings.showTransferFiles,
      bandwidth_cap_mbps: uploadSettings.bandwidthCapMbps,
      upload_streams: uploadSettings.uploadStreams,
      auto_resume: uploadSettings.autoResume,
      auto_redeploy_on_wake: uploadSettings.autoRedeployOnWake,
      system_file_read: uploadSettings.systemFileRead,
      log_level: diagSettings.logLevel,
      theme: theme.theme,
    },
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
