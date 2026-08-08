import { useMemo } from "react";
import { Link } from "react-router";
import {
  LayoutDashboard,
  Cable,
  Cpu,
  Power,
  Activity as ActivityIcon,
  Bell,
  CheckCircle2,
  XCircle,
  Upload,
  PackageOpen,
  Save,
  Server,
  FolderTree,
  Gamepad2,
  type LucideIcon,
} from "lucide-react";
import { useConnectionStore } from "../../state/connection";
import { useActivityHistoryStore } from "../../state/activityHistory";
import { useNotificationsStore } from "../../state/notifications";
import { useRunningAppsStore } from "../../state/runningApps";
import { useSensors } from "../../state/sensors";
import { Card, Badge, Spinner, ConsoleChip, Sparkline } from "../../components";
import { useTr } from "../../state/lang";

/**
 * v5 Home tab — at-a-glance dashboard with quick actions.
 *
 * Layout: responsive grid. On desktop, a 2-column layout with the
 * Connection card prominent. On mobile, single column.
 *
 * Widgets (v5 §5.2):
 *   1. Connection — engine/payload/kernel status
 *   2. Live sensors — CPU/SoC temps (when available)
 *   3. Quick actions — Upload, Install, Saves, FTP, etc.
 *   4. Recent activity — last 5 operations
 *   5. Notifications — last 5
 *
 * Data: reuses existing stores + RPCs. v5 §5.6 eventually replaces
 * the 5s poll with a single telemetry SSE stream; until then we keep
 * the existing cadence (which works and is well-tested).
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

  // CPU temp sparkline data — last 30 samples (~2.5 min at 5s cadence).
  const cpuHistory = useMemo(
    () =>
      history
        .flatMap((s) =>
          s.temps && s.temps.cpu_temp > 0 ? [s.temps.cpu_temp] : [],
        )
        .slice(-30),
    [history],
  );

  const connected = engineStatus === "up" && payloadStatus === "up";

  return (
    <div className="mx-auto w-full max-w-6xl p-4 md:p-6">
      {/* Page heading — concise. v5 spec wants "at-a-glance status + most
          likely next actions". */}
      <header className="mb-6 flex items-center justify-between">
        <div>
          <h1 className="flex items-center gap-2 text-2xl font-bold tracking-tight">
            <LayoutDashboard size={24} aria-hidden />
            {tr("v5_home_title", "Home")}
          </h1>
          <p className="mt-1 text-sm text-[var(--color-muted)]">
            {tr(
              "v5_home_subtitle",
              "At-a-glance status and quick actions for your PS5.",
            )}
          </p>
        </div>
        {connected ? (
          <Badge tone="good">
            {tr("v5_home_connected", "Connected")}
          </Badge>
        ) : (
          <Badge tone="bad">
            {tr("v5_home_disconnected", "Not connected")}
          </Badge>
        )}
      </header>

      {/* Responsive grid. On desktop: 3-col. Tablet: 2-col. Mobile: 1-col. */}
      <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
        {/* 1. Connection card — spans 1 col, prominent position. */}
        <Card
          className="xl:col-span-1"
        >
          <CardHeader
            icon={Cable}
            title={tr("v5_home_connection", "Connection")}
          />
          <div className="space-y-2">
            <KvRow
              label={tr("v5_home_host", "Host")}
              value={host || "—"}
            />
            <KvRow
              label={tr("v5_home_engine", "Engine")}
              value={engineStatus === "up" ? "up" : "down"}
              tone={
                engineStatus === "up" ? "good" : "bad"
              }
            />
            <KvRow
              label={tr("v5_home_helper", "Helper")}
              value={payloadVersion ? `v${payloadVersion}` : payloadStatus}
              tone={payloadStatus === "up" ? "good" : "bad"}
            />
            <KvRow
              label={tr("v5_home_kernel", "Kernel")}
              value={ps5Kernel ?? "—"}
              small
            />
            <KvRow
              label={tr("v5_home_krw", "Kernel R/W")}
              value={
                ucredElevated === null
                  ? "—"
                  : ucredElevated
                    ? tr("v5_home_available", "available")
                    : tr("v5_home_missing", "missing")
              }
              tone={
                ucredElevated === null
                  ? undefined
                  : ucredElevated
                    ? "good"
                    : "warn"
              }
            />
          </div>
        </Card>

        {/* 2. Live sensors — CPU/SoC temps with sparkline. */}
        <Card>
          <CardHeader
            icon={Cpu}
            title={tr("v5_home_sensors", "Live sensors")}
          />
          {temps ? (
            <div className="space-y-2">
              <div className="flex items-center justify-between">
                <KvRow
                  label={tr("v5_home_cpu", "CPU")}
                  value={`${temps.cpu_temp?.toFixed(0) ?? "?"}°C`}
                  tone={
                    (temps.cpu_temp ?? 0) >= 85
                      ? "warn"
                      : undefined
                  }
                />
                {cpuHistory.length >= 2 && (
                  <Sparkline
                    data={cpuHistory}
                    width={70}
                    height={22}
                    color={
                      (temps.cpu_temp ?? 0) >= 85
                        ? "var(--color-warn)"
                        : "var(--color-text)"
                    }
                    fill
                  />
                )}
              </div>
              <KvRow
                label={tr("v5_home_soc", "SoC")}
                value={`${temps.soc_temp?.toFixed(0) ?? "?"}°C`}
                tone={
                  (temps.soc_temp ?? 0) >= 85
                    ? "warn"
                    : undefined
                }
              />
              {temps.m2_temp > 0 && (
                <KvRow
                  label={tr("v5_home_m2", "M.2 SSD")}
                  value={`${temps.m2_temp.toFixed(0)}°C`}
                />
              )}
              {power && (
                <KvRow
                  label={tr("v5_home_lifetime", "Lifetime")}
                  value={`${power.operating_time_hours ?? 0}h, ${power.boot_count ?? 0} ${tr("v5_home_boots", "boots")}`}
                />
              )}
            </div>
          ) : (
            <div className="flex items-center gap-2 text-sm text-[var(--color-muted)]">
              <Spinner size={14} />
              {tr("v5_home_loading_sensors", "Reading sensors…")}
            </div>
          )}
        </Card>

        {/* 3. Quick actions — deep-links to common tasks. */}
        <Card>
          <CardHeader
            icon={ActivityIcon}
            title={tr("v5_home_quick_actions", "Quick actions")}
          />
          <div className="grid grid-cols-2 gap-2">
            <QuickAction
              to="/upload"
              icon={Upload}
              label={tr("v5_qa_upload", "Upload")}
            />
            <QuickAction
              to="/install-package"
              icon={PackageOpen}
              label={tr("v5_qa_install", "Install PKG")}
            />
            <QuickAction
              to="/saves"
              icon={Save}
              label={tr("v5_qa_saves", "Backup saves")}
            />
            <QuickAction
              to="/ftp-server"
              icon={Server}
              label={tr("v5_qa_ftp", "Start FTP")}
            />
            <QuickAction
              to="/files"
              icon={FolderTree}
              label={tr("v5_qa_files", "Files")}
            />
            <QuickAction
              to="/games"
              icon={Gamepad2}
              label={tr("v5_qa_games", "Games")}
            />
          </div>
        </Card>

        {/* 4. Running apps (if any). */}
        {runningTitleIds.size > 0 && (
          <Card>
            <CardHeader
              icon={Power}
              title={tr(
                "v5_home_running",
                { n: runningTitleIds.size },
                `Running (${runningTitleIds.size})`,
              )}
            />
            <ul className="space-y-1 text-xs">
              {Array.from(runningTitleIds).slice(0, 5).map((tid) => (
                <li key={tid} className="font-mono">
                  {tid}
                </li>
              ))}
              {runningTitleIds.size > 5 && (
                <li className="text-[var(--color-muted)]">
                  + {runningTitleIds.size - 5} {tr("v5_home_more", "more")}
                </li>
              )}
            </ul>
          </Card>
        )}

        {/* 5. Recent activity — last 5 operations. */}
        <Card>
          <CardHeader
            icon={ActivityIcon}
            title={tr("v5_home_recent_activity", "Recent activity")}
          />
          {recentActivity.length === 0 ? (
            <p className="text-sm text-[var(--color-muted)]">
              {tr("v5_home_no_activity", "No recent activity.")}
            </p>
          ) : (
            <ul className="space-y-1.5 text-xs">
              {recentActivity.map((e) => (
                <li
                  key={e.id}
                  className="flex items-start gap-2"
                >
                  {e.outcome === "done" && (
                    <CheckCircle2
                      size={12}
                      aria-hidden
                      className="mt-0.5 shrink-0 text-[var(--color-good)]"
                    />
                  )}
                  {e.outcome === "failed" && (
                    <XCircle
                      size={12}
                      aria-hidden
                      className="mt-0.5 shrink-0 text-[var(--color-bad)]"
                    />
                  )}
                  {e.outcome === "running" && (
                    <Spinner size={12} className="mt-0.5 shrink-0" />
                  )}
                  <span className="min-w-0 flex-1 truncate">
                    {e.label}
                  </span>
                  <ConsoleChip
                    addr={e.addr}
                    className="shrink-0 text-xs"
                  />
                </li>
              ))}
            </ul>
          )}
        </Card>

        {/* 6. Recent notifications. */}
        <Card>
          <CardHeader
            icon={Bell}
            title={tr("v5_home_notifications", "Notifications")}
          />
          {recentNotifs.length === 0 ? (
            <p className="text-sm text-[var(--color-muted)]">
              {tr("v5_home_no_notifications", "No notifications.")}
            </p>
          ) : (
            <ul className="space-y-1.5 text-xs">
              {recentNotifs.map((n) => (
                <li key={n.id}>
                  <span className="font-medium">{n.title}</span>
                  {n.body && (
                    <span className="ml-1 text-[var(--color-muted)]">
                      {n.body}
                    </span>
                  )}
                </li>
              ))}
            </ul>
          )}
        </Card>
      </div>
    </div>
  );
}

