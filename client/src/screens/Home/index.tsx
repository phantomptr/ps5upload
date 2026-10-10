import { useMemo, useState, type CSSProperties } from "react";
import PowerControl from "../Connection/PowerControl";
import { Link } from "react-router";
import {
  Activity as ActivityIcon,
  ArrowRight,
  ArrowUpRight,
  Bell,
  Cable,
  Coffee,
  CheckCircle2,
  FolderTree,
  Gamepad2,
  PackageOpen,
  PackagePlus,
  Power,
  Save,
  Upload,
  WifiOff,
  XCircle,
  type LucideIcon,
} from "lucide-react";

import { useConnectionStore } from "../../state/connection";
import { useActivityHistoryStore } from "../../state/activityHistory";
import { useNotificationsStore } from "../../state/notifications";
import { useRunningTitleIds } from "../../state/runningApps";
import { useSensors } from "../../state/sensors";
import { Badge, Card, ConsoleChip, Orb, Sparkline, Spinner } from "../../components";
import { useTr } from "../../state/lang";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { COFFEE_URL } from "../../lib/supportLinks";
import { ServersCard } from "./ServersCard";
import { HealthCard } from "./HealthCard";
import { RecentGames } from "./RecentGames";
import { greetingPart } from "./greeting";
import {
  evaluateOperationReadiness,
  type Operation,
} from "../../lib/operationReadiness";

/** The soft generated "photo" behind each action tile: a blurred mesh of the
 *  reference palette, one per tile so they read apart at a glance. No images,
 *  no network. */
const TILE_PHOTOS: Record<string, string> = {
  upload:
    "radial-gradient(60% 70% at 18% 30%, #b8a2ea 0%, transparent 70%), radial-gradient(55% 60% at 62% 45%, #ff8fc6 0%, transparent 70%), radial-gradient(50% 55% at 85% 20%, #c9b8ff 0%, transparent 70%), radial-gradient(60% 50% at 40% 85%, #ffb486 0%, transparent 70%), linear-gradient(165deg, #cdb9f2 0%, #f6c3da 60%, #fde4dc 100%)",
  install:
    "radial-gradient(60% 60% at 25% 70%, #f6c39c 0%, transparent 70%), radial-gradient(70% 60% at 70% 35%, #bfe3d0 0%, transparent 70%), radial-gradient(50% 50% at 50% 50%, #e8efe0 0%, transparent 70%), linear-gradient(170deg, #d9eee4 0%, #f3e5d4 70%, #fbf1ea 100%)",
  games:
    "radial-gradient(55% 60% at 25% 40%, #ffaf85 0%, transparent 70%), radial-gradient(45% 55% at 70% 30%, #7fd39b 0%, transparent 70%), radial-gradient(60% 50% at 55% 75%, #ffe0c2 0%, transparent 70%), linear-gradient(150deg, #ffc39f 0%, #d8efc9 55%, #f4f1e0 100%)",
  files:
    "radial-gradient(50% 70% at 30% 35%, #93b2ea 0%, transparent 70%), radial-gradient(45% 70% at 70% 45%, #f597b4 0%, transparent 70%), radial-gradient(60% 45% at 50% 90%, #e9e1f7 0%, transparent 70%), linear-gradient(180deg, #b9c8f0 0%, #f2bfd2 60%, #f1ecf7 100%)",
  saves:
    "radial-gradient(55% 60% at 30% 60%, #ff8d63 0%, transparent 70%), radial-gradient(50% 55% at 72% 35%, #ffd27a 0%, transparent 70%), radial-gradient(55% 50% at 60% 85%, #ffc0d2 0%, transparent 70%), linear-gradient(160deg, #ffb895 0%, #ffe3b8 55%, #fde8ee 100%)",
  convert:
    "radial-gradient(55% 60% at 25% 35%, #ff9ac2 0%, transparent 70%), radial-gradient(55% 60% at 75% 60%, #c8b3ff 0%, transparent 70%), radial-gradient(50% 45% at 50% 90%, #ffd1a6 0%, transparent 70%), linear-gradient(140deg, #ffc3d9 0%, #e1d2fb 60%, #fbe9e1 100%)",
};

/** How each tile's light streaks lie, so no two photos look stamped from one. */
const TILE_STREAKS: Record<string, string> = {
  upload: "rotate(-7deg)",
  install: "rotate(4deg) scaleX(-1)",
  games: "rotate(-14deg) translateY(8%)",
  files: "rotate(78deg) scale(1.3)",
  saves: "rotate(10deg) translateY(-6%)",
  convert: "rotate(-3deg) scaleX(-1) translateY(10%)",
};

