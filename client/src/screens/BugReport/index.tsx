import { useCallback, useEffect, useState } from "react";
import { invoke } from "../../lib/invokeLogged";
import {
  Bug,
  ImagePlus,
  Trash2,
  FileArchive,
  ExternalLink,
  FolderOpen,
  Loader2,
  CheckCircle2,
  RefreshCw,
  Check,
  Camera,
} from "lucide-react";

import { PageHeader, Card, Button, ErrorCard } from "../../components";
import { useTr } from "../../state/lang";
import { useDiagSettingsStore, LOG_LEVELS } from "../../state/diagSettings";
import { useConnectionStore } from "../../state/connection";
import { buildDiagnosticBundle } from "../../lib/diagnosticBundle";
import { buildPs5Snapshot } from "../../lib/ps5Snapshot";
import { ensurePayloadCurrent } from "../../lib/ensurePayloadCurrent";
import { hostOf } from "../../lib/addr";
import { flushDiskLogNow } from "../../state/logs";
import { openReportChannel } from "../../lib/reportProblem";
import type { SavedShot } from "../../lib/captureScreenshot";
import type { LogLevel } from "../../state/logs";

/** Time windows offered for the log slice, in minutes. Mirrors the user's
 *  request: last 1 / 5 / 10 / 30 / 60 / 120 minutes. */
const WINDOWS: number[] = [1, 5, 10, 30, 60, 120];

interface IncludeFlags {
  app_logs: boolean;
  engine_log: boolean;
  crash_reports: boolean;
  ps5_logs: boolean;
  images: boolean;
}

interface AttachedImage {
  path: string;
  name: string;
}

interface BuildResult {
  entries: number;
  bytes: number;
  dest: string;
  log_lines: number;
  crash_reports: number;
  images: number;
}

/** `YYYYMMDD-HHMMSS` in local time, for a human-readable zip filename. */
function stamp(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return (
    `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}` +
    `-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`
  );
}

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

