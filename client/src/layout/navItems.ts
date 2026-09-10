/**
 * Canonical navigation catalogue — the single source of truth for every
 * screen the app can reach from a nav surface.
 *
 * This used to live inside `Sidebar.tsx`, fused with the desktop rail's
 * rendering, collapse state and brand header. That made it unreusable:
 * the mobile "More" surface could not get at the data without inheriting
 * a fixed-width, hover-driven desktop component (which is exactly what it
 * did, and why the mobile sheet was broken).
 *
 * Data and pure helpers only — no React, no styling. Consumers:
 *   - `Sidebar.tsx`        focused desktop primary navigation
 *   - `screens/More`       mobile full-screen nav
 */
import type { LucideIcon } from "lucide-react";
import {
  Cable,
  Upload,
  PackageOpen,
  Gamepad2,
  LibraryBig,
  Search,
  FolderTree,
  Cpu,
  CircleUserRound,
  Gauge,
  Boxes,
  Save,
  Image as ImageIcon,
  Video as VideoIcon,
  Settings as SettingsIcon,
  Info,
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
  Archive,
  MonitorPlay,
  Fan,
  Bell,
  Clock,
  ShieldAlert,
  Server,
  Network,
  Stethoscope,
  HardDrive,
} from "lucide-react";

export interface NavItem {
  to: string;
  key: string;
  fallback: string;
  icon: LucideIcon;
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

export type NavGroup = {
  section: NonNullable<NavItem["section"]>;
  items: NavItem[];
};

/** Shape of the i18n `tr` function, so this module stays React-free. */
export type TrFn = (
  key: string,
  vars?: Record<string, string | number>,
  fallback?: string,
) => string;

// More-menu information architecture. Group by the job a user is trying to
// complete, not by which protocol or implementation owns the screen.
export const NAV_ITEMS: NavItem[] = [
  {
    to: "/whats-new",
    key: "whats_new",
    fallback: "What's new",
    icon: Sparkles,
    section: { key: "nav_section_setup", fallback: "Setup" },
  },
  { to: "/connection", key: "connect", fallback: "Connection", icon: Cable },

  // Move data and inspect storage.
  {
    to: "/upload",
    key: "upload",
    fallback: "Upload",
    icon: Upload,
    section: { key: "nav_section_files", fallback: "Files & storage" },
  },
  {
    to: "/files",
    key: "v5_tab_files",
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
  {
    to: "/smb-browser",
    key: "smb_title",
    fallback: "SMB Browser",
    icon: Network,
  },
  {
    to: "/ftp-server",
    key: "ftp_title",
    fallback: "FTP Server",
    icon: Server,
  },
  { to: "/backup", key: "backup", fallback: "Backup", icon: Archive },

  // Play, install, and manage game-related content.
  {
    to: "/games",
    key: "v5_tab_games",
    fallback: "Games",
    icon: LibraryBig,
    section: { key: "nav_section_games_mods", fallback: "Games & content" },
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
  {
    to: "/local-image",
    key: "local_image",
    fallback: "Edit Game Image",
    icon: HardDrive,
  },
  {
    to: "/game-activity",
    key: "game_activity_title",
    fallback: "Game Activity",
    icon: Clock,
  },
  {
    to: "/cheats",
    key: "cheats_title",
    fallback: "Cheats",
    icon: Gamepad2,
  },
  // Observe and manage the selected console.
  {
    to: "/console",
    key: "v5_tab_console",
    fallback: "Console",
    icon: Cpu,
    section: { key: "nav_section_console", fallback: "Console" },
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
  { to: "/fan-curve", key: "fan_curve", fallback: "Fan Curve", icon: Fan },
  {
    to: "/health",
    key: "health",
    fallback: "Health Check",
    icon: Stethoscope,
  },
  {
    to: "/remote-play",
    key: "remote_play",
    fallback: "Remote Play",
    icon: MonitorPlay,
  },
  {
    to: "/notifications",
    key: "notifications_screen",
    fallback: "Notifications",
    icon: Bell,
  },

  // Interfaces belonging to payloads, together in one workspace.
  {
    to: "/payloads",
    key: "payloads",
    fallback: "Payloads",
    icon: Boxes,
    section: { key: "nav_section_payload_tools", fallback: "Payload tools" },
    hideInBrowser: true,
  },

  // Expert-only controls.
  {
    to: "/fw-spoof",
    key: "fw_spoof_title",
    fallback: "FW Spoof",
    icon: ShieldAlert,
    section: { key: "nav_section_advanced", fallback: "Advanced" },
  },
  { to: "/shell", key: "shell", fallback: "Shell", icon: TerminalSquare },

  // ─ Diagnostics: history, logs, debugging ─
  {
    to: "/tasks",
    key: "v5_tab_tasks",
    fallback: "Tasks",
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

  // ─ Help ─
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

/** The one destination that is always in the sidebar.
 *
 * Home is deliberately not a favorite: it cannot be unstarred, so the
 * sidebar can never end up empty and there is always a way back to a
 * known screen. Everything else is the user's choice — see
 * `resolveFavorites`. The section label lives here so `groupNavItems`
 * has a header to open the group with.
 */
export const HOME_NAV_ITEM: NavItem = {
  to: "/home",
  key: "v5_tab_home",
  fallback: "Home",
  icon: LayoutDashboard,
  section: { key: "nav_section_favorites", fallback: "Favorites" },
};

/**
 * Resolve stored favorite route paths into real nav items.
 *
 * Unknown paths are DROPPED rather than rendered. Favorites are persisted
 * per-machine and outlive the build that wrote them, so a screen that is
 * later renamed or removed would otherwise stay pinned in someone's
 * sidebar forever, linking nowhere. Order follows the stored list (the
 * order the user starred things in), and Home is filtered out so it can
 * never appear twice.
 */
export function resolveFavorites(paths: readonly string[]): NavItem[] {
  const byPath = new Map(NAV_ITEMS.map((item) => [item.to, item]));
  const seen = new Set<string>([HOME_NAV_ITEM.to]);
  const out: NavItem[] = [];
  for (const path of paths) {
    if (seen.has(path)) continue;
    const item = byPath.get(path);
    if (!item) continue;
    seen.add(path);
    // Strip any section header the item carries in the More list — inside
    // Favorites it is a plain row under the Favorites header, not the start
    // of a new group.
    const { section: _section, ...rest } = item;
    out.push(rest);
  }
  return out;
}

/**
 * Collapse a flat item list into sections.
 *
 * An item carrying a `section` opens a new group; every item after it
 * joins that group until the next sectioned item. Items appearing before
 * the first section header are dropped — `NAV_ITEMS[0]` always carries
 * one, which a unit test asserts.
 */
export function groupNavItems(items: NavItem[]): NavGroup[] {
  const acc: NavGroup[] = [];
  for (const item of items) {
    if (item.section) {
      acc.push({ section: item.section, items: [item] });
    } else {
      acc[acc.length - 1]?.items.push(item);
    }
  }
  return acc;
}

/** Strip diacritics and case so "sauvegardes" matches "Sauvegardés". */
function norm(s: string): string {
  return s
    .normalize("NFD")
    .replace(/\p{Diacritic}/gu, "")
    .toLocaleLowerCase();
}

/**
 * Filter nav items by a free-text query.
 *
 * Matches BOTH the translated label and the English fallback. Most of the
 * community documentation for this app uses the English screen names, so
 * someone on a Japanese locale must still be able to type "hardware" and
 * land on Hardware. An empty or whitespace-only query returns everything.
 */
export function filterNavItems(
  items: NavItem[],
  query: string,
  tr: TrFn,
): NavItem[] {
  const q = norm(query.trim());
  if (!q) return items;
  return items.filter((item) => {
    const translated = norm(tr(item.key, undefined, item.fallback));
    const english = norm(item.fallback);
    return translated.includes(q) || english.includes(q);
  });
}
