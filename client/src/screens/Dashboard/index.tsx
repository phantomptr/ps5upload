import { useEffect, useMemo, useState } from "react";
import {
  LayoutDashboard,
  Cpu,
  Activity as ActivityIcon,
  Bell,
  Cable,
  CheckCircle2,
  XCircle,
  Loader2,
  Power,
} from "lucide-react";
import { useConnectionStore } from "../../state/connection";
import { useActivityHistoryStore } from "../../state/activityHistory";
import { useNotificationsStore } from "../../state/notifications";
import { useRunningAppsStore } from "../../state/runningApps";
import {
  fetchHwTemps,
  fetchHwPower,
  type HwTemps,
  type HwPower,
} from "../../api/ps5";
import { PageHeader, EmptyState } from "../../components";
import { useTr } from "../../state/lang";
import { useDocumentVisible } from "../../lib/visibility";

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
  const visible = useDocumentVisible();
  const [temps, setTemps] = useState<HwTemps | null>(null);
  const [power, setPower] = useState<HwPower | null>(null);
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

  useEffect(() => {
    if (!visible || !host?.trim() || payloadStatus !== "up") return;
    let cancelled = false;
    const tick = async () => {
      const addr = `${host.trim()}:9113`;
      try {
        const [t, p] = await Promise.all([
          fetchHwTemps(addr).catch(() => null),
          fetchHwPower(addr).catch(() => null),
        ]);
        if (!cancelled) {
          if (t) setTemps(t);
          if (p) setPower(p);
        }
      } catch {
        // ignore
      }
    };
    tick();
    const id = window.setInterval(tick, 5000);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [visible, host, payloadStatus]);

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

      {payloadStatus !== "up" ? (
        <EmptyState
          icon={LayoutDashboard}
          message={tr(
            "dashboard_no_payload",
            undefined,
            "Connect to your PS5 first.",
          )}
        />
      ) : (
        <div className="mx-auto grid max-w-6xl gap-3 md:grid-cols-2 xl:grid-cols-3">
          <DashCard
            icon={<Cable size={14} />}
            title={tr("dashboard_connection_title", "Connection")}
          >
            <KvRow label={tr("dashboard_label_host", "Host")} value={host || "—"} />
            <KvRow
              label={tr("dashboard_label_engine", "Engine")}
              value={engineStatus === "up" ? "up" : "down"}
              good={engineStatus === "up"}
            />
            <KvRow
              label={tr("dashboard_label_payload", "Payload")}
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
                <KvRow
                  label={tr("dashboard_label_cpu_freq", "CPU freq")}
                  value={`${(temps.cpu_freq_mhz ?? 0).toFixed(0)} MHz`}
                />
                <KvRow
                  label={tr("dashboard_label_soc_power", "SoC power")}
                  value={`${((temps.soc_power_mw ?? 0) / 1000).toFixed(1)} W`}
                />
              </>
            ) : (
              <KvRow label="—" value="(loading)" />
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
            title={`Running apps (${runningTitleIds.size})`}
          >
            {runningTitleIds.size === 0 ? (
              <div className="text-[11px] text-[var(--color-muted)]">
                {tr("dashboard_nothing_running", "Nothing currently running.")}
              </div>
            ) : (
              <ul className="space-y-0.5 text-[11px]">
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
              <div className="text-[11px] text-[var(--color-muted)]">
                {tr("dashboard_no_recent_operations", "No recent operations.")}
              </div>
            ) : (
              <ul className="space-y-0.5 text-[11px]">
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
                      <Loader2
                        size={10}
                        className="mt-0.5 shrink-0 animate-spin text-[var(--color-warn)]"
                      />
                    )}
                    <span className="min-w-0 flex-1 truncate">{e.label}</span>
                  </li>
                ))}
              </ul>
            )}
          </DashCard>

          <DashCard
            icon={<Bell size={14} />}
            title={tr("dashboard_recent_notifications_title", "Recent notifications")}
          >
            {recentNotifs.length === 0 ? (
              <div className="text-[11px] text-[var(--color-muted)]">
                {tr("dashboard_no_notifications", "No notifications yet.")}
              </div>
            ) : (
              <ul className="space-y-1 text-[11px]">
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
      )}
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
    <div className="flex items-center justify-between text-[11px]">
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

