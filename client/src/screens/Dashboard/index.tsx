import { useMemo } from "react";
import {
  LayoutDashboard,
  Cpu,
  Activity as ActivityIcon,
  Bell,
  Cable,
  CheckCircle2,
  XCircle,
  Power,
} from "lucide-react";
import { useConnectionStore } from "../../state/connection";
import { useActivityHistoryStore } from "../../state/activityHistory";
import { useNotificationsStore } from "../../state/notifications";
import { useRunningAppsStore } from "../../state/runningApps";
import { useSensors } from "../../state/sensors";
import { PageHeader, ConnectionGate, ConsoleChip, Spinner } from "../../components";
import { useTr } from "../../state/lang";

/**
 * Live status dashboard. One-pane summary of every signal the app
 * already tracks, refreshed every 5 seconds while the window is
 * visible. Reuses existing state stores + the existing Hardware
 * RPCs; no new payload work.
 *
 * Sections:
 *   • Connection status (engine + payload + ucred)
 *   • Live temps + power
 *   • Running apps count + list
 *   • Last 5 activity entries
 *   • Last 5 notifications
 *
 * Useful as a "morning check" when first opening the app.
 */
export default function DashboardScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const engineStatus = useConnectionStore((s) => s.engineStatus);
  const payloadVersion = useConnectionStore((s) => s.payloadVersion);
  const ps5Kernel = useConnectionStore((s) => s.ps5Kernel);
  const ucredElevated = useConnectionStore((s) => s.ucredElevated);
  const { sample: sensorSample } = useSensors(host);
  const temps = sensorSample?.temps ?? null;
  const power = sensorSample?.power ?? null;
  // Subscribe to the raw entries arrays — selectors that return
  // .slice()/.reverse() create a fresh array on every call, which
  // zustand v5 + React's useSyncExternalStore detects as an
  // unstable snapshot and bails with "Maximum update depth exceeded".
  // Derive the slices in useMemo so the slice/reverse output is
  // stable per entries array identity.
  const allActivity = useActivityHistoryStore((s) => s.entries);
  const allNotifs = useNotificationsStore((s) => s.entries);
  const recentActivity = useMemo(
    () => allActivity.slice(-5).reverse(),
    [allActivity],
  );
  const recentNotifs = useMemo(() => allNotifs.slice(0, 5), [allNotifs]);
  const runningTitleIds = useRunningAppsStore((s) => s.titleIds);

  return (
    <div className="p-6">
      <PageHeader
        icon={LayoutDashboard}
        title={tr("dashboard_title", undefined, "Dashboard")}
        description={tr(
          "dashboard_description",
          undefined,
          "Live overview of your PS5 — connection, temperatures, running apps, recent activity and notifications. Auto-refreshes every 5 seconds.",
        )}
      />

      <ConnectionGate require="payload">
        <div className="mx-auto grid max-w-6xl gap-3 md:grid-cols-2 xl:grid-cols-3">
          <DashCard
            icon={<Cable size={14} />}
            title={tr("dashboard_connection_title", "Connection")}
          >
            <KvRow
              label={tr("dashboard_label_host", "Host")}
              value={host || "—"}
            />
            <KvRow
              label={tr("dashboard_label_engine", "Engine")}
              value={engineStatus === "up" ? "up" : "down"}
              good={engineStatus === "up"}
            />
            <KvRow
              label={tr("dashboard_label_payload", "Helper")}
              value={payloadVersion ? `v${payloadVersion}` : "up"}
              good={payloadStatus === "up"}
            />
            <KvRow
              label={tr("dashboard_label_kernel", "Kernel")}
              value={ps5Kernel ?? "—"}
              small
            />
            <KvRow
              label={tr("dashboard_label_kernel_rw", "Kernel R/W")}
              value={
                ucredElevated === null
                  ? "—"
                  : ucredElevated
                    ? "available"
                    : "missing"
              }
              good={ucredElevated === true}
              warn={ucredElevated === false}
            />
          </DashCard>

          <DashCard
            icon={<Cpu size={14} />}
            title={tr("dashboard_live_sensors_title", "Live sensors")}
          >
            {temps ? (
              <>
                <KvRow
                  label={tr("dashboard_label_cpu", "CPU")}
                  value={`${temps.cpu_temp?.toFixed(0) ?? "?"}°C`}
                />
                <KvRow
                  label={tr("dashboard_label_soc", "SoC")}
                  value={`${temps.soc_temp?.toFixed(0) ?? "?"}°C`}
                />
                {temps.m2_temp > 0 && (
                  <KvRow
                    label={tr("dashboard_label_m2", "M.2 SSD")}
                    value={`${temps.m2_temp.toFixed(0)}°C`}
                  />
                )}
                <KvRow
                  label={tr("dashboard_label_cpu_freq", "CPU freq")}
                  value={`${(temps.cpu_freq_mhz ?? 0).toFixed(0)} MHz`}
                />
                {/* SoC power is an extended-read-only field; the Dashboard's
                    basic auto-poll never fetches it, so it always read 0.0 W.
                    Dropped to avoid showing a permanently-zero value (the
                    Hardware screen removed it for the same reason). */}
              </>
            ) : (
              <div className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
                <Spinner size={12} />
                {tr("dashboard_loading_sensors", "Reading sensors…")}
              </div>
            )}
            {power && (
              <>
                <KvRow
                  label={tr("dashboard_label_lifetime", "Lifetime")}
                  value={`${power.operating_time_hours ?? 0}h, ${power.boot_count ?? 0} boots`}
                />
              </>
            )}
          </DashCard>

          <DashCard
            icon={<Power size={14} />}
            title={tr(
              "dashboard_running_apps",
              { n: runningTitleIds.size },
              `Running apps (${runningTitleIds.size})`,
            )}
          >
            {runningTitleIds.size === 0 ? (
              <div className="text-xs text-[var(--color-muted)]">
                {tr("dashboard_nothing_running", "Nothing currently running.")}
              </div>
            ) : (
              <ul className="space-y-0.5 text-xs">
                {Array.from(runningTitleIds)
                  .slice(0, 5)
                  .map((tid) => (
                    <li key={tid} className="font-mono">
                      {tid}
                    </li>
                  ))}
                {runningTitleIds.size > 5 && (
                  <li className="text-[var(--color-muted)]">
                    + {runningTitleIds.size - 5} {tr("dashboard_more", "more")}
                  </li>
                )}
              </ul>
            )}
          </DashCard>

          <DashCard
            icon={<ActivityIcon size={14} />}
            title={tr("dashboard_recent_activity_title", "Recent activity")}
          >
            {recentActivity.length === 0 ? (
              <div className="text-xs text-[var(--color-muted)]">
                {tr("dashboard_no_recent_operations", "No recent operations.")}
              </div>
            ) : (
              <ul className="space-y-0.5 text-xs">
                {recentActivity.map((e) => (
                  <li key={e.id} className="flex items-start gap-1">
                    {e.outcome === "done" && (
                      <CheckCircle2
                        size={10}
                        className="mt-0.5 shrink-0 text-[var(--color-good)]"
                      />
                    )}
                    {e.outcome === "failed" && (
                      <XCircle
                        size={10}
                        className="mt-0.5 shrink-0 text-[var(--color-bad)]"
                      />
                    )}
                    {e.outcome === "running" && (
                      <Spinner
                        size={10}
                        tone="inherit"
                        className="mt-0.5 shrink-0 text-[var(--color-warn)]"
                      />
                    )}
                    <span className="min-w-0 flex-1 truncate">{e.label}</span>
                    <ConsoleChip addr={e.addr} className="shrink-0" />
                  </li>
                ))}
              </ul>
            )}
          </DashCard>

          <DashCard
            icon={<Bell size={14} />}
            title={tr(
              "dashboard_recent_notifications_title",
              "Recent notifications",
            )}
          >
            {recentNotifs.length === 0 ? (
              <div className="text-xs text-[var(--color-muted)]">
                {tr("dashboard_no_notifications", "No notifications yet.")}
              </div>
            ) : (
              <ul className="space-y-1 text-xs">
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
          </DashCard>
        </div>
      </ConnectionGate>
    </div>
  );
}

function DashCard({
  icon,
  title,
  children,
}: {
  icon: React.ReactNode;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3">
      <header className="mb-2 flex items-center gap-1 text-xs font-semibold">
        {icon}
        {title}
      </header>
      {children}
    </section>
  );
}

function KvRow({
  label,
  value,
  good,
  warn,
  small,
}: {
  label: string;
  value: string;
  good?: boolean;
  warn?: boolean;
  small?: boolean;
}) {
  return (
    <div className="flex items-center justify-between text-xs">
      <span className="text-[var(--color-muted)]">{label}</span>
      <span
        className={`tabular-nums ${
          good
            ? "text-[var(--color-good)]"
            : warn
              ? "text-[var(--color-warn)]"
              : ""
        } ${small ? "max-w-[180px] truncate" : ""}`}
        title={value}
      >
        {value}
      </span>
    </div>
  );
}