function CardHeader({ icon: Icon, title }: { icon: LucideIcon; title: string }) {
  return (
    <h2 className="mb-3 flex items-center gap-2 text-sm font-semibold">
      <Icon size={16} aria-hidden className="text-[var(--color-muted)]" />
      {title}
    </h2>
  );
}

function KvRow({
  label,
  value,
  tone,
  small,
}: {
  label: string;
  value: string;
  tone?: "good" | "warn" | "bad";
  small?: boolean;
}) {
  const toneCls =
    tone === "good"
      ? "text-[var(--color-good)]"
      : tone === "warn"
        ? "text-[var(--color-warn)]"
        : tone === "bad"
          ? "text-[var(--color-bad)]"
          : "";
  return (
    <div className="flex items-center justify-between text-sm">
      <span className="text-[var(--color-muted)]">{label}</span>
      <span
        className={`tabular-nums ${toneCls} ${small ? "max-w-[200px] truncate" : ""}`}
        title={value}
      >
        {value}
      </span>
    </div>
  );
}

function QuickAction({
  to,
  icon: Icon,
  label,
}: {
  to: string;
  icon: LucideIcon;
  label: string;
}) {
  return (
    <Link
      to={to}
      className="flex flex-col items-center gap-1.5 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] p-3 text-center transition-colors hover:border-[var(--color-accent)] hover:bg-[var(--color-surface-3)] focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-accent)]"
    >
      <Icon size={20} aria-hidden className="text-[var(--color-accent)]" />
      <span className="text-xs font-medium">{label}</span>
    </Link>
  );
}
