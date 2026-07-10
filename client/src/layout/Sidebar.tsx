import { useEffect, useState } from "react";
import { NavLink } from "react-router-dom";
import { getAppVersion } from "../lib/appVersion";
import { isTauriEnv } from "../lib/tauriEnv";
import {
  Cable,
  Upload,
  PackageOpen,
  Gamepad2,
  LibraryBig,
  Search,
  HardDrive,
  FolderTree,
  Cpu,
  CircleUserRound,
  Gauge,
  Boxes,
  Globe,
  Save,
  Image as ImageIcon,
  Video as VideoIcon,
  Settings as SettingsIcon,
  Info,
  Sun,
  Moon,
  MoonStar,
  Flower2,
  Sparkles,
  HelpCircle,
  ScrollText,
  Activity as ActivityIcon,
  BarChart3,
  TerminalSquare,
  PieChart,
  LayoutDashboard,
  ShieldCheck,
  Bug,
} from "lucide-react";
import clsx from "clsx";
import { useThemeStore } from "../state/theme";
import { useTr } from "../state/lang";
import { useLogsStore } from "../state/logs";
import { useUpdateStore } from "../state/update";
import RosterPicker from "./RosterPicker";
import NotificationInbox from "./NotificationInbox";
import type { Theme } from "../state/theme";

/** Friendly label for the active theme. Pulled out so the toggle row
 *  in the footer doesn't need a chained ternary. */
function themeLabel(
  theme: Theme,
  tr: (
    key: string,
    vars?: Record<string, string | number>,
    fallback?: string,
  ) => string,
): string {
  if (theme === "light") return tr("light_mode", undefined, "PS5 Light");
  if (theme === "oled") return tr("oled_mode", undefined, "OLED mode");
  if (theme === "rose") return tr("rose_mode", undefined, "Rose");
  return tr("dark_mode", undefined, "PS5 Dark");
}

/** Icon picker that mirrors `themeLabel`. One icon per state keeps each
 *  visually distinct: sun (PS5 Light) → moon (PS5 Dark) → moon-star
 *  (OLED) → flower (Rose). The toggle button cycles through these in order. */
function themeIcon(theme: Theme) {
  if (theme === "light") return <Sun size={14} />;
  if (theme === "oled") return <MoonStar size={14} />;
  if (theme === "rose") return <Flower2 size={14} />;
  return <Moon size={14} />;
}

interface NavItem {
  to: string;
  key: string;
  fallback: string;
  icon: typeof Cable;
  /** Optional section label — groups nav items visually. Stored as a
   *  {key, fallback} pair so the section label translates alongside
   *  the nav items. */
  section?: { key: string; fallback: string };
  /** True for screens with no browser-functional path at all (e.g. Upload
   *  requires a host OS file/folder picker with zero web equivalent) — the
   *  nav entry is hidden entirely in a browser session rather than linking
   *  to a screen that can't do anything there. */
  hideInBrowser?: boolean;
}

