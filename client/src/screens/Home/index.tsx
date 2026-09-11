import { useMemo } from "react";
import PowerControl from "../Connection/PowerControl";
import { Link } from "react-router";
import {
  Activity as ActivityIcon,
  ArrowRight,
  Bell,
  Cable,
  CheckCircle2,
  Cpu,
  FolderTree,
  Gamepad2,
  LayoutDashboard,
  PackageOpen,
  Power,
  Save,
  Server,
  Upload,
  WifiOff,
  XCircle,
  type LucideIcon,
} from "lucide-react";

import { useConnectionStore } from "../../state/connection";
import { useActivityHistoryStore } from "../../state/activityHistory";
import { useNotificationsStore } from "../../state/notifications";
import { useRunningAppsStore } from "../../state/runningApps";
import { useSensors } from "../../state/sensors";
import { Badge, Card, ConsoleChip, Sparkline, Spinner } from "../../components";
import { useTr } from "../../state/lang";
import {
  evaluateOperationReadiness,
  type Operation,
} from "../../lib/operationReadiness";

/**
 * Product-level command center. The hierarchy is deliberate:
 *  1. Can I use the console right now, and what should I do next?
 *  2. What common operation do I want to start?
 *  3. What is the console reporting, and what just happened?
 */
export default function HomeScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const engineStatus = useConnectionStore((s) => s.engineStatus);
  const payloadVersion = useConnectionStore((s) => s.payloadVersion);
  const ps5Kernel = useConnectionStore((s) => s.ps5Kernel);
  const ucredElevated = useConnectionStore((s) => s.ucredElevated);
  const { sample: sensorSample, history } = useSensors(host);
  const temps = sensorSample?.temps ?? null;
  const power = sensorSample?.power ?? null;

  const allActivity = useActivityHistoryStore((s) => s.entries);
  const allNotifs = useNotificationsStore((s) => s.entries);
  const recentActivity = useMemo(
    () => allActivity.slice(-5).reverse(),
    [allActivity],
  );
  const recentNotifs = useMemo(() => allNotifs.slice(0, 5), [allNotifs]);
  const runningTitleIds = useRunningAppsStore((s) => s.titleIds);
  const cpuHistory = useMemo(
    () =>
      history
        .flatMap((sample) =>
          sample.temps && sample.temps.cpu_temp > 0
            ? [sample.temps.cpu_temp]
            : [],
        )
        .slice(-30),
    [history],
  );

  const connected = engineStatus === "up" && payloadStatus === "up";
  const readinessContext = {
    host,
    engineUp: engineStatus === "up",
    helperUp: payloadStatus === "up",
    kernelRw: ucredElevated,
  };
  const readinessFor = (operation: Operation) =>
    evaluateOperationReadiness(operation, readinessContext);

  return (
    <div className="app-page">
      <header className="mb-6">
        <div className="page-kicker">
          <LayoutDashboard size={13} aria-hidden />
          {tr("v5_home_command_center", "Command center")}
        </div>
        <div className="flex flex-col gap-2 sm:flex-row sm:items-end sm:justify-between">
          <div>
            <h1 className="text-[1.75rem] font-bold tracking-[-0.035em]">
              {tr("v5_home_title", "Home")}
            </h1>
            <p className="mt-1 max-w-2xl text-sm text-[var(--color-muted)]">
              {tr(
                "v5_home_subtitle",
                "Manage your console, move content, and monitor active work.",
              )}
            </p>
          </div>
          <Badge tone={connected ? "good" : "warn"} size="md" dot className="self-start">
            {connected
              ? tr("v5_home_connected", "Connected")
              : tr("v5_home_setup_required", "Setup required")}
          </Badge>
        </div>
      </header>

      <section
        className={`dashboard-hero mb-4 ${connected ? "is-connected" : "is-offline"}`}
      >
        <span className="dashboard-hero-icon">
          {connected ? <Cable size={21} /> : <WifiOff size={21} />}
        </span>
        <div className="min-w-0 flex-1">
          <h2 className="text-base font-semibold tracking-tight">
            {connected
              ? tr("v5_home_console_ready", "Your PS5 is ready")
              : tr("v5_home_connect_title", "Connect a PS5 to get started")}
          </h2>
          <p className="mt-0.5 text-xs leading-relaxed text-[var(--color-muted)] sm:text-sm">
            {connected
              ? tr(
                  "v5_home_console_ready_desc",
                  `Helper connected${host ? ` at ${host}` : ""}. Console operations are available.`,
                )
              : tr(
                  "payload_not_connected_message",
                  "Add your console address and send the helper once. We’ll verify every capability before enabling console actions.",
                )}
          </p>
        </div>
        <Link
          to="/connection"
          className="inline-flex min-h-10 shrink-0 items-center justify-center gap-2 rounded-[var(--radius-control)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] px-3.5 text-xs font-semibold shadow-sm transition-colors hover:border-[var(--color-border-strong)] hover:bg-[var(--color-surface-3)]"
        >
          {connected
            ? tr("v5_home_manage_connection", "Manage connection")
            : tr("v5_home_connect", "Connect PS5")}
          <ArrowRight size={14} aria-hidden />
        </Link>
      </section>

      {/* Power, right under the hero — issue #316. These lived only at the
          bottom of Manage connections, which is several clicks away from the
          screen people actually sit on. Same component as that page rather
          than a copy, so the confirmations and the Wake button cannot drift
          apart between the two places.

          Shown whenever a host is configured, not only when connected: Wake
          is precisely the action you want when the console is NOT reachable,
          and hiding the panel then would remove it at the one moment it is
          the point. */}
      {host ? (
        <div className="mb-4">
          <PowerControl host={host} />
        </div>
      ) : null}

      <div className="grid gap-4 xl:grid-cols-12">
        <Card className="xl:col-span-7">
          <SectionHeading
            icon={ActivityIcon}
            title={tr("v5_home_quick_actions", "Quick actions")}
            description={tr(
              "v5_home_quick_actions_desc",
              "Start the jobs you use most.",
            )}
          />
          <div className="grid gap-2 sm:grid-cols-2">
            <QuickAction to="/upload" icon={Upload} label={tr("v5_qa_upload", "Upload files")} readiness={readinessFor("upload")} />
            <QuickAction to="/install-package" icon={PackageOpen} label={tr("v5_qa_install", "Install package")} readiness={readinessFor("install-package")} />
            <QuickAction to="/files" icon={FolderTree} label={tr("v5_qa_files", "Browse files")} readiness={readinessFor("browse-console")} />
            <QuickAction to="/games" icon={Gamepad2} label={tr("v5_qa_games", "Open library")} readiness={readinessFor("browse-console")} />
            <QuickAction to="/saves" icon={Save} label={tr("v5_qa_saves", "Back up saves")} readiness={readinessFor("browse-console")} />
            <QuickAction to="/ftp-server" icon={Server} label={tr("v5_qa_ftp", "Start FTP server")} readiness={readinessFor("manage-system")} />
          </div>
        </Card>

        <Card className="xl:col-span-5">
          <SectionHeading
            icon={Cpu}
            title={tr("v5_home_console_status", "Console status")}
            description={tr(
              "v5_home_console_status_desc",
              "Connection and live system health.",
            )}
          />
          <div>
            <MetricRow label={tr("v5_home_host", "Host")} value={host || "—"} />
            <MetricRow label={tr("v5_home_engine", "Local engine")} value={engineStatus} tone={engineStatus === "up" ? "good" : "bad"} />
            <MetricRow label={tr("v5_home_helper", "PS5 helper")} value={payloadVersion ? `v${payloadVersion}` : payloadStatus} tone={payloadStatus === "up" ? "good" : "warn"} />
            <MetricRow
              label={tr("v5_home_krw", "Kernel access")}
              value={ucredElevated === null ? "—" : ucredElevated ? tr("v5_home_available", "Available") : tr("v5_home_missing", "Unavailable")}
              tone={ucredElevated === null ? undefined : ucredElevated ? "good" : "warn"}
            />
          </div>

          <div className="mt-4 rounded-xl border border-[var(--color-border)] bg-[color-mix(in_oklab,var(--color-surface)_45%,transparent)] p-3">
            <div className="mb-3 flex items-center justify-between gap-3">
              <div>
                <div className="text-xs font-semibold">{tr("v5_home_sensors", "Live sensors")}</div>
                <div className="mt-0.5 text-[0.6875rem] text-[var(--color-muted)]">
                  {connected ? tr("v5_home_sensor_live", "Updates automatically") : tr("v5_home_sensor_waiting", "Available after connection")}
                </div>
              </div>
              {temps && cpuHistory.length >= 2 && (
                <Sparkline
                  data={cpuHistory}
                  width={82}
                  height={26}
                  color={(temps.cpu_temp ?? 0) >= 85 ? "var(--color-warn)" : "var(--color-accent)"}
                  fill
                />
              )}
            </div>
            {temps ? (
              <div className="grid grid-cols-2 gap-2">
                <SensorMetric label={tr("v5_home_cpu", "CPU")} value={`${temps.cpu_temp?.toFixed(0) ?? "?"}°C`} />
                <SensorMetric label={tr("v5_home_soc", "SoC")} value={`${temps.soc_temp?.toFixed(0) ?? "?"}°C`} />
                {temps.m2_temp > 0 && <SensorMetric label={tr("v5_home_m2", "M.2 SSD")} value={`${temps.m2_temp.toFixed(0)}°C`} />}
                {power && <SensorMetric label={tr("v5_home_lifetime", "Runtime")} value={`${power.operating_time_hours ?? 0}h`} />}
              </div>
            ) : connected ? (
              <div className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
                <Spinner size={13} />
                {tr("v5_home_loading_sensors", "Reading sensors…")}
              </div>
            ) : (
              <div className="text-xs text-[var(--color-muted)]">
                {tr("telemetry_not_connected_desc", "Connect to see temperatures and runtime.")}
              </div>
            )}
          </div>

          {ps5Kernel && (
            <p className="mt-3 truncate font-mono text-[0.6875rem] text-[var(--color-muted)]" title={ps5Kernel}>{ps5Kernel}</p>
          )}
          {runningTitleIds.size > 0 && (
            <div className="mt-3 flex items-center gap-2 text-xs">
              <Power size={13} className="text-[var(--color-good)]" />
              <span className="font-medium">{tr("v5_home_running", { n: runningTitleIds.size }, `${runningTitleIds.size} running`)}</span>
              <span className="truncate font-mono text-[var(--color-muted)]">{Array.from(runningTitleIds).slice(0, 3).join(", ")}</span>
            </div>
          )}
        </Card>

        <Card className="xl:col-span-7">
          <SectionHeading
            icon={ActivityIcon}
            title={tr("v5_home_recent_activity", "Recent activity")}
            description={tr("v5_home_recent_activity_desc", "Latest operations across every console.")}
            action={<InlineLink to="/activity" label={tr("v5_tab_tasks", "View tasks")} />}
          />
          {recentActivity.length === 0 ? (
            <CompactEmpty icon={ActivityIcon} title={tr("v5_home_no_activity", "No activity yet")} body={tr("v5_home_no_activity_desc", "Uploads, installs, and file jobs will appear here.")} />
          ) : (
            <ul className="divide-y divide-[var(--color-border)]">
              {recentActivity.map((entry) => (
                <li key={entry.id} className="flex min-h-10 items-center gap-2 py-2 text-xs">
                  {entry.outcome === "done" ? (
                    <CheckCircle2 size={14} className="shrink-0 text-[var(--color-good)]" />
                  ) : entry.outcome === "failed" ? (
                    <XCircle size={14} className="shrink-0 text-[var(--color-bad)]" />
                  ) : entry.outcome === "running" ? (
                    <Spinner size={13} className="shrink-0" />
                  ) : (
                    <ActivityIcon size={14} className="shrink-0 text-[var(--color-muted)]" />
                  )}
                  <span className="min-w-0 flex-1 truncate font-medium">{entry.label}</span>
                  <ConsoleChip addr={entry.addr} className="shrink-0" />
                </li>
              ))}
            </ul>
          )}
        </Card>

        <Card className="xl:col-span-5">
          <SectionHeading
            icon={Bell}
            title={tr("v5_home_notifications", "Notifications")}
            description={tr("v5_home_notifications_desc", "Important results and warnings.")}
            action={<InlineLink to="/notifications" label={tr("v5_home_view_all", "View all")} />}
          />
          {recentNotifs.length === 0 ? (
            <CompactEmpty icon={Bell} title={tr("v5_home_no_notifications", "You’re all caught up")} body={tr("v5_home_no_notifications_desc", "New alerts will appear here.")} />
          ) : (
            <ul className="space-y-2">
              {recentNotifs.map((notification) => (
                <li key={notification.id} className="rounded-lg bg-[color-mix(in_oklab,var(--color-surface)_45%,transparent)] px-3 py-2 text-xs">
                  <div className="font-semibold">{notification.title}</div>
                  {notification.body && <div className="mt-0.5 line-clamp-2 text-[var(--color-muted)]">{notification.body}</div>}
                </li>
              ))}
            </ul>
          )}
        </Card>
      </div>
    </div>
  );
}

