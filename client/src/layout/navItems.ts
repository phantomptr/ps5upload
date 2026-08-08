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
 *   - `Sidebar.tsx`        desktop rail + drawer
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
  Layers,
  Database,
  ShieldAlert,
  Server,
  Network,
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

// v4.0.0 sidebar regroup. Consolidated from 6 sections with a duplicate
// "System" header into clean sections: Setup → Files → Browse → System →
// Diagnostics → Help. The old sidebar had two separate "System" blocks
// which was confusing — all system-management items now live under a
// single System heading.
export const NAV_ITEMS: NavItem[] = [
  // ─ Setup: orient, connect, get started ─
  {
    to: "/whats-new",
    key: "whats_new",
    fallback: "What's new",
    icon: Sparkles,
    section: { key: "nav_section_setup", fallback: "Setup" },
  },
  { to: "/connection", key: "connect", fallback: "Connection", icon: Cable },
  {
    to: "/payloads",
    key: "payloads",
    fallback: "Payloads",
    icon: Boxes,
    hideInBrowser: true,
  },
  {
    to: "/dashboard",
    key: "dashboard",
    fallback: "Dashboard",
    icon: LayoutDashboard,
  },

  // ─ Files: upload, install, manage saves & captures ─
  {
    to: "/upload",
    key: "upload",
    fallback: "Upload",
    icon: Upload,
    section: { key: "nav_section_files", fallback: "Files" },
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
    to: "/games",
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
    to: "/files",
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
    to: "/console",
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
  { to: "/fan-curve", key: "fan_curve", fallback: "Fan Curve", icon: Fan },
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
  {
    to: "/cheats",
    key: "cheats_title",
    fallback: "Cheats",
    icon: Gamepad2,
  },
  {
    to: "/game-activity",
    key: "game_activity_title",
    fallback: "Game Activity",
    icon: Clock,
  },
  {
    to: "/sdk-changer",
    key: "sdk_changer_title",
    fallback: "SDK Changer",
    icon: Layers,
  },
  {
    to: "/tmdb",
    key: "tmdb_title",
    fallback: "TMDB",
    icon: Database,
  },
  {
    to: "/fw-spoof",
    key: "fw_spoof_title",
    fallback: "FW Spoof",
    icon: ShieldAlert,
  },
  {
    to: "/ftp-server",
    key: "ftp_title",
    fallback: "FTP Server",
    icon: Server,
  },
  {
    to: "/smb-browser",
    key: "smb_title",
    fallback: "SMB Browser",
    icon: Network,
  },
  { to: "/backup", key: "backup", fallback: "Backup", icon: Archive },
  { to: "/nanodns", key: "nanodns", fallback: "nanoDNS", icon: Globe },
  { to: "/shell", key: "shell", fallback: "Shell", icon: TerminalSquare },

  // ─ Diagnostics: history, logs, debugging ─
  {
    to: "/tasks",
    key: "transfer_log_title",
    fallback: "Transfer Log",
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
