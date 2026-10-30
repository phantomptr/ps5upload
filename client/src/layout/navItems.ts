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
  Send as SendIcon,
  Gamepad2,
  FolderTree,
  Cpu,
  CircleUserRound,
  MonitorPlay,
  Boxes,
  Save,
  Image as ImageIcon,
  Settings as SettingsIcon,
  HelpCircle,
  Activity as ActivityIcon,
  TerminalSquare,
  LayoutDashboard,
  Bug,
  Bell,
  HardDrive,
  Hammer,
  WandSparkles,
  Eraser,
  FolderSync,
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
  // Play, install and manage games: the most used screens first.
  {
    to: "/games",
    key: "v5_tab_games",
    fallback: "Games",
    icon: Gamepad2,
    section: { key: "nav_section_games_mods", fallback: "Games & Content" },
  },
  // Every way something gets onto the console: from this computer or a server (any file,
  // folder, image, archive or package), from a link, or what is already on the PS5's drives.
  {
    to: "/send",
    key: "send_title_v2",
    fallback: "Upload & Install",
    icon: SendIcon,
  },
  // Everything done to a game on this computer: convert it (package, image, folder), look
  // inside an image or package and edit it, or open an image as a drive. Offered everywhere:
  // the engine does the work; only "Open as a Drive" needs a macOS or Linux desktop, and the
  // screen hides that part by itself (#479).
  {
    to: "/workshop",
    key: "workshop_title",
    fallback: "Workshop",
    icon: Hammer,
  },
  { to: "/saves", key: "saves", fallback: "Save Data", icon: Save },
  {
    to: "/cheats",
    key: "cheats_title",
    fallback: "Cheats",
    icon: WandSparkles,
  },
  // Screenshots and video clips: one screen, a tab each.
  {
    to: "/captures",
    key: "captures",
    fallback: "Screenshots & Clips",
    icon: ImageIcon,
  },

  // Browse the console's files and look after the drives.

  {
    to: "/files",
    key: "v5_tab_files",
    fallback: "File System",
    icon: FolderTree,
    section: { key: "nav_section_files", fallback: "Files & Storage" },
  },
  // 8.0.0: a folder kept the same on every device, at home on the console.
  // Folders reached over the network: your NAS and servers, and Mesh Drives (8.0.0) kept the
  // same on every device with their home on the console.
  {
    to: "/shared-folders",
    key: "shared_folders_title",
    fallback: "Shared Folders",
    icon: FolderSync,
  },
  { to: "/volumes", key: "volumes", fallback: "Volumes", icon: HardDrive },
  {
    to: "/cleanup",
    key: "cleanup_nav",
    fallback: "Clean Up",
    icon: Eraser,
  },

  // The console itself: its hardware, what runs on it, who is signed in, playing it remotely.
  {
    to: "/console",
    key: "v5_tab_console",
    fallback: "Console",
    icon: Cpu,
    section: { key: "nav_section_console", fallback: "Console" },
  },
  // What gets loaded onto the console, and what runs there (processes and the ports in use).
  {
    to: "/payloads",
    key: "payloads_title",
    fallback: "Payloads",
    icon: Boxes,
  },
  // The console's users and accounts, their avatars and the offline sign-in (Play is at the top,
  // with Home: see PLAY_NAV_ITEM).
  {
    to: "/accounts",
    key: "accounts_title",
    fallback: "Accounts",
    icon: CircleUserRound,
  },

  // Reaching the console: connecting, the devices that use it, and checking the link.
  {
    to: "/connection",
    key: "connect",
    fallback: "Connection",
    icon: Cable,
    section: { key: "nav_section_connect", fallback: "Connect" },
  },

  // What happened: history, logs, notifications, debugging.
  {
    to: "/tasks",
    key: "activity_logs_title",
    fallback: "Activity & Logs",
    icon: ActivityIcon,
    section: { key: "nav_section_diagnostics", fallback: "Diagnostics" },
  },
  {
    to: "/notifications",
    key: "notifications_screen",
    fallback: "Notifications",
    icon: Bell,
  },
  // A console shell is for debugging: it sits with the other diagnostics.
  {
    to: "/shell",
    key: "shell",
    fallback: "Shell",
    icon: TerminalSquare,
    hideInBrowser: true,
  },

  // The app itself.
  {
    to: "/settings",
    key: "settings",
    fallback: "Settings",
    icon: SettingsIcon,
    section: { key: "nav_section_app", fallback: "App" },
  },
  {
    to: "/faq",
    key: "help_title",
    fallback: "Help",
    icon: HelpCircle,
  },
];

/** Home: always first in the sidebar, above the sections, and never hidden — there is always
 *  a way back to a known screen. */
export const HOME_NAV_ITEM: NavItem = {
  to: "/home",
  key: "v5_tab_home",
  fallback: "Home",
  icon: LayoutDashboard,
};