function SectionHeading({ icon: Icon, title, description, action }: { icon: LucideIcon; title: string; description: string; action?: React.ReactNode }) {
  return (
    <header className="mb-4 flex items-start gap-3">
      <span className="grid h-8 w-8 shrink-0 place-items-center rounded-[0.6rem] bg-[var(--color-surface-3)] text-[var(--color-muted)]"><Icon size={15} /></span>
      <div className="min-w-0 flex-1">
        <h2 className="text-sm font-semibold tracking-tight">{title}</h2>
        <p className="mt-0.5 text-[0.6875rem] text-[var(--color-muted)]">{description}</p>
      </div>
      {action}
    </header>
  );
}

function MetricRow({ label, value, tone }: { label: string; value: string; tone?: "good" | "warn" | "bad" }) {
  const color = tone === "good" ? "text-[var(--color-good)]" : tone === "warn" ? "text-[var(--color-warn)]" : tone === "bad" ? "text-[var(--color-bad)]" : "text-[var(--color-text)]";
  return (
    <div className="metric-row">
      <span className="text-[var(--color-muted)]">{label}</span>
      <span className={`max-w-[65%] truncate font-medium tabular-nums ${color}`} title={value}>{value}</span>
    </div>
  );
}

function SensorMetric({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-lg bg-[var(--color-surface-2)] px-2.5 py-2">
      <div className="text-[0.625rem] uppercase tracking-wide text-[var(--color-muted)]">{label}</div>
      <div className="mt-0.5 text-base font-semibold tabular-nums">{value}</div>
    </div>
  );
}

