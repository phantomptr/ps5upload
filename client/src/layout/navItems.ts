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
  Library,
  Upload,
  PackageOpen,
  Gamepad2,
  Search,
  FolderTree,
  Cpu,
  CircleUserRound,
  Boxes,
  Save,
  Image as ImageIcon,
  Settings as SettingsIcon,
  Info,
  Sparkles,
  HelpCircle,
  ScrollText,
  Activity as ActivityIcon,
  TerminalSquare,
  PieChart,
  LayoutDashboard,
  ShieldCheck,
  Bug,
  Archive,
  MonitorPlay,
  Bell,
  Clock,
  ShieldAlert,
  Network,
  Stethoscope,
  HardDrive,
  PackagePlus,
  Layers,
  WandSparkles,
  FilePen,
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
  /** Needs a macOS or Linux desktop (the screen drives that OS's own
   *  tools on this computer); hidden on Windows and Android. */
  macOrLinuxOnly?: boolean;
  /** Hidden unless the user turns beta features on in Settings. Reserved for
   *  screens that are still being finished — see state/betaFeatures.ts. */
  beta?: boolean;
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
  // What gets loaded onto the console sits with connecting to it.
  {
    to: "/payloads",
    key: "payloads",
    fallback: "Payloads",
    icon: Boxes,
  },

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
    to: "/connections",
    key: "v5_home_servers",
    fallback: "Servers",
    icon: Network,
  },
  {
    to: "/backup",
    key: "console_snapshots",
    fallback: "Console snapshots",
    icon: Archive,
  },

  // Play, install, and manage game-related content.
  {
    to: "/games",
    key: "v5_tab_games",
    fallback: "Games",
    icon: Gamepad2,
    section: { key: "nav_section_games_mods", fallback: "Games & content" },
  },
  {
    to: "/collection",
    key: "collection_nav",
    fallback: "Collection",
    icon: Library,
  },
  {
    to: "/install-package",
    key: "install_package",
    fallback: "Install Package",
    icon: PackageOpen,
  },
  {
    to: "/convert",
    key: "convert_games_title",
    fallback: "Convert Games",
    icon: PackagePlus,
  },
  { to: "/saves", key: "saves", fallback: "Save data", icon: Save },
  // Screenshots and video clips: one screen, a tab each.
  {
    to: "/captures",
    key: "captures",
    fallback: "Screenshots & clips",
    icon: ImageIcon,
  },
  {
    to: "/local-image",
    key: "local_image",
    fallback: "Edit image on this computer",
    icon: FilePen,
    // It attaches the image on the machine the engine runs on: in the
    // browser build that is the server, not the viewer's computer.
    hideInBrowser: true,
    macOrLinuxOnly: true,
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
    icon: WandSparkles,
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
    icon: Layers,
  },
  {
    to: "/profile",
    key: "profile",
    fallback: "Profile",
    icon: CircleUserRound,
  },
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

  // Expert-only controls.
  {
    to: "/fw-spoof",
    key: "fw_spoof_title_v2",
    fallback: "Firmware spoof check",
    icon: ShieldAlert,
    section: { key: "nav_section_advanced", fallback: "Advanced" },
  },
  {
    to: "/shell",
    key: "shell",
    fallback: "Shell",
    icon: TerminalSquare,
    hideInBrowser: true,
  },

  // ─ Diagnostics: history, logs, debugging ─
  {
    to: "/tasks",
    key: "v5_tab_tasks",
    fallback: "Tasks",
    icon: ActivityIcon,
    section: { key: "nav_section_diagnostics", fallback: "Diagnostics" },
  },
  { to: "/logs", key: "logs", fallback: "Logs", icon: ScrollText },
  {
    to: "/audit-log",
    key: "audit_log",
    fallback: "Audit log",
    icon: ShieldCheck,
  },

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

/** Home: always first in the sidebar, above the sections, and never hidden — there is always
 *  a way back to a known screen. */
export const HOME_NAV_ITEM: NavItem = {
  to: "/home",
  key: "v5_tab_home",
  fallback: "Home",
  icon: LayoutDashboard,
};

/** Bug report: right under Home, outside the sections and never hidden — when something goes
 *  wrong, the way to report it must be where anyone can find it. */
export const BUG_REPORT_NAV_ITEM: NavItem = {
  to: "/bug-report",
  key: "bug_report",
  fallback: "Bug report",
  icon: Bug,
};

/** Above every section, in this order; none can be hidden. */
export const PINNED_NAV_ITEMS: readonly NavItem[] = [HOME_NAV_ITEM, BUG_REPORT_NAV_ITEM];

/** Whether a screen is pinned (never hidden, never counted as hidden). */
export function isPinnedNav(to: string): boolean {
  return PINNED_NAV_ITEMS.some((i) => i.to === to);
}

/** Whether an item is currently visible. Beta items stay hidden until the
 *  user turns them on, which is what keeps a half-finished screen out of a
 *  sidebar that the user never asked to be a test bench. */
/** Whether any screen is in beta — Settings only offers the switch then. */
export function hasBetaItems(items: NavItem[] = NAV_ITEMS): boolean {
  return items.some((i) => i.beta);
}

export function navItemVisible(item: NavItem, betaEnabled: boolean): boolean {
  return !item.beta || betaEnabled;
}

/** Whether this build can offer the screen at all. */
export function navItemOffered(
  item: NavItem,
  where: { inBrowser: boolean; macOrLinux: boolean },
): boolean {
  if (where.inBrowser && item.hideInBrowser) return false;
  return !item.macOrLinuxOnly || where.macOrLinux;
}

/**
 * The desktop sidebar's sections: every screen in its section, minus the ones the user hid
 * (and beta screens while beta is off, and what the browser build can't offer).
 *
 * Grouped BEFORE filtering: a section's header rides on its first screen, so hiding that
 * screen and grouping afterwards would fold the rest of the section into the one above. A
 * section with nothing left in it goes.
 */
export function sidebarGroups(
  hidden: readonly string[],
  betaEnabled: boolean,
  inBrowser: boolean,
  macOrLinux = true,
): NavGroup[] {
  const shown = (i: NavItem) =>
    navItemVisible(i, betaEnabled) &&
    navItemOffered(i, { inBrowser, macOrLinux }) &&
    !hidden.includes(i.to);
  return groupNavItems(NAV_ITEMS)
    .map((g) => ({ section: g.section, items: g.items.filter(shown) }))
    .filter((g) => g.items.length > 0);
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
