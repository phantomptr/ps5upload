// Tabs are Links, not NavLinks: a NavLink sets aria-current from its own path alone, so the
// Games tab told screen readers nothing while Collection, Saves or Captures was open.
import { Link, useLocation } from "react-router";
import {
  LayoutDashboard,
  Gamepad2,
  FolderTree,
  Cpu,
  MonitorPlay,
  MoreHorizontal,
} from "lucide-react";
import { useTr } from "../state/lang";
import { useAnyGameRunning } from "../state/runningApps";
import type { LucideIcon } from "lucide-react";
import { NAV_ITEMS, groupNavItems } from "./navItems";

/**
 * v5 primary navigation for phones.
 *
 * Mobile (<md): a floating glass pill bar with 5 primary tabs. The mobile
 *   top-bar hamburger is replaced by this nav; the "More" tab navigates
 *   to /more. Both tiers used to render the desktop Sidebar in an
 *   overlay — a 270px column in a 448px sheet on phones, and a drawer
 *   with no search on tablet/desktop. One route replaces both.
 *
 * Routing note: each tab links to the *current* v4 route that best
 *   represents that v5 tab. As Phase 5.1 builds each new tab shell,
 *   these targets will switch to the new `/home`, `/games`, `/files`,
 *   `/console`, `/tasks` routes.
 */

export type TabId = "home" | "games" | "play" | "files" | "console";

interface TabDef {
  /** v5 tab id (also used as the i18n key suffix). */
  id: TabId;
  /** lucide icon component. */
  icon: LucideIcon;
  /** Route the tab links to. */
  to: string;
  /** The sidebar sections (navItems' section keys) whose screens light this tab up, so the
   *  tabs file every screen where the sidebar does. */
  sections: string[];
  /** Routes outside the sidebar that still belong here: redirects, sub-screens, pinned items. */
  extra: string[];
}

const TAB_DEFS: TabDef[] = [
  {
    id: "home",
    icon: LayoutDashboard,
    to: "/home",
    sections: ["nav_section_connect", "nav_section_app"],
    extra: ["/home", "/dashboard", "/first-run", "/bug-report"],
  },
  {
    id: "games",
    icon: Gamepad2,
    to: "/games",
    sections: ["nav_section_games_mods"],
    extra: ["/library", "/installed", "/screenshots", "/videos"],
  },
  // Play (10.0.0): a tab of its own, where Activity was (Activity is in More).
  {
    id: "play",
    icon: MonitorPlay,
    to: "/play",
    sections: [],
    extra: ["/play"],
  },
  {
    id: "files",
    icon: FolderTree,
    to: "/files",
    sections: ["nav_section_files"],
    extra: ["/file-system"],
  },
  {
    id: "console",
    icon: Cpu,
    to: "/console",
    sections: ["nav_section_console"],
    extra: ["/hardware", "/fan-curve", "/nanodns", "/nano-dns", "/send-payload", "/shadowmount"],
  },
];

/** Each tab with the routes that count as "on" it: its sidebar sections' screens, then its extras. */
const TABS = TAB_DEFS.map((tab) => ({
  ...tab,
  matches: [
    ...groupNavItems(NAV_ITEMS)
      .filter((g) => tab.sections.includes(g.section.key))
      .flatMap((g) => g.items.map((i) => i.to)),
    ...tab.extra,
  ],
}));

/** The tab a path belongs to, or null (e.g. /more). */
export function tabForPath(pathname: string): TabId | null {
  for (const tab of TABS) {
    if (tab.matches.some((p) => pathname === p || pathname.startsWith(p + "/"))) {
      return tab.id;
    }
  }
  return null;
}

/**
 * The "a game is running" dot on the Games tab.
 *
 * Navigation is the only surface visible from every screen, which is
 * exactly why the cue belongs here: a game keeps running while the user is
 * off looking at sensors or logs, and until now nothing told them so
 * outside the Games grid itself. Deliberately a dot and not a count —
 * the console runs one game at a time, and the number would be noise.
 *
 * `aria-hidden` with the state carried in the tab's `title`/label instead:
 * a bare dot announces nothing useful to a screen reader.
 */
function PlayingDot() {
  return (
    <span
      aria-hidden
      className="absolute right-0 top-0 h-2 w-2 animate-pulse rounded-full bg-[var(--color-good)] ring-2 ring-[var(--color-float)]"
    />
  );
}

function useActiveTab(): string | null {
  return tabForPath(useLocation().pathname);
}
/**
 * Mobile bottom nav. Renders only below md. 5 labeled icon tabs. The "More"
 * tab is a normal route containing every legacy screen, so browser and Android
 * back navigation behave consistently.
 */
export function TabBottomNav() {
  const tr = useTr();
  const activeTab = useActiveTab();
  const playing = useAnyGameRunning();

  return (
    <>
      <nav
        data-bottom-nav
        aria-label={tr("v5_tab_primary_nav", undefined, "Primary")}
        // Floats above the gesture bar: the safe area is a margin here, not
        // padding, so the pill keeps its shape on every phone.
        className="glass-float elev-2 md:hidden fixed inset-x-3 bottom-[calc(var(--safe-bottom)_+_0.6rem)] z-40 flex h-16 items-stretch justify-around gap-0.5 rounded-full p-1.5"
      >
        {TABS.map((tab) => {
          const Icon = tab.icon;
          const active = activeTab === tab.id;
          const base = tr(`v5_tab_${tab.id}`, undefined, tab.id);
          const showPlaying = tab.id === "games" && playing;
          const label = showPlaying
            ? `${base} — ${tr("installed_now_playing", undefined, "Now playing")}`
            : base;
          return (
            <Link
              key={tab.id}
              to={tab.to}
              aria-label={label}
              aria-current={active ? "page" : undefined}
              className={[
                // `min-w-0` + a truncating label: six tabs share a phone's width, and a
                // long translation ("Konsole", "Aufgaben") otherwise widens its own tab
                // until the labels touch.
                "nav-pill relative flex min-w-0 flex-1 flex-col items-center justify-center gap-0.5 rounded-full text-[0.6875rem] font-medium",
                "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-accent)]",
              ]
                .filter(Boolean)
                .join(" ")}
            >
              {/* The icon carries the dot, not the tab: a dot pinned to the
                  full-width tab box would float far from the glyph. */}
              <span className="relative">
                <Icon size={21} strokeWidth={1.8} className="nav-pill-icon" aria-hidden />
                {showPlaying && <PlayingDot />}
              </span>
              <span className="max-w-full truncate px-0.5">{base}</span>
            </Link>
          );
        })}
        {/* More — a real route, not a sheet. That makes the Android
            hardware back button and router history treat it like any
            other screen (mobile-design §3.4), and it lets the screen
            use <main>'s scroller instead of nesting its own. */}
        {/* More lights for /more and for the screens only it reaches (Activity, since Play took
            that tab in 10.0.0). */}
        <Link
          to="/more"
          aria-label={tr("v5_tab_more", undefined, "More")}
          aria-current={activeTab === null ? "page" : undefined}
          className={[
            "nav-pill flex min-w-0 flex-1 flex-col items-center justify-center gap-0.5 rounded-full text-[0.6875rem] font-medium",
            "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-accent)]",
          ].join(" ")}
        >
          <MoreHorizontal size={21} strokeWidth={1.8} className="nav-pill-icon" aria-hidden />
          <span className="max-w-full truncate px-0.5">{tr("v5_tab_more", undefined, "More")}</span>
        </Link>
      </nav>
    </>
  );
}