type RecentFilter = "all" | "done" | "failed";

/**
 * Product-level command center. The hierarchy is deliberate:
 *  1. Can I use the console right now, and what should I do next?
 *  2. What common operation do I want to start?
 *  3. What is the console reporting, and what just happened?
 *
 * Laid out like the reference dashboard: framed photo tiles for the main
 * actions, a greeting panel with the orb that carries the console's status,
 * and glass cards below.
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
  const [recentFilter, setRecentFilter] = useState<RecentFilter>("all");

  const allActivity = useActivityHistoryStore((s) => s.entries);
  const allNotifs = useNotificationsStore((s) => s.entries);
  const recentActivity = useMemo(
    () =>
      allActivity
        .filter((e) => recentFilter === "all" || e.outcome === recentFilter)
        .slice(-5)
        .reverse(),
    [allActivity, recentFilter],
  );
  const recentNotifs = useMemo(() => allNotifs.slice(0, 5), [allNotifs]);
  const runningTitleIds = useRunningTitleIds(host);
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

  const part = greetingPart(new Date().getHours());
  const greeting =
    part === "morning"
      ? tr("v5_home_greeting_morning", "Good morning")
      : part === "afternoon"
        ? tr("v5_home_greeting_afternoon", "Good afternoon")
        : tr("v5_home_greeting_evening", "Good evening");

  const recentFilters: Array<{ id: RecentFilter; label: string }> = [
    { id: "all", label: tr("v5_home_recent_all", "All") },
    { id: "done", label: tr("v5_home_recent_done", "Finished") },
    { id: "failed", label: tr("v5_home_recent_failed", "Failed") },
  ];

  return (
    <div className="app-page">
      <header className="mb-7 flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0">
          <h1 className="text-[2.25rem] leading-[1.05] font-bold tracking-[-0.035em] sm:text-[3rem]">
            {tr("v5_home_title", "Home")}
          </h1>
          <p className="mt-2 max-w-2xl text-sm text-[var(--color-muted)]">
            {tr(
              "v5_home_subtitle",
              "Manage your console, move content, and monitor active work.",
            )}
          </p>
        </div>
        {/* The connection badge, and Buy me a coffee right under it: right-aligned on a wide
            window, under the title on a phone. */}
        <div className="flex flex-col items-start gap-2 sm:items-end">
          <span data-testid="home-connection-badge">
            <Badge tone={connected ? "good" : "warn"} size="md" dot>
              {connected
                ? tr("v5_home_connected", "Connected")
                : tr("v5_home_setup_required", "Setup required")}
            </Badge>
          </span>
          <button
            type="button"
            onClick={() => void openExternalUrl(COFFEE_URL)}
            className="inline-flex min-h-9 items-center gap-1.5 rounded-full border border-[var(--color-border-strong)] px-4 text-xs font-medium text-[var(--color-text)] transition-colors hover:bg-[var(--color-surface)]"
          >
            <Coffee size={13} aria-hidden className="text-[var(--color-accent-bright)]" />
            {tr("buy_coffee", "Buy me a coffee")}
          </button>
        </div>
      </header>

      <div className="grid gap-6 xl:grid-cols-[minmax(0,1fr)_21rem]">
        <div className="min-w-0 space-y-6">
          {/* The main actions as framed photo tiles: one wide, the rest in a row. */}
          {/* The main actions as framed photo tiles, a mosaic like the reference:
              Upload large on the left, two stacked beside it, three below. */}
          <section
            aria-label={tr("v5_home_quick_actions", "Quick actions")}
            className="grid grid-cols-2 gap-4 sm:auto-rows-[10.5rem] sm:grid-cols-6"
          >
            <PhotoTile
              to="/upload"
              photo="upload"
              icon={Upload}
              label={tr("v5_qa_upload", "Upload files")}
              readiness={readinessFor("upload")}
              className="col-span-2 h-48 sm:col-span-4 sm:row-span-2 sm:h-auto"
              large
            />
            <PhotoTile to="/install-package" photo="install" icon={PackageOpen} label={tr("v5_qa_install", "Install package")} readiness={readinessFor("install-package")} className="h-40 sm:col-span-2 sm:h-auto" />
            <PhotoTile to="/games" photo="games" icon={Gamepad2} label={tr("v5_qa_games", "Open Games")} readiness={readinessFor("browse-console")} className="h-40 sm:col-span-2 sm:h-auto" />
            <PhotoTile to="/files" photo="files" icon={FolderTree} label={tr("v5_qa_files", "Browse files")} readiness={readinessFor("browse-console")} className="h-40 sm:col-span-2 sm:h-auto" />
            <PhotoTile to="/saves" photo="saves" icon={Save} label={tr("v5_qa_saves", "Back up saves")} readiness={readinessFor("browse-console")} className="h-40 sm:col-span-2 sm:h-auto" />
            <PhotoTile to="/convert" photo="convert" icon={PackagePlus} label={tr("v5_qa_convert", "Convert")} readiness={readinessFor("local-only")} className="col-span-2 h-40 sm:col-span-2 sm:h-auto" />
          </section>

          {/* The newest games from the Collection, framed like the reference's
              "Recommended" row. Renders nothing while the Collection is empty. */}
          <RecentGames />

          {/* Power, right under the actions — issue #316. These lived only at the
              bottom of Manage connections, which is several clicks away from the
              screen people actually sit on. Same component as that page rather
              than a copy, so the confirmations and the Wake button cannot drift
              apart between the two places.

              Shown whenever a host is configured, not only when connected: Wake
              is precisely the action you want when the console is NOT reachable,
              and hiding the panel then would remove it at the one moment it is
              the point. */}
          {host ? <PowerControl host={host} /> : null}

          {/* What needs attention, before what just happened: a blocked port or a full drive
              is the reason the next action would fail. Only once the helper answers; without it
              the greeting panel already says what is wrong. */}
          {host && connected ? <HealthCard host={host} /> : null}

          <section>
            <div className="mb-3 flex flex-wrap items-center gap-3">
              {/* A floor on its width: on a phone the chips wrap under it instead
                  of squeezing it to a letter per line. */}
              <h2 className="min-w-[10rem] flex-1 text-[1.125rem] font-semibold tracking-[-0.01em]">
                {tr("v5_home_recent_activity", "Recent activity")}
              </h2>
              <div
                role="group"
                aria-label={tr("v5_home_recent_filter", "Show activity")}
                className="flex flex-wrap items-center gap-2"
              >
                {recentFilters.map((f) => (
                  <button
                    key={f.id}
                    type="button"
                    aria-pressed={recentFilter === f.id}
                    onClick={() => setRecentFilter(f.id)}
                    className="chip min-h-9 px-4 text-xs font-medium"
                  >
                    {f.label}
                  </button>
                ))}
              </div>
              <InlineLink to="/tasks" label={tr("v5_tab_tasks", "View tasks")} />
            </div>
            <Card>
              <p className="mb-3 text-[0.6875rem] text-[var(--color-muted)]">
                {tr("v5_home_recent_activity_desc", "Latest operations across every console.")}
              </p>
              {recentActivity.length === 0 ? (
                <CompactEmpty icon={ActivityIcon} title={tr("v5_home_no_activity", "No activity yet")} body={tr("v5_home_no_activity_desc", "Uploads, installs, and file jobs will appear here.")} />
              ) : (
                <ul className="space-y-1">
                  {recentActivity.map((entry) => (
                    <li key={entry.id} className="flex min-h-11 items-center gap-3 rounded-2xl px-3 py-2 text-xs hover:bg-[var(--color-surface-3)]">
                      {entry.outcome === "done" ? (
                        <CheckCircle2 size={15} className="shrink-0 text-[var(--color-good)]" />
                      ) : entry.outcome === "failed" ? (
                        <XCircle size={15} className="shrink-0 text-[var(--color-bad)]" />
                      ) : entry.outcome === "running" ? (
                        <Spinner size={13} className="shrink-0" />
                      ) : (
                        <ActivityIcon size={15} className="shrink-0 text-[var(--color-muted)]" />
                      )}
                      <span className="min-w-0 flex-1 truncate font-medium">{entry.label}</span>
                      <ConsoleChip addr={entry.addr} className="shrink-0" />
                    </li>
                  ))}
                </ul>
              )}
            </Card>
          </section>
        </div>

        {/* The greeting panel: the orb, a time-of-day hello, and everything about the console. */}
        <aside className="glass relative min-w-0 self-start overflow-hidden rounded-[var(--radius-panel)] p-6">
          <div aria-hidden className="dot-texture pointer-events-none absolute inset-x-0 top-0 h-56" />
          <div className="relative flex flex-col items-center pt-2 text-center">
            <Orb size={72} />
            <p className="mt-5 text-[1.6rem] leading-[1.2] tracking-[-0.02em]">
              <span className="font-semibold">{greeting},</span>{" "}
              <span>{tr("v5_home_greeting_question", "What are we sending today?")}</span>
            </p>
          </div>

          <div className="relative mt-6 border-t border-[var(--color-border)] pt-5">
            <div className="flex items-start gap-3">
              <span
                className={`grid h-10 w-10 shrink-0 place-items-center rounded-full ${
                  connected
                    ? "bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)]"
                    : "bg-[var(--color-warn-soft)] text-[var(--color-warn)]"
                }`}
              >
                {connected ? <Cable size={18} /> : <WifiOff size={18} />}
              </span>
              <div className="min-w-0 flex-1">
                <h2 className="text-sm font-semibold">
                  {connected
                    ? tr("v5_home_console_ready", "Your PS5 is ready")
                    : tr("v5_home_connect_title", "Connect a PS5 to get started")}
                </h2>
                <p className="mt-0.5 text-xs leading-relaxed text-[var(--color-muted)]">
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
            </div>
            <Link
              to="/connection"
              className="mt-4 flex min-h-12 items-center justify-between gap-2 rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] py-1.5 ps-5 pe-1.5 text-sm font-medium shadow-[var(--edge-highlight),var(--shadow-1)] transition-colors hover:bg-[var(--color-float)]"
            >
              {connected
                ? tr("v5_home_manage_connection", "Manage connection")
                : tr("v5_home_connect", "Connect PS5")}
              <span className="photo-tile-arrow h-9 w-9">
                <ArrowRight size={16} aria-hidden />
              </span>
            </Link>
          </div>

          <div className="relative mt-5">
            <h3 className="mb-1 text-[0.6875rem] font-medium uppercase tracking-[0.08em] text-[var(--color-muted)]">
              {tr("v5_home_console_status", "Console status")}
            </h3>
            <p className="sr-only">
              {tr("v5_home_console_status_desc", "Connection and live system health.")}
            </p>
            <MetricRow label={tr("v5_home_host", "Host")} value={host || "—"} />
            <MetricRow label={tr("v5_home_engine", "Local engine")} value={engineStatus} tone={engineStatus === "up" ? "good" : "bad"} />
            <MetricRow label={tr("v5_home_helper", "PS5 helper")} value={payloadVersion ? `v${payloadVersion}` : payloadStatus} tone={payloadStatus === "up" ? "good" : "warn"} />
            <MetricRow
              label={tr("v5_home_krw", "Kernel access")}
              value={ucredElevated === null ? "—" : ucredElevated ? tr("v5_home_available", "Available") : tr("v5_home_missing", "Unavailable")}
              tone={ucredElevated === null ? undefined : ucredElevated ? "good" : "warn"}
            />
          </div>

          <div className="relative mt-4 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] p-4">
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
                  color={(temps.cpu_temp ?? 0) >= 85 ? "var(--color-warn)" : "var(--color-accent-bright)"}
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
            <p className="relative mt-3 truncate font-mono text-[0.6875rem] text-[var(--color-muted)]" title={ps5Kernel}>{ps5Kernel}</p>
          )}
          {runningTitleIds.size > 0 && (
            <div className="relative mt-3 flex items-center gap-2 text-xs">
              <Power size={13} className="text-[var(--color-good)]" />
              <span className="font-medium">{tr("v5_home_running", { n: runningTitleIds.size }, `${runningTitleIds.size} running`)}</span>
              <span className="truncate font-mono text-[var(--color-muted)]">{Array.from(runningTitleIds).slice(0, 3).join(", ")}</span>
            </div>
          )}
        </aside>
      </div>

      <div className="mt-6 grid gap-6 xl:grid-cols-12">
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
                <li key={notification.id} className="rounded-2xl bg-[var(--color-surface)] px-4 py-2.5 text-xs">
                  <div className="font-semibold">{notification.title}</div>
                  {notification.body && <div className="mt-0.5 line-clamp-2 text-[var(--color-muted)]">{notification.body}</div>}
                </li>
              ))}
            </ul>
          )}
        </Card>

        {/* Saved servers, once and always here: browse a NAS, or add the first one. */}
        <div className="min-w-0 xl:col-span-7 [&>section]:h-full">
          <ServersCard />
        </div>
      </div>
    </div>
  );
}

