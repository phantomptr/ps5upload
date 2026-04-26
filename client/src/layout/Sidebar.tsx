import { useEffect, useState } from "react";
import { NavLink } from "react-router-dom";
import { getVersion } from "@tauri-apps/api/app";
import {
  Cable,
  Upload,
  LibraryBig,
  Search,
  HardDrive,
  FolderTree,
  Cpu,
  Rocket,
  Settings as SettingsIcon,
  Info,
  Sun,
  Moon,
  Sparkles,
  HelpCircle,
  ScrollText,
  Activity as ActivityIcon,
} from "lucide-react";
import clsx from "clsx";
import { useThemeStore } from "../state/theme";
import { useTr } from "../state/lang";
import { useLogsStore } from "../state/logs";
import { useUpdateStore } from "../state/update";

interface NavItem {
  to: string;
  key: string;
  fallback: string;
  icon: typeof Cable;
  /** Optional section label — groups nav items visually. Stored as a
   *  {key, fallback} pair so the section label translates alongside
   *  the nav items. */
  section?: { key: string; fallback: string };
}

const items: NavItem[] = [
  // ─ What's new / landing ─
  { to: "/whats-new", key: "whats_new", fallback: "What's new", icon: Sparkles, section: { key: "nav_section_overview", fallback: "Overview" } },
  // ─ Workflow: get set up, send things, browse ─
  { to: "/connection", key: "connect", fallback: "Connection", icon: Cable, section: { key: "nav_section_workflow", fallback: "Workflow" } },
  { to: "/upload", key: "upload", fallback: "Upload", icon: Upload },
  { to: "/library", key: "library", fallback: "Library", icon: LibraryBig },
  { to: "/search", key: "search", fallback: "Search", icon: Search },
  { to: "/volumes", key: "volumes", fallback: "Volumes", icon: HardDrive },
  { to: "/file-system", key: "file_system", fallback: "File System", icon: FolderTree },
  { to: "/hardware", key: "hardware", fallback: "Hardware", icon: Cpu },
  { to: "/send-payload", key: "send_payload", fallback: "Send payload", icon: Rocket },
  // ─ Help / about / debug ─
  { to: "/faq", key: "faq", fallback: "FAQ", icon: HelpCircle, section: { key: "nav_section_help", fallback: "Help" } },
  { to: "/activity", key: "activity", fallback: "Activity", icon: ActivityIcon },
  { to: "/logs", key: "logs", fallback: "Logs", icon: ScrollText },
  { to: "/settings", key: "settings", fallback: "Settings", icon: SettingsIcon },
  { to: "/about", key: "about", fallback: "About", icon: Info },
];

export default function Sidebar() {
  const { theme, toggleTheme } = useThemeStore();
  const tr = useTr();
  const errorCount = useLogsStore(
    (s) => s.entries.filter((e) => e.level === "error").length,
  );
  const updateAvailable = useUpdateStore(
    (s) => s.phase.kind === "available",
  );
  const [version, setVersion] = useState<string>("");
  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => setVersion(""));
  }, []);

  return (
    <aside className="flex w-60 shrink-0 flex-col border-r border-[var(--color-border)] bg-[var(--color-surface-2)]">
      {/* Brand header — compact, logo + name + version in a single
          row. Subtle border below separates it from the nav. */}
      <div className="flex items-center gap-2.5 border-b border-[var(--color-border)] px-4 py-4">
        <img
          src="/logo-square.png"
          alt=""
          aria-hidden
          className="h-9 w-9 rounded-md"
        />
        <div className="flex min-w-0 flex-col leading-tight">
          <span className="truncate text-sm font-semibold">PS5Upload</span>
          <span className="truncate text-[11px] text-[var(--color-muted)]">
            {version ? `v${version}` : "—"}
          </span>
        </div>
      </div>

      {/* Navigation — grouped by section. The `section` on the first
          item in a group triggers a small uppercase label above it. */}
      <nav className="flex-1 overflow-y-auto p-2">
        {items.map(({ to, key, fallback, icon: Icon, section }, idx) => {
          const isLogs = to === "/logs";
          const isSettings = to === "/settings";
          return (
            <div key={to}>
              {section && (
                <div
                  className={clsx(
                    "px-3 text-[10px] font-semibold uppercase tracking-wider text-[var(--color-muted)]",
                    idx === 0 ? "mb-1" : "mb-1 mt-3",
                  )}
                >
                  {tr(section.key, undefined, section.fallback)}
                </div>
              )}
              <NavLink
                to={to}
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
                    className="rounded-full bg-[var(--color-bad)] px-1.5 py-0.5 text-[10px] font-semibold tabular-nums text-white group-[.active]:bg-white group-[.active]:text-[var(--color-bad)]"
                    title={tr(
                      errorCount === 1 ? "logged_error_one" : "logged_error_many",
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
                    aria-label={tr("update_available_short", undefined, "Update available")}
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

      {/* Theme toggle — minimal footer row. Muted label + icon. */}
      <div className="flex items-center justify-between border-t border-[var(--color-border)] px-3 py-2">
        <span className="text-xs text-[var(--color-muted)]">
          {theme === "dark"
            ? tr("dark_mode", undefined, "Dark mode")
            : tr("light_mode", undefined, "Light mode")}
        </span>
        <button
          type="button"
          onClick={toggleTheme}
          aria-label={
            theme === "dark"
              ? tr("switch_light_mode")
              : tr("switch_dark_mode")
          }
          className="rounded-md p-1.5 text-[var(--color-muted)] transition-colors hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
        >
          {theme === "dark" ? <Sun size={14} /> : <Moon size={14} />}
        </button>
      </div>
    </aside>
  );
}