// 2.12.0 sidebar regroup. Previously: 3 sections (Overview / Workflow /
// Help), with Workflow being 13 flat items — a "every feature gets a
// top-level slot" anti-pattern that hid the app's story behind a wall
// of equal-weight options. Now: 5 verb-driven sections that tell a
// progression — Setup → Files → Browse → System → Diagnostics — plus
// a clear distinction between primary navigation and utility entries.
// Dashboard moved from "Overview" (it's neither setup nor first-thing)
// to System. Activity + Stats grouped under Diagnostics. Payloads sits
// in Setup next to Connection — sending the helper is a beginning-phase
// step (Connection's own step 2), so it belongs with "get started",
// not buried under System.
//
// Total nav items unchanged (no screen removed); only the grouping +
// "Payload library → Homebrew catalog" rename. The screen merges
// (Payloads + SendPayload into one tabbed screen, Volumes split, etc.)
// are separate commits that don't touch the sidebar shape.
const items: NavItem[] = [
  // ─ Setup: orient, connect, get started ─
  {
    to: "/whats-new",
    key: "whats_new",
    fallback: "What's new",
    icon: Sparkles,
    section: { key: "nav_section_setup", fallback: "Setup" },
  },
  { to: "/connection", key: "connect", fallback: "Connection", icon: Cable },
  // Payloads sits right after Connection: sending the helper (and other
  // homebrew payloads) is a beginning-phase step — it's literally step 2
  // of the Connection screen's own flow. Grouping it here matches how
  // people actually work (orient → connect → send payload → check
  // status) instead of burying it under System as a "manage what's
  // running" tool.
  {
    to: "/payloads",
    key: "payloads",
    fallback: "Payloads",
    icon: Boxes,
    // Both tabs (Catalog, Send) download/read a payload file on the HOST
    // filesystem before pushing it to the PS5's loader port — no browser
    // equivalent. Distinct from Connection's own "send the ps5upload
    // helper" step (already gated separately), which stays available.
    hideInBrowser: true,
  },
  // Dashboard lives with Setup, not System: it's the "am I connected,
  // what's running?" morning check — the thing you look at right after
  // (or instead of) the Connection screen, not a hardware tool.
  {
    to: "/dashboard",
    key: "dashboard",
    fallback: "Dashboard",
    icon: LayoutDashboard,
  },

  // ─ Files: the "send things and install things" verbs ─
  {
    to: "/upload",
    key: "upload",
    fallback: "Upload",
    icon: Upload,
    section: { key: "nav_section_files", fallback: "Files" },
    hideInBrowser: true,
  },
  {
    to: "/install-package",
    key: "install_package",
    fallback: "Install Package",
    icon: PackageOpen,
  },
  { to: "/saves", key: "saves", fallback: "Save data", icon: Save },
  {
    to: "/screenshots",
    key: "screenshots",
    fallback: "Screenshots",
    icon: ImageIcon,
  },
  { to: "/videos", key: "videos", fallback: "Video clips", icon: VideoIcon },

  // ─ Browse PS5: navigate what's on the console ─
  {
    to: "/library",
    key: "library",
    fallback: "Library",
    icon: LibraryBig,
    section: { key: "nav_section_browse", fallback: "Browse PS5" },
  },
  {
    to: "/installed",
    key: "installed_apps",
    fallback: "Installed Apps",
    icon: Gamepad2,
  },
  {
    to: "/file-system",
    key: "file_system",
    fallback: "File System",
    icon: FolderTree,
  },
  { to: "/search", key: "search", fallback: "Search", icon: Search },
  { to: "/volumes", key: "volumes", fallback: "Volumes", icon: HardDrive },
  {
    to: "/disk-usage",
    key: "disk_usage",
    fallback: "Disk usage",
    icon: PieChart,
  },

  // ─ System: observe + manage the PS5 itself ─
  {
    to: "/hardware",
    key: "hardware",
    fallback: "Hardware",
    icon: Cpu,
    section: { key: "nav_section_system", fallback: "System" },
  },
  {
    to: "/processes",
    key: "processes",
    fallback: "Processes",
    icon: Gauge,
  },
  {
    to: "/profile",
    key: "profile",
    fallback: "Profile",
    icon: CircleUserRound,
  },
  { to: "/nanodns", key: "nanodns", fallback: "nanoDNS", icon: Globe },
  { to: "/shell", key: "shell", fallback: "Shell", icon: TerminalSquare },

  // ─ Diagnostics: history, logs, debugging ─
  {
    to: "/activity",
    key: "activity",
    fallback: "Activity",
    icon: ActivityIcon,
    section: { key: "nav_section_diagnostics", fallback: "Diagnostics" },
  },
  { to: "/stats", key: "stats", fallback: "Stats", icon: BarChart3 },
  { to: "/logs", key: "logs", fallback: "Logs", icon: ScrollText },
  {
    to: "/audit-log",
    key: "audit_log",
    fallback: "Audit log",
    icon: ShieldCheck,
  },
  { to: "/bug-report", key: "bug_report", fallback: "Bug report", icon: Bug },

  // ─ Footer-style utility entries (still rendered inline for now;
  //   a future change could split them visually with a divider) ─
  {
    to: "/faq",
    key: "faq",
    fallback: "FAQ",
    icon: HelpCircle,
    section: { key: "nav_section_help", fallback: "Help" },
  },
  {
    to: "/settings",
    key: "settings",
    fallback: "Settings",
    icon: SettingsIcon,
  },
  { to: "/about", key: "about", fallback: "About", icon: Info },
];

