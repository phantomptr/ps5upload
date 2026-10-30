import { useMemo, useState } from "react";
import PowerControl from "../Connection/PowerControl";
import { Link } from "react-router";
import {
  Activity as ActivityIcon,
  ArrowRight,
  ChevronRight,
  Bell,
  Cable,
  CheckCircle2,
  FolderTree,
  Gamepad2,
  PackagePlus,
  Power,
  Rocket,
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
import { Badge, Card, ConsoleChip, Sparkline, Spinner } from "../../components";
import { SendHelperButton } from "../../components/SendHelperButton";
import { useTr } from "../../state/lang";
import { SupportButton } from "../../components/SupportButton";
import { ServersCard } from "./ServersCard";
import { HealthCard } from "./HealthCard";
import { LowSpaceCard } from "./LowSpaceCard";
import { WhatsNewCard } from "./WhatsNewCard";
import { PlayCard } from "./PlayCard";
import { RecentGames } from "./RecentGames";
import { greetingPart } from "./greeting";
import { initials, useGreetedUser } from "./consoleUser";
import { mgmtAddr } from "../../lib/addr";
import {
  evaluateOperationReadiness,
  HELPER_NOT_CONNECTED,
  type Operation,
} from "../../lib/operationReadiness";

type RecentFilter = "all" | "done" | "failed";

/**
 * Product-level command center. The hierarchy is deliberate:
 *  1. Can I use the console right now, and what should I do next?
 *  2. What common operation do I want to start?
 *  3. What is the console reporting, and what just happened?
 *
 * Laid out like the reference dashboard: framed photo tiles for the main
 * actions, a console panel that carries the console's status,
 * and glass cards below.
 */
export default function HomeScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const consolePower = useConnectionStore((s) => s.power);
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
  // Asleep is not "setup required": nothing needs setting up, the console only needs a wake.
  const resting =
    !connected && engineStatus === "up" && payloadStatus === "down" && consolePower === "rest";
  const readinessContext = {
    host,
    engineUp: engineStatus === "up",
    helperUp: payloadStatus === "up",
    kernelRw: ucredElevated,
  };
  // An asleep console's helper is gone because of the rest, not a fault: say so on the tiles.
  const readinessFor = (operation: Operation) => {
    const r = evaluateOperationReadiness(operation, readinessContext);
    if (!resting) return r;
    const rest = tr("status_ps5_rest_mode", "In rest mode");
    return { ...r, blockers: r.blockers.map((b) => (b === HELPER_NOT_CONNECTED ? rest : b)) };
  };

  // The user at the selected console, greeted by name with their avatar; read in the background
  // and cached per console (consoleUser.ts), and nobody while no console is connected.
  const greeted = useGreetedUser(connected && host ? mgmtAddr(host) : "");
  const part = greetingPart(new Date().getHours());
  const name = greeted?.name ?? "";
  const greeting = greeted
    ? part === "morning"
      ? tr("v5_home_greeting_morning_name", { name }, `Good morning, ${name}`)
      : part === "afternoon"
        ? tr("v5_home_greeting_afternoon_name", { name }, `Good afternoon, ${name}`)
        : tr("v5_home_greeting_evening_name", { name }, `Good evening, ${name}`)
    : part === "morning"
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
          <div className="mb-1 flex min-w-0 items-center gap-2.5" data-testid="home-greeting">
            {greeted && <UserAvatar name={greeted.name} src={greeted.avatar} />}
            <p className="min-w-0 truncate text-sm font-medium text-[var(--color-muted)]">
              {greeting}
            </p>
          </div>
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
            <Badge tone={connected ? "good" : resting ? "accent" : "warn"} size="md" dot>
              {connected
                ? tr("v5_home_connected", "Connected")
                : resting
                  ? tr("v5_home_rest_mode", "In rest mode")
                  : tr("v5_home_setup_required", "Setup required")}
            </Badge>
          </span>
          <SupportButton />
        </div>
      </header>

      {/* Both grids answer to their own width in rem, not the window's: a large Text
          size or a narrow window drops columns instead of squeezing tiles until a label
          wraps one letter per line (report with Text size at its largest). */}
      <div className="@container">
        <div className="grid gap-6 @4xl:grid-cols-[minmax(0,1fr)_21rem]">
          <div className="@container min-w-0 space-y-6">
            {/* Play (10.0.0): first on Home. Hidden on request; nothing below needs it. */}
            <PlayCard />

            {/* The main actions: six even tiles, each a plain glass card. */}
            <section
              aria-label={tr("v5_home_quick_actions", "Quick actions")}
              className="grid grid-cols-1 gap-3 @sm:grid-cols-2 @xl:grid-cols-3"
            >
              <ActionTile
                to="/send"
                icon={Upload}
                label={tr("send_title_v2", undefined, "Upload & Install")}
                readiness={readinessFor("upload")}
              />
              <ActionTile
                to="/payloads?tab=send"
                icon={Rocket}
                label={tr("v5_qa_send_payload", undefined, "Send Payload")}
                // A payload goes to the console's loader, not the helper: it is often what
                // gets the helper (or kstuff) running in the first place.
                readiness={readinessFor("local-only")}
              />
              <ActionTile
                to="/games"
                icon={Gamepad2}
                label={tr("v5_qa_games", "Open Games")}
                readiness={readinessFor("browse-console")}
              />
              <ActionTile
                to="/files"
                icon={FolderTree}
                label={tr("v5_qa_files", "Browse files")}
                readiness={readinessFor("browse-console")}
              />
              <ActionTile
                to="/saves"
                icon={Save}
                label={tr("v5_qa_saves", "Back up saves")}
                readiness={readinessFor("browse-console")}
              />
              <ActionTile
                to="/workshop"
                icon={PackagePlus}
                label={tr("workshop_title", undefined, "Workshop")}
                readiness={readinessFor("local-only")}
              />
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
              the console panel already says what is wrong. */}
            <WhatsNewCard />
            {host && connected ? <LowSpaceCard host={host} /> : null}
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
                <InlineLink
                  to="/tasks"
                  label={tr("v5_tab_tasks", "View tasks")}
                />
              </div>
              <Card>
                <p className="mb-3 text-[0.6875rem] text-[var(--color-muted)]">
                  {tr(
                    "v5_home_recent_activity_desc",
                    "Latest operations across every console.",
                  )}
                </p>
                {recentActivity.length === 0 ? (
                  <CompactEmpty
                    icon={ActivityIcon}
                    title={tr("v5_home_no_activity", "No activity yet")}
                    body={tr(
                      "v5_home_no_activity_desc",
                      "Uploads, installs, and file jobs will appear here.",
                    )}
                  />
                ) : (
                  <ul className="space-y-1">
                    {recentActivity.map((entry) => (
                      <li
                        key={entry.id}
                        className="flex min-h-11 items-center gap-3 rounded-2xl px-3 py-2 text-xs hover:bg-[var(--color-surface-3)]"
                      >
                        {entry.outcome === "done" ? (
                          <CheckCircle2
                            size={15}
                            className="shrink-0 text-[var(--color-good)]"
                          />
                        ) : entry.outcome === "failed" ? (
                          <XCircle
                            size={15}
                            className="shrink-0 text-[var(--color-bad)]"
                          />
                        ) : entry.outcome === "running" ? (
                          <Spinner size={13} className="shrink-0" />
                        ) : (
                          <ActivityIcon
                            size={15}
                            className="shrink-0 text-[var(--color-muted)]"
                          />
                        )}
                        <span className="min-w-0 flex-1 truncate font-medium">
                          {entry.label}
                        </span>
                        <ConsoleChip addr={entry.addr} className="shrink-0" />
                      </li>
                    ))}
                  </ul>
                )}
              </Card>
            </section>
          </div>

          {/* Everything about the console, then Notifications under it. The status card
            alone was half the height of the column beside it, which left a tall empty strip
            under it on a wide window (it used to stick while scrolling to hide that); the
            Notifications card fills the column instead and is narrow content anyway. On a
            narrow window the order is the same as before: status, Notifications, Servers. */}
          <div className="flex min-w-0 flex-col gap-6 self-start">
            <aside className="glass relative min-w-0 rounded-[var(--radius-panel)] p-5">
              <div className="relative">
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
                        : tr(
                            "v5_home_connect_title",
                            "Connect a PS5 to get started",
                          )}
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
                {/* The helper is the one thing a known console needs to be usable, so it is sent
                    from here rather than only from Connections. */}
                {!connected && !resting && (
                  <div className="mt-4">
                    <SendHelperButton wide />
                  </div>
                )}
                <Link
                  to="/connection"
                  className="mt-4 flex min-h-12 items-center justify-between gap-2 rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] py-1.5 ps-5 pe-1.5 text-sm font-medium shadow-[var(--edge-highlight),var(--shadow-1)] transition-colors hover:bg-[var(--color-float)]"
                >
                  {connected
                    ? tr("v5_home_manage_connection", "Manage connection")
                    : tr("v5_home_connect", "Connect PS5")}
                  <ArrowRight
                    size={16}
                    aria-hidden
                    className="me-3 shrink-0 text-[var(--color-muted)] rtl:rotate-180"
                  />
                </Link>
              </div>

              <div className="relative mt-5">
                <h3 className="mb-1 text-[0.6875rem] font-medium uppercase tracking-[0.08em] text-[var(--color-muted)]">
                  {tr("v5_home_console_status", "Console status")}
                </h3>
                <p className="sr-only">
                  {tr(
                    "v5_home_console_status_desc",
                    "Connection and live system health.",
                  )}
                </p>
                <MetricRow
                  label={tr("v5_home_host", "Host")}
                  value={host || "—"}
                />
                <MetricRow
                  label={tr("v5_home_engine", "Local engine")}
                  value={engineStatus}
                  tone={engineStatus === "up" ? "good" : "bad"}
                />
                <MetricRow
                  label={tr("v5_home_helper", "PS5 helper")}
                  value={
                    resting
                      ? tr("v5_home_rest_mode", "In rest mode")
                      : payloadVersion
                        ? `v${payloadVersion}`
                        : payloadStatus
                  }
                  tone={payloadStatus === "up" ? "good" : resting ? undefined : "warn"}
                />
                <MetricRow
                  label={tr("v5_home_krw", "Kernel access")}
                  value={
                    ucredElevated === null
                      ? "—"
                      : ucredElevated
                        ? tr("v5_home_available", "Available")
                        : tr("v5_home_missing", "Unavailable")
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

              <div className="relative mt-4 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] p-4">
                <div className="mb-3 flex items-center justify-between gap-3">
                  <div>
                    <div className="text-xs font-semibold">
                      {tr("v5_home_sensors", "Live sensors")}
                    </div>
                    <div className="mt-0.5 text-[0.6875rem] text-[var(--color-muted)]">
                      {connected
                        ? tr("v5_home_sensor_live", "Updates automatically")
                        : tr(
                            "v5_home_sensor_waiting",
                            "Available after connection",
                          )}
                    </div>
                  </div>
                  {temps && cpuHistory.length >= 2 && (
                    <Sparkline
                      data={cpuHistory}
                      width={82}
                      height={26}
                      color={
                        (temps.cpu_temp ?? 0) >= 85
                          ? "var(--color-warn)"
                          : "var(--color-accent-bright)"
                      }
                      fill
                    />
                  )}
                </div>
                {temps ? (
                  <div className="grid grid-cols-2 gap-2">
                    <SensorMetric
                      label={tr("v5_home_cpu", "CPU")}
                      value={`${temps.cpu_temp?.toFixed(0) ?? "?"}°C`}
                    />
                    <SensorMetric
                      label={tr("v5_home_soc", "SoC")}
                      value={`${temps.soc_temp?.toFixed(0) ?? "?"}°C`}
                    />
                    {temps.m2_temp > 0 && (
                      <SensorMetric
                        label={tr("v5_home_m2", "M.2 SSD")}
                        value={`${temps.m2_temp.toFixed(0)}°C`}
                      />
                    )}
                    {power && (
                      <SensorMetric
                        label={tr("v5_home_lifetime", "Runtime")}
                        value={`${power.operating_time_hours ?? 0}h`}
                      />
                    )}
                  </div>
                ) : connected ? (
                  <div className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
                    <Spinner size={13} />
                    {tr("v5_home_loading_sensors", "Reading sensors…")}
                  </div>
                ) : (
                  <div className="text-xs text-[var(--color-muted)]">
                    {tr(
                      "telemetry_not_connected_desc",
                      "Connect to see temperatures and runtime.",
                    )}
                  </div>
                )}
              </div>

              {ps5Kernel && (
                <p
                  className="relative mt-3 truncate font-mono text-[0.6875rem] text-[var(--color-muted)]"
                  title={ps5Kernel}
                >
                  {ps5Kernel}
                </p>
              )}
              {runningTitleIds.size > 0 && (
                <div className="relative mt-3 flex items-center gap-2 text-xs">
                  <Power size={13} className="text-[var(--color-good)]" />
                  <span className="font-medium">
                    {tr(
                      "v5_home_running",
                      { n: runningTitleIds.size },
                      `${runningTitleIds.size} running`,
                    )}
                  </span>
                  <span className="truncate font-mono text-[var(--color-muted)]">
                    {Array.from(runningTitleIds).slice(0, 3).join(", ")}
                  </span>
                </div>
              )}
            </aside>

            <Card>
              <SectionHeading
                icon={Bell}
                title={tr("v5_home_notifications", "Notifications")}
                description={tr(
                  "v5_home_notifications_desc",
                  "Important results and warnings.",
                )}
                action={
                  <InlineLink
                    to="/notifications"
                    label={tr("v5_home_view_all", "View all")}
                  />
                }
              />
              {recentNotifs.length === 0 ? (
                <CompactEmpty
                  icon={Bell}
                  title={tr("v5_home_no_notifications", "You’re all caught up")}
                  body={tr(
                    "v5_home_no_notifications_desc",
                    "New alerts will appear here.",
                  )}
                />
              ) : (
                <ul className="space-y-2">
                  {recentNotifs.map((notification) => (
                    <li
                      key={notification.id}
                      className="rounded-2xl bg-[var(--color-surface)] px-4 py-2.5 text-xs"
                    >
                      <div className="font-semibold">{notification.title}</div>
                      {notification.body && (
                        <div className="mt-0.5 line-clamp-2 text-[var(--color-muted)]">
                          {notification.body}
                        </div>
                      )}
                    </li>
                  ))}
                </ul>
              )}
            </Card>
          </div>
        </div>

        <div className="mt-6">
          {/* Saved servers, once and always here: browse a NAS, or add the first one. */}
          <ServersCard />
        </div>
      </div>
    </div>
  );
}