export default function BugReportScreen() {
  const tr = useTr();
  const logLevel = useDiagSettingsStore((s) => s.logLevel);
  const setLogLevel = useDiagSettingsStore((s) => s.setLogLevel);
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const connected = !!host?.trim() && payloadStatus === "up";

  const [description, setDescription] = useState("");
  const [images, setImages] = useState<AttachedImage[]>([]);
  // Captured-screenshot gallery (from the global capture button → disk).
  const [shots, setShots] = useState<SavedShot[]>([]);
  const [selectedShots, setSelectedShots] = useState<Set<string>>(new Set());
  const [windowMinutes, setWindowMinutes] = useState(30);
  const [redact, setRedact] = useState(true);
  const [include, setInclude] = useState<IncludeFlags>({
    app_logs: true,
    engine_log: true,
    crash_reports: true,
    ps5_logs: true,
    images: true,
  });

  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<BuildResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reconnecting, setReconnecting] = useState(false);

  // Re-deploy the helper so the PS5 snapshot + kernel log can be captured.
  // Crucially, the PS5 kernel log lives in the kernel's circular buffer
  // (kern.msgbuf), which SURVIVES a helper crash — so reconnecting here lets
  // the report capture the log from BEFORE/DURING the crash, as long as the
  // console hasn't been rebooted. (payloadStatus flips to "up" via the poller,
  // which re-enables the PS5-snapshot checkbox.)
  const reconnectHelper = useCallback(async () => {
    if (!host?.trim() || reconnecting) return;
    setReconnecting(true);
    try {
      await ensurePayloadCurrent(hostOf(host), undefined, true);
    } catch {
      /* best-effort; the poller surfaces the resulting status */
    } finally {
      setReconnecting(false);
    }
  }, [host, reconnecting]);

  // Live summary numbers.
  const [logStats, setLogStats] = useState<{ count: number; bytes: number } | null>(
    null,
  );
  const [crashCount, setCrashCount] = useState<number | null>(null);

  const refreshStats = useCallback(async () => {
    try {
      const s = await invoke<{ count: number; bytes: number }>("diag_log_stats");
      setLogStats({ count: s.count, bytes: s.bytes });
    } catch {
      setLogStats(null);
    }
    try {
      const s = await invoke<{ count: number }>("crash_reports_stats");
      setCrashCount(s.count);
    } catch {
      setCrashCount(null);
    }
  }, []);

  useEffect(() => {
    void refreshStats();
  }, [refreshStats]);

  // Load the saved-screenshot gallery on mount (captures happen elsewhere via
  // the status-bar button, so re-read whenever the page is opened). New shots
  // default to selected.
  const refreshShots = useCallback(async () => {
    try {
      const list = await invoke<SavedShot[]>("screenshot_list", { limit: 24 });
      setShots(list);
      setSelectedShots(new Set(list.map((s) => s.name)));
    } catch {
      setShots([]);
    }
  }, []);
  useEffect(() => {
    void refreshShots();
  }, [refreshShots]);

  const toggleShot = (name: string) =>
    setSelectedShots((cur) => {
      const next = new Set(cur);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });

  async function deleteShot(name: string) {
    try {
      await invoke("screenshot_delete", { name });
    } catch {
      /* best-effort */
    }
    void refreshShots();
  }

  const toggle = (k: keyof IncludeFlags) =>
    setInclude((f) => ({ ...f, [k]: !f[k] }));

  async function attachImages() {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const picked = await open({
        multiple: true,
        filters: [
          { name: "Images", extensions: ["png", "jpg", "jpeg", "webp", "gif", "bmp"] },
        ],
      });
      if (!picked) return;
      const paths = Array.isArray(picked) ? picked : [picked];
      const next: AttachedImage[] = paths.map((p) => ({
        path: p,
        name: p.split(/[\\/]/).pop() || p,
      }));
      // De-dupe by path so re-picking the same file doesn't double it.
      setImages((cur) => {
        const seen = new Set(cur.map((i) => i.path));
        return [...cur, ...next.filter((i) => !seen.has(i.path))];
      });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  function removeImage(path: string) {
    setImages((cur) => cur.filter((i) => i.path !== path));
  }

  async function createReport() {
    setBusy(true);
    setError(null);
    setResult(null);
    try {
      // Make sure the buffered tail of the log is on disk before we window it.
      await flushDiskLogNow();

      const { getVersion } = await import("@tauri-apps/api/app");
      const appVersion = await getVersion().catch(() => "unknown");

      const bundle = buildDiagnosticBundle({
        appVersion,
        redact,
        logLimit: 1000,
      });
      const {
        snapshot,
        klog,
        syslog,
        payload_logs: payloadLogs,
      } = await buildPs5Snapshot({ redact });

      const manifest = {
        schema: 1,
        kind: "ps5upload-bug-report",
        generated_at: new Date().toISOString(),
        app_version: appVersion,
        platform:
          typeof navigator !== "undefined" ? navigator.platform : "unknown",
        user_agent:
          typeof navigator !== "undefined" ? navigator.userAgent : "unknown",
        description: description.trim(),
        log_level: logLevel,
        window_minutes: windowMinutes,
        redacted: redact,
        includes: include,
        diagnostic: bundle,
        ps5: snapshot,
      };

      const { save } = await import("@tauri-apps/plugin-dialog");
      const destFilename = `ps5upload-bugreport-${stamp(new Date())}.zip`;
      const dest = await save({
        defaultPath: destFilename,
        filters: [{ name: "Zip", extensions: ["zip"] }],
      });
      if (!dest || typeof dest !== "string") {
        setBusy(false);
        return;
      }

      const res = await invoke<BuildResult>("bug_report_build", {
        args: {
          dest,
          // On Android `dest` is a content:// SAF URI std::fs can't write to;
          // the backend redirects to Downloads using this name and returns the
          // real path it wrote (shown in the success summary).
          dest_filename: destFilename,
          report_json: JSON.stringify(manifest, null, 2),
          window_minutes: windowMinutes,
          klog_text: include.ps5_logs ? klog : null,
          syslog_text: include.ps5_logs ? syslog : null,
          payload_logs: include.ps5_logs ? payloadLogs : [],
          image_paths: include.images
            ? [
                ...shots
                  .filter((s) => selectedShots.has(s.name))
                  .map((s) => s.path),
                ...images.map((i) => i.path),
              ]
            : [],
          include,
        },
      });
      setResult(res);
      void refreshStats();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function openLogsFolder() {
    try {
      await invoke("diag_log_open_dir");
    } catch {
      // best-effort
    }
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <PageHeader
        icon={Bug}
        title={tr("bug_report_page_title", undefined, "Bug report")}
        description={tr(
          "bug_report_page_desc",
          undefined,
          "Describe what went wrong, attach screenshots, and package logs + a PS5 snapshot into one .zip to post on Discord.",
        )}
      />

      <div className="grid min-h-0 flex-1 gap-4 overflow-y-auto pb-6 md:grid-cols-2">
        {/* ── What happened ── */}
        <Card
          title={tr("bug_report_describe_title", undefined, "What happened?")}
          icon={Bug}
        >
          <textarea
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            rows={6}
            placeholder={tr(
              "bug_report_describe_ph",
              undefined,
              "Steps to reproduce, what you expected, what happened instead. Firmware, jailbreak/loader, file sizes — anything that helps.",
            )}
            className="w-full resize-y rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] p-3 text-sm outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]"
          />

          {/* Screenshots gallery */}
          <div className="mt-4">
            <div className="mb-1 flex items-center justify-between gap-2">
              <span className="text-sm font-medium">
                {tr("bug_report_images_title", undefined, "Screenshots")}
              </span>
              <div className="flex items-center gap-1.5">
                <Button
                  variant="ghost"
                  size="sm"
                  leftIcon={<RefreshCw size={12} />}
                  onClick={() => void refreshShots()}
                >
                  {tr("refresh", undefined, "Refresh")}
                </Button>
                <Button
                  variant="secondary"
                  size="sm"
                  leftIcon={<ImagePlus size={12} />}
                  onClick={attachImages}
                >
                  {tr("bug_report_images_add", undefined, "Attach file")}
                </Button>
              </div>
            </div>
            <p className="mb-2 flex items-center gap-1 text-xs text-[var(--color-muted)]">
              <Camera size={12} />
              {tr(
                "bug_report_capture_hint",
                undefined,
                "Use the camera button in the status bar (bottom) to capture any screen — then tick which to include.",
              )}
            </p>

            {shots.length === 0 ? (
              <p className="text-xs text-[var(--color-muted)]">
                {tr(
                  "bug_report_shots_none",
                  undefined,
                  "No screenshots captured yet.",
                )}
              </p>
            ) : (
              <div className="grid grid-cols-3 gap-2">
                {shots.map((s) => {
                  const sel = selectedShots.has(s.name);
                  return (
                    <div key={s.name} className="group relative">
                      <button
                        type="button"
                        onClick={() => toggleShot(s.name)}
                        className={`block w-full overflow-hidden rounded-lg border-2 ${
                          sel
                            ? "border-[var(--color-accent)]"
                            : "border-[var(--color-border)] opacity-60"
                        }`}
                        title={s.name}
                      >
                        <img
                          src={s.data_url}
                          alt={s.name}
                          className="h-16 w-full bg-[var(--color-surface)] object-cover"
                        />
                        {sel && (
                          <span className="absolute left-1 top-1 rounded-full bg-[var(--color-accent)] p-0.5 text-[var(--color-accent-contrast)]">
                            <Check size={10} />
                          </span>
                        )}
                      </button>
                      <button
                        type="button"
                        onClick={() => void deleteShot(s.name)}
                        className="absolute right-1 top-1 rounded bg-[var(--color-surface-2)]/80 p-0.5 text-[var(--color-muted)] opacity-0 transition-opacity hover:text-[var(--color-bad)] group-hover:opacity-100"
                        aria-label={tr("remove", undefined, "Remove")}
                      >
                        <Trash2 size={11} />
                      </button>
                    </div>
                  );
                })}
              </div>
            )}

            {/* External (file-picked) attachments */}
            {images.length > 0 && (
              <ul className="mt-2 space-y-1">
                {images.map((img) => (
                  <li
                    key={img.path}
                    className="flex items-center justify-between gap-2 rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-xs"
                  >
                    <span className="min-w-0 flex-1 truncate font-mono">
                      {img.name}
                    </span>
                    <button
                      type="button"
                      onClick={() => removeImage(img.path)}
                      className="shrink-0 text-[var(--color-muted)] hover:text-[var(--color-bad)]"
                      aria-label={tr("remove", undefined, "Remove")}
                    >
                      <Trash2 size={12} />
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </Card>

        {/* ── What to include ── */}
        <Card
          title={tr("bug_report_include_title", undefined, "What to include")}
          icon={FileArchive}
        >
          {/* Log window + level */}
          <div className="grid grid-cols-2 gap-3">
            <label className="text-xs">
              <span className="mb-1 block text-[var(--color-muted)]">
                {tr("bug_report_window", undefined, "Log time window")}
              </span>
              <select
                value={windowMinutes}
                onChange={(e) => setWindowMinutes(Number(e.target.value))}
                className="w-full rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1.5 text-sm"
              >
                {WINDOWS.map((m) => (
                  <option key={m} value={m}>
                    {tr("bug_report_window_last", { n: m }, `Last ${m} min`)}
                  </option>
                ))}
              </select>
            </label>
            <label className="text-xs">
              <span className="mb-1 block text-[var(--color-muted)]">
                {tr("bug_report_level", undefined, "Recording level")}
              </span>
              <select
                value={logLevel}
                onChange={(e) => setLogLevel(e.target.value as LogLevel)}
                className="w-full rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1.5 text-sm"
              >
                {LOG_LEVELS.map((l) => (
                  <option key={l} value={l}>
                    {tr(`log_level_${l}`, undefined, l[0].toUpperCase() + l.slice(1))}
                  </option>
                ))}
              </select>
            </label>
          </div>
          <p className="mt-1.5 text-xs text-[var(--color-muted)]">
            {tr(
              "bug_report_level_hint",
              undefined,
              "Hard to reproduce? Set the level to debug or trace, reproduce the issue, then create the report.",
            )}
          </p>

          {/* Section toggles */}
          <div className="mt-4 space-y-2">
            <IncludeRow
              label={tr("bug_report_inc_app", undefined, "App + engine logs")}
              detail={
                logStats
                  ? tr(
                      "bug_report_inc_app_n",
                      { n: logStats.count, size: fmtBytes(logStats.bytes) },
                      `${logStats.count} file(s) · ${fmtBytes(logStats.bytes)} on disk`,
                    )
                  : undefined
              }
              checked={include.app_logs}
              onChange={() => toggle("app_logs")}
            />
            <IncludeRow
              label={tr("bug_report_inc_engine", undefined, "Engine log file")}
              checked={include.engine_log}
              onChange={() => toggle("engine_log")}
            />
            <IncludeRow
              label={tr("bug_report_inc_crash", undefined, "Crash & error reports")}
              detail={
                crashCount != null
                  ? tr(
                      "bug_report_inc_crash_n",
                      { n: crashCount },
                      `${crashCount} report(s)`,
                    )
                  : undefined
              }
              checked={include.crash_reports}
              onChange={() => toggle("crash_reports")}
            />
            <IncludeRow
              label={tr("bug_report_inc_ps5", undefined, "PS5 snapshot + kernel log")}
              detail={
                connected
                  ? tr("bug_report_inc_ps5_on", undefined, "PS5 connected")
                  : tr("bug_report_inc_ps5_off", undefined, "No PS5 connected — skipped")
              }
              checked={include.ps5_logs}
              disabled={!connected}
              onChange={() => toggle("ps5_logs")}
            />
            <IncludeRow
              label={tr("bug_report_inc_images", undefined, "Screenshots & images")}
              detail={tr(
                "bug_report_inc_images_n",
                { n: selectedShots.size + images.length },
                `${selectedShots.size + images.length} selected`,
              )}
              checked={include.images}
              disabled={selectedShots.size + images.length === 0}
              onChange={() => toggle("images")}
            />
          </div>

          {/* Helper-down recovery hint. The kernel log is the most useful thing
              after a helper crash, yet it can't be fetched without a live
              helper. But the PS5 kernel buffer (kern.msgbuf) SURVIVES the
              crash, so reconnecting recovers the crash log — make that
              discoverable instead of just greying the checkbox out. */}
          {!connected && !!host?.trim() && (
            <div className="mt-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 text-xs text-[var(--color-muted)]">
              <p className="mb-2">
                {tr(
                  "bug_report_klog_recover",
                  undefined,
                  "The PS5 kernel log lives in the console's kernel buffer, which survives a helper crash — so reconnecting the helper recovers the log from the crash. Grab it before rebooting the PS5 (a reboot clears it).",
                )}
              </p>
              <Button
                size="sm"
                variant="secondary"
                loading={reconnecting}
                onClick={() => void reconnectHelper()}
              >
                {tr("bug_report_reconnect_helper", undefined, "Reconnect helper")}
              </Button>
            </div>
          )}

          {/* Privacy */}
          <label className="mt-4 flex items-start gap-2 text-xs">
            <input
              type="checkbox"
              checked={redact}
              onChange={(e) => setRedact(e.target.checked)}
              className="mt-0.5"
            />
            <span>
              <span className="font-medium">
                {tr("bug_report_redact", undefined, "Redact IPs & serial")}
              </span>
              <span className="block text-[var(--color-muted)]">
                {tr(
                  "bug_report_redact_hint",
                  undefined,
                  "Strip your PS5's IP address and serial number so the zip is safe to post publicly.",
                )}
              </span>
            </span>
          </label>

          <div className="mt-4 flex items-center gap-2">
            <Button
              variant="primary"
              leftIcon={
                busy ? <Loader2 size={14} className="animate-spin" /> : <FileArchive size={14} />
              }
              onClick={createReport}
              disabled={busy}
            >
              {busy
                ? tr("bug_report_building", undefined, "Building…")
                : tr("bug_report_create", undefined, "Create bug report (.zip)")}
            </Button>
            <button
              type="button"
              onClick={openLogsFolder}
              className="text-xs text-[var(--color-accent)] hover:underline"
            >
              {tr("logs_open_folder", undefined, "Open logs folder")}
            </button>
          </div>
        </Card>

        {/* ── Result / errors span both columns ── */}
        {error && (
          <div className="md:col-span-2">
            <ErrorCard
              title={tr("bug_report_failed", undefined, "Couldn't build the report")}
              detail={error}
            />
          </div>
        )}
        {result && (
          <div className="md:col-span-2">
            <Card
              title={tr("bug_report_ready_title", undefined, "Bug report ready")}
              icon={CheckCircle2}
              accent
            >
              <p className="text-sm">
                {tr(
                  "bug_report_ready_summary",
                  {
                    entries: result.entries,
                    size: fmtBytes(result.bytes),
                    lines: result.log_lines,
                  },
                  `${result.entries} files · ${fmtBytes(result.bytes)} · ${result.log_lines} log lines`,
                )}
              </p>
              <p className="mt-1 break-all font-mono text-xs text-[var(--color-muted)]">
                {result.dest}
              </p>
              <p className="mt-3 text-sm">
                {tr(
                  "bug_report_post_hint",
                  undefined,
                  "Post this .zip in the #bugs-report channel on Discord, with a short description.",
                )}
              </p>
              <div className="mt-3 flex flex-wrap items-center gap-2">
                <Button
                  variant="primary"
                  size="sm"
                  leftIcon={<ExternalLink size={12} />}
                  onClick={() => void openReportChannel()}
                >
                  {tr("bug_report_open_discord", undefined, "Open Discord channel")}
                </Button>
                <Button
                  variant="secondary"
                  size="sm"
                  leftIcon={<FolderOpen size={12} />}
                  onClick={() => void openLogsFolder()}
                >
                  {tr("bug_report_open_folder", undefined, "Open logs folder")}
                </Button>
              </div>
            </Card>
          </div>
        )}
      </div>
    </div>
  );
}

function IncludeRow({
  label,
  detail,
  checked,
  disabled,
  onChange,
}: {
  label: string;
  detail?: string;
  checked: boolean;
  disabled?: boolean;
  onChange: () => void;
}) {
  return (
    <label
      className={`flex items-center justify-between gap-3 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 ${
        disabled ? "opacity-50" : "cursor-pointer"
      }`}
    >
      <span className="min-w-0">
        <span className="block text-sm">{label}</span>
        {detail && (
          <span className="block text-xs text-[var(--color-muted)]">{detail}</span>
        )}
      </span>
      <input
        type="checkbox"
        checked={checked && !disabled}
        disabled={disabled}
        onChange={onChange}
        className="shrink-0"
      />
    </label>
  );
}
