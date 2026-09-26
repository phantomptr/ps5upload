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
  Network,
  Stethoscope,
  HardDrive,
  PackagePlus,
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
    key: "connections_title",
    fallback: "Connections",
    icon: Network,
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
  {
    to: "/convert",
    key: "fpkg_title",
    fallback: "Convert to FPKG",
    icon: PackagePlus,
    beta: true,
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
 * Home and About are deliberately not favorites: neither can be unstarred,
 * so the sidebar can never end up empty and there is always a way back to a
 * known screen AND to the links/help a newcomer needs. Everything else is
 * the user's choice — see `resolveFavorites`. The section label lives on the
 * first item so `groupNavItems` has a header to open the group with.
 */
export const HOME_NAV_ITEM: NavItem = {
  to: "/home",
  key: "v5_tab_home",
  fallback: "Home",
  icon: LayoutDashboard,
  section: { key: "nav_section_favorites", fallback: "Favorites" },
};

/** Pinned at the BOTTOM of the sidebar. About carries the project links
 *  (GitHub, Discord, X), the changelog and the disclaimer — the things
 *  someone new needs to find without first learning that More exists — but it
 *  is reference material, so it sits below the user's favourites rather than
 *  pushing them down. No `section`: it joins the group Home opens rather than
 *  starting another, which is what keeps it inside the same list. */
export const ABOUT_NAV_ITEM: NavItem = {
  to: "/about",
  key: "about",
  fallback: "About",
  icon: Info,
};

/** The rows that are always present. Order here is for de-duplication only —
 *  the sidebar composes the render order itself (Home, favourites, About). */
export const PERMANENT_NAV_ITEMS: readonly NavItem[] = [
  HOME_NAV_ITEM,
  ABOUT_NAV_ITEM,
];

/**
 * Resolve stored favorite route paths into real nav items.
 *
 * Unknown paths are DROPPED rather than rendered. Favorites are persisted
 * per-machine and outlive the build that wrote them, so a screen that is
 * later renamed or removed would otherwise stay pinned in someone's
 * sidebar forever, linking nowhere. Order follows the stored list (the
 * order the user starred things in), and the permanent rows are filtered out
 * so they can never appear twice.
 */
/** Whether an item is currently visible. Beta items stay hidden until the
 *  user turns them on, which is what keeps a half-finished screen out of a
 *  sidebar that the user never asked to be a test bench. */
export function navItemVisible(item: NavItem, betaEnabled: boolean): boolean {
  return !item.beta || betaEnabled;
}

export function resolveFavorites(
  paths: readonly string[],
  betaEnabled = false,
): NavItem[] {
  const byPath = new Map(NAV_ITEMS.map((item) => [item.to, item]));
  const seen = new Set<string>(PERMANENT_NAV_ITEMS.map((i) => i.to));
  const out: NavItem[] = [];
  for (const path of paths) {
    if (seen.has(path)) continue;
    const item = byPath.get(path);
    if (!item || !navItemVisible(item, betaEnabled)) continue;
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
 * The sidebar's Favorites list, in render order: Home first, the user's
 * starred screens in the order they starred them, About last.
 *
 * About is pinned to the BOTTOM rather than beside Home because it is
 * reference material — you go there once to find the links and the
 * changelog, not on the way to anything else — so it should not push the
 * favourites down. It carries no `section`, which is what keeps it inside
 * the group Home opens instead of starting a second one.
 */
export function sidebarNavItems(
  favorites: readonly string[],
  betaEnabled = false,
): NavItem[] {
  return [
    HOME_NAV_ITEM,
    ...resolveFavorites(favorites, betaEnabled),
    ABOUT_NAV_ITEM,
  ];
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