/** The greeted user's current avatar, or their initials on the accent disc when the console has no
 *  readable picture (a stock PSN avatar). Decorative: the name is in the greeting beside it. */
function UserAvatar({ name, src }: { name: string; src: string | null }) {
  const box = "h-9 w-9 shrink-0 rounded-full ring-1 ring-[var(--color-border)]";
  return src ? (
    <img src={src} alt="" aria-hidden className={`${box} object-cover`} draggable={false} />
  ) : (
    <span
      aria-hidden
      className={`${box} grid place-items-center bg-[var(--color-accent-soft)] text-xs font-semibold text-[var(--color-accent-bright)]`}
    >
      {initials(name)}
    </span>
  );
}

function SectionHeading({
  icon: Icon,
  title,
  description,
  action,
}: {
  icon: LucideIcon;
  title: string;
  description: string;
  action?: React.ReactNode;
}) {
  return (
    <header className="mb-4 flex items-start gap-3">
      <span className="grid h-9 w-9 shrink-0 place-items-center rounded-full bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)]">
        <Icon size={16} />
      </span>
      <div className="min-w-0 flex-1">
        <h2 className="text-[0.9375rem] font-semibold tracking-tight">
          {title}
        </h2>
        <p className="mt-0.5 text-[0.6875rem] text-[var(--color-muted)]">
          {description}
        </p>
      </div>
      {action}
    </header>
  );
}