function QuickAction({ to, icon: Icon, label, readiness }: { to: string; icon: LucideIcon; label: string; readiness: { ready: boolean; blockers: string[]; warnings: string[] } }) {
  const detail = readiness.ready ? readiness.warnings[0] || "Ready" : readiness.blockers[0] || "Unavailable";
  const content = (
    <>
      <span className="action-tile-icon"><Icon size={17} aria-hidden /></span>
      <span className="min-w-0 flex-1">
        <span className="block truncate text-xs font-semibold">{label}</span>
        <span className="mt-0.5 block truncate text-[0.625rem] text-[var(--color-muted)]">{detail}</span>
      </span>
      {readiness.ready && <ArrowRight size={14} className="shrink-0 text-[var(--color-muted)]" aria-hidden />}
    </>
  );
  if (!readiness.ready) {
    return <div aria-disabled="true" title={readiness.blockers.join(" ")} className="action-tile">{content}</div>;
  }
  return <Link to={to} className="action-tile">{content}</Link>;
}

function CompactEmpty({ icon: Icon, title, body }: { icon: LucideIcon; title: string; body: string }) {
  return (
    <div className="flex min-h-24 items-center gap-3 rounded-xl border border-dashed border-[var(--color-border)] bg-[color-mix(in_oklab,var(--color-surface)_30%,transparent)] px-4 py-3">
      <span className="grid h-9 w-9 shrink-0 place-items-center rounded-full bg-[var(--color-surface-3)] text-[var(--color-muted)]"><Icon size={16} /></span>
      <div>
        <div className="text-xs font-semibold">{title}</div>
        <div className="mt-0.5 text-[0.6875rem] text-[var(--color-muted)]">{body}</div>
      </div>
    </div>
  );
}

function InlineLink({ to, label }: { to: string; label: string }) {
  return (
    <Link to={to} className="inline-flex shrink-0 items-center gap-1 text-[0.6875rem] font-semibold text-[var(--color-accent)] hover:underline">
      {label}<ArrowRight size={12} />
    </Link>
  );
}