/** Play (10.0.0): the console's screen streamed here, the app's centre. Right under Home,
 *  outside the sections and never hidden; everything else works without it. */
export const PLAY_NAV_ITEM: NavItem = {
  to: "/play",
  key: "play_title",
  fallback: "Play",
  icon: MonitorPlay,
};

/** Bug report: under Home and Play, outside the sections and never hidden — when something goes
 *  wrong, the way to report it must be where anyone can find it. */
export const BUG_REPORT_NAV_ITEM: NavItem = {
  to: "/bug-report",
  key: "bug_report",
  fallback: "Bug Report",
  icon: Bug,
};

/** Above every section, in this order; none can be hidden. */
export const FIXED_NAV_ITEMS: readonly NavItem[] = [HOME_NAV_ITEM, PLAY_NAV_ITEM, BUG_REPORT_NAV_ITEM];

/** Whether a screen is pinned (never hidden, never counted as hidden). */
export function isFixedNav(to: string): boolean {
  return FIXED_NAV_ITEMS.some((i) => i.to === to);
}

/** At most this many screens in the user's Pinned group, counting the ones it shows (a pin
 *  this build doesn't show now — a beta screen with beta off — doesn't take a place). */
export const MAX_PINNED = 12;

/** Whether the user can pin this screen: any screen in the sections. Home, Play and Bug report are
 *  already always at the top, so pinning them would only duplicate them. */
export function isPinnableNav(to: string): boolean {
  return !isFixedNav(to) && NAV_ITEMS.some((i) => i.to === to);
}

/**
 * A stored pin list made safe to use: strings only, screens that still exist (a removed or
 * renamed screen's id is dropped), no repeats, pin order kept. Not capped: which pins count
 * toward `MAX_PINNED` depends on what is shown, which this can't know.
 */
export function cleanPinned(list: readonly unknown[]): string[] {
  const out: string[] = [];
  for (const raw of list) {
    if (typeof raw !== "string") continue;
    const v = navMoved(raw);
    if (out.includes(v) || !isPinnableNav(v)) continue;
    out.push(v);
  }
  return out;
}

/**
 * Screens folded into another one, by their old route. A pin, a hidden entry or a bookmark
 * for the old screen carries over to the screen that holds it now.
 */
export const NAV_MOVED: Readonly<Record<string, string>> = {
  "/profile": "/accounts",
  "/remote-play": "/accounts",
  "/mesh-drive": "/shared-folders",
  "/connections": "/shared-folders",
  "/convert": "/workshop",
  "/local-image": "/workshop",
  "/upload": "/send",
  "/install-package": "/send",
  // Tabs of Connection (?tab=devices, ?tab=health).
  "/devices": "/connection",
  "/health": "/connection",
};

/** The route a screen lives at now: its new home if it was folded into another, else itself. */
export function navMoved(to: string): string {
  return NAV_MOVED[to] ?? to;
}

/**
 * A stored hidden list made current: old routes become the screen that holds them now. A
 * screen made from several old ones stays hidden only if the user hid every one of them,
 * so nothing someone used disappears from the sidebar.
 */
export function cleanHidden(list: readonly unknown[]): string[] {
  const given = new Set(list.filter((v): v is string => typeof v === "string"));
  const out: string[] = [];
  for (const v of given) {
    const to = navMoved(v);
    if (out.includes(to)) continue;
    const sources = Object.keys(NAV_MOVED).filter((k) => NAV_MOVED[k] === to);
    if (to !== v && !sources.every((k) => given.has(k))) continue;
    out.push(to);
  }
  return out;
}

/**
 * The user's Pinned group, in pin order: the pinned screens this build can show now (beta and
 * platform rules still apply). Hiding does not: a pinned screen shows here even when hidden
 * from its section.
 */
export function pinnedNavItems(
  pinned: readonly string[],
  betaEnabled: boolean,
  inBrowser: boolean,
  macOrLinux = true,
): NavItem[] {
  return pinned
    .map((to) => NAV_ITEMS.find((i) => i.to === to))
    .filter(
      (i): i is NavItem =>
        !!i && navItemVisible(i, betaEnabled) && navItemOffered(i, { inBrowser, macOrLinux }),
    );
}

/** Pinned screens first (in pin order), then the rest in their usual order — the command
 *  palette's screen list. */
export function pinnedFirst<T extends { to: string }>(items: readonly T[], pinned: readonly string[]): T[] {
  const rank = (i: T) => {
    const at = pinned.indexOf(i.to);
    return at === -1 ? Number.MAX_SAFE_INTEGER : at;
  };
  return items
    .map((item, index) => ({ item, index }))
    .sort((a, b) => rank(a.item) - rank(b.item) || a.index - b.index)
    .map((x) => x.item);
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