export default function Sidebar({
  onNavigate,
}: {
  /** Called when a nav item is tapped — used by the mobile drawer to
   *  close itself after navigation. No-op on desktop (inline sidebar). */
  onNavigate?: () => void;
} = {}) {
  const { theme, toggleTheme } = useThemeStore();
  const tr = useTr();
  const errorCount = useLogsStore(
    (s) => s.entries.filter((e) => e.level === "error").length,
  );
  const updateAvailable = useUpdateStore((s) => s.phase.kind === "available");
  const [version, setVersion] = useState<string>("");
  useEffect(() => {
    getAppVersion()
      .then(setVersion)
      .catch(() => setVersion(""));
  }, []);

  return (
    <aside className="flex w-60 shrink-0 flex-col border-r border-[var(--color-border)] bg-[var(--color-surface-2)] pt-[env(safe-area-inset-top)] pb-[env(safe-area-inset-bottom)] pl-[env(safe-area-inset-left)]">
      {/* Brand header — compact, logo + name + version in a single
          row. Subtle border below separates it from the nav. */}
      <div className="flex items-center gap-3 border-b border-[var(--color-border)] px-4 py-3.5">
        <img
          src="/logo-square.png"
          alt=""
          aria-hidden
          className="h-11 w-11 shrink-0 rounded-lg"
        />
        <div className="flex min-w-0 flex-col leading-tight">
          <span className="truncate text-base font-bold tracking-tight">
            PS5Upload
          </span>
          <span className="truncate text-xs text-[var(--color-muted)]">
            {version ? `v${version}` : "—"}
          </span>
        </div>
      </div>

      {/* Multi-PS5 picker — sits between the brand header and nav.
          Always present so the user can switch consoles from any
          screen without context-switching. Migrates legacy single-
          host users to a default profile on first mount via
          ensureRosterMigrated() in AppShell. */}
      <RosterPicker />

      {/* Navigation — grouped by section. The `section` on the first
          item in a group triggers a small uppercase label above it. */}
      <nav className="min-h-0 flex-1 overflow-y-auto p-2">
        {items
          .filter((item) => !item.hideInBrowser || isTauriEnv())
          .map(({ to, key, fallback, icon: Icon, section }, idx) => {
          const isLogs = to === "/logs";
          const isSettings = to === "/settings";
          return (
            <div key={to}>
              {section && (
                <div
                  className={clsx(
                    "px-3 text-xs font-semibold uppercase tracking-wider text-[var(--color-muted)]",
                    idx === 0 ? "mb-1" : "mb-1 mt-3",
                  )}
                >
                  {tr(section.key, undefined, section.fallback)}
                </div>
              )}
              <NavLink
                to={to}
                onClick={onNavigate}
                className={({ isActive }) =>
                  clsx(
                    "group flex items-center gap-2.5 rounded-md px-3 py-1.5 text-sm transition-colors",
                    isActive
                      ? "bg-[var(--color-accent)] font-medium text-[var(--color-accent-contrast)]"
                      : "text-[var(--color-text)] hover:bg-[var(--color-surface-3)]",
                  )
                }
              >
                <Icon size={16} strokeWidth={1.75} />
                <span className="min-w-0 flex-1 truncate">
                  {tr(key, undefined, fallback)}
                </span>
                {isLogs && errorCount > 0 && (
                  <span
                    className="rounded-full bg-[var(--color-bad)] px-1.5 py-0.5 text-xs font-semibold tabular-nums text-white group-[.active]:bg-white group-[.active]:text-[var(--color-bad)]"
                    title={tr(
                      errorCount === 1
                        ? "logged_error_one"
                        : "logged_error_many",
                      { count: errorCount },
                      `${errorCount} logged error${errorCount === 1 ? "" : "s"}`,
                    )}
                  >
                    {errorCount > 99 ? "99+" : errorCount}
                  </span>
                )}
                {isSettings && updateAvailable && (
                  <span
                    className="h-2 w-2 rounded-full bg-[var(--color-accent)] group-[.active]:bg-[var(--color-accent-contrast)]"
                    aria-label={tr(
                      "update_available_short",
                      undefined,
                      "Update available",
                    )}
                    title={tr(
                      "update_available_tooltip",
                      undefined,
                      "Update available — open Settings to install",
                    )}
                  />
                )}
              </NavLink>
            </div>
          );
        })}
      </nav>

      {/* Theme toggle + notification inbox — minimal footer row.
          The inbox bell shows unread count badges; the theme toggle
          cycles Dark → Light → OLED. Both are persistent affordances
          that live across screens. */}
      <div className="flex items-center justify-between border-t border-[var(--color-border)] px-3 py-2">
        <span className="text-xs text-[var(--color-muted)]">
          {themeLabel(theme, tr)}
        </span>
        <div className="flex items-center gap-1">
          <NotificationInbox />
          <button
            type="button"
            onClick={toggleTheme}
            aria-label={tr(
              "switch_theme",
              { current: theme },
              `Switch theme (current: ${theme})`,
            )}
            className="rounded-md p-1.5 text-[var(--color-muted)] transition-colors hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
          >
            {themeIcon(theme)}
          </button>
        </div>
      </div>
    </aside>
  );
}