function SectionHeading({ icon: Icon, title, description, action }: { icon: LucideIcon; title: string; description: string; action?: React.ReactNode }) {
  return (
    <header className="mb-4 flex items-start gap-3">
      <span className="grid h-9 w-9 shrink-0 place-items-center rounded-full bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)]"><Icon size={16} /></span>
      <div className="min-w-0 flex-1">
        <h2 className="text-[0.9375rem] font-semibold tracking-tight">{title}</h2>
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
    <div className="rounded-2xl bg-[var(--color-surface)] px-3 py-2">
      <div className="text-[0.625rem] uppercase tracking-wide text-[var(--color-muted)]">{label}</div>
      <div className="mt-0.5 text-base font-semibold tabular-nums">{value}</div>
    </div>
  );
}

/** A main action as a framed photo: thick white frame, the soft generated
 *  photo, the label bottom-left and a white round arrow. When the action
 *  can't run yet it stays visible but inert, with the reason in place of
 *  "Ready". */
function PhotoTile({
  to,
  photo,
  icon: Icon,
  label,
  readiness,
  className = "",
  large = false,
}: {
  to: string;
  photo: keyof typeof TILE_PHOTOS;
  icon: LucideIcon;
  label: string;
  readiness: { ready: boolean; blockers: string[]; warnings: string[] };
  className?: string;
  large?: boolean;
}) {
  const tr = useTr();
  const detail = readiness.ready
    ? readiness.warnings[0] || tr("v5_qa_ready", "Ready")
    : readiness.blockers[0] || tr("v5_qa_unavailable", "Unavailable");
  const photoStyle: CSSProperties = { backgroundImage: TILE_PHOTOS[photo] };
  const content = (
    <span className="photo-tile-image block" style={{ "--streak": TILE_STREAKS[photo] } as CSSProperties}>
      {/* Blurred and slightly enlarged, so the mesh reads as a soft
          long-exposure photo rather than a gradient. */}
      <span aria-hidden className="absolute -inset-6 -z-10 block blur-xl saturate-[1.15]" style={photoStyle} />
      <span className="absolute left-4 top-4 z-[1] grid h-9 w-9 place-items-center rounded-full bg-white/70 text-[#2a2224] shadow-sm backdrop-blur-sm">
        <Icon size={16} aria-hidden />
      </span>
      {/* The arrow sits beside the label on the wide tile, as in the reference;
          on the narrow ones it moves to the top corner so the label keeps the
          whole width. */}
      {readiness.ready && !large && (
        <span className="photo-tile-arrow absolute right-4 top-4 z-[1]">
          <ArrowUpRight size={18} aria-hidden />
        </span>
      )}
      <span className="absolute inset-x-0 bottom-0 z-[1] flex items-end gap-3 p-4">
        <span className="min-w-0 flex-1">
          <span className={`line-clamp-2 block font-medium leading-tight tracking-[-0.01em] text-[var(--color-text)] ${large ? "text-[1.5rem]" : "text-[1rem]"}`}>
            {label}
          </span>
          <span className="mt-0.5 block truncate text-[0.6875rem] text-[var(--color-muted)]">{detail}</span>
        </span>
        {readiness.ready && large && (
          <span className="photo-tile-arrow shrink-0">
            <ArrowUpRight size={18} aria-hidden />
          </span>
        )}
      </span>
    </span>
  );
  if (!readiness.ready) {
    return (
      <div aria-disabled="true" title={readiness.blockers.join(" ")} className={`photo-tile ${className}`}>
        {content}
      </div>
    );
  }
  return (
    <Link to={to} className={`photo-tile ${className}`}>
      {content}
    </Link>
  );
}

function CompactEmpty({ icon: Icon, title, body }: { icon: LucideIcon; title: string; body: string }) {
  return (
    <div className="flex min-h-24 items-center gap-4 rounded-[var(--radius-card)] bg-[var(--color-surface)] px-4 py-3">
      <span className="relative grid h-11 w-11 shrink-0 place-items-center">
        <Orb size={44} className="[grid-area:1/1]" />
        <Icon size={17} className="relative text-white [grid-area:1/1]" />
      </span>
      <div>
        <div className="text-xs font-semibold">{title}</div>
        <div className="mt-0.5 text-[0.6875rem] text-[var(--color-muted)]">{body}</div>
      </div>
    </div>
  );
}

function InlineLink({ to, label }: { to: string; label: string }) {
  return (
    <Link to={to} className="inline-flex shrink-0 items-center gap-1 text-xs font-semibold text-[var(--color-accent)] hover:underline">
      {label}<ArrowRight size={13} />
    </Link>
  );
}