function MetricRow({
  label,
  value,
  tone,
}: {
  label: string;
  value: string;
  tone?: "good" | "warn" | "bad";
}) {
  const color =
    tone === "good"
      ? "text-[var(--color-good)]"
      : tone === "warn"
        ? "text-[var(--color-warn)]"
        : tone === "bad"
          ? "text-[var(--color-bad)]"
          : "text-[var(--color-text)]";
  return (
    <div className="metric-row">
      <span className="text-[var(--color-muted)]">{label}</span>
      <span
        className={`max-w-[65%] truncate font-medium tabular-nums ${color}`}
        title={value}
      >
        {value}
      </span>
    </div>
  );
}

function SensorMetric({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-2xl bg-[var(--color-surface)] px-3 py-2">
      <div className="text-[0.625rem] uppercase tracking-wide text-[var(--color-muted)]">
        {label}
      </div>
      <div className="mt-0.5 text-base font-semibold tabular-nums">{value}</div>
    </div>
  );
}

/** A main action as a plain glass tile: icon, label, and whether it can run.
 *  When it can't run yet it stays visible but inert, with the reason in place
 *  of "Ready". */
function ActionTile({
  to,
  icon: Icon,
  label,
  readiness,
}: {
  to: string;
  icon: LucideIcon;
  label: string;
  readiness: { ready: boolean; blockers: string[]; warnings: string[] };
}) {
  const tr = useTr();
  const detail = readiness.ready
    ? readiness.warnings[0] || tr("v5_qa_ready", "Ready")
    : readiness.blockers[0] || tr("v5_qa_unavailable", "Unavailable");
  // The tile is a size container and the row inside it reacts to the tile's own width
  // (see .action-tile-inner in index.css): a tile too narrow for icon + label side by side
  // stacks them, whatever made it narrow (a phone, a large Text size, a narrow window).
  const content = (
    <span className="action-tile-inner">
      <span className="grid h-10 w-10 shrink-0 place-items-center rounded-2xl bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)]">
        <Icon size={18} aria-hidden />
      </span>
      <span className="min-w-0 max-w-full flex-1">
        <span className="line-clamp-2 block text-[0.9375rem] font-medium leading-tight tracking-[-0.01em]">
          {label}
        </span>
        <span className="mt-0.5 block truncate text-[0.6875rem] text-[var(--color-muted)]">
          {detail}
        </span>
      </span>
      {readiness.ready && (
        <ChevronRight
          size={16}
          aria-hidden
          className="action-tile-chevron shrink-0 text-[var(--color-muted)] rtl:rotate-180"
        />
      )}
    </span>
  );
  if (!readiness.ready) {
    return (
      <div
        aria-disabled="true"
        title={readiness.blockers.join(" ")}
        className="action-tile"
      >
        {content}
      </div>
    );
  }
  return (
    <Link to={to} className="action-tile">
      {content}
    </Link>
  );
}

function CompactEmpty({
  icon: Icon,
  title,
  body,
}: {
  icon: LucideIcon;
  title: string;
  body: string;
}) {
  return (
    <div className="flex min-h-24 items-center gap-4 rounded-[var(--radius-card)] bg-[var(--color-surface)] px-4 py-3">
      <span className="relative grid h-11 w-11 shrink-0 place-items-center">
        <span
          aria-hidden
          className="h-11 w-11 rounded-2xl bg-[var(--color-accent-soft)] [grid-area:1/1]"
        />
        <Icon
          size={17}
          className="relative text-[var(--color-accent-bright)] [grid-area:1/1]"
        />
      </span>
      <div>
        <div className="text-xs font-semibold">{title}</div>
        <div className="mt-0.5 text-[0.6875rem] text-[var(--color-muted)]">
          {body}
        </div>
      </div>
    </div>
  );
}

function InlineLink({ to, label }: { to: string; label: string }) {
  return (
    <Link
      to={to}
      className="inline-flex shrink-0 items-center gap-1 text-xs font-semibold text-[var(--color-accent)] hover:underline"
    >
      {label}
      <ArrowRight size={13} className="rtl:rotate-180" />
    </Link>
  );
}
