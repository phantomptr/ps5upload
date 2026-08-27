import { NavLink, useLocation } from "react-router";
import {
  LayoutDashboard,
  Gamepad2,
  FolderTree,
  Cpu,
  Activity,
  MoreHorizontal,
} from "lucide-react";
import { useTr } from "../state/lang";
import { useAnyGameRunning } from "../state/runningApps";
import type { LucideIcon } from "lucide-react";

/**
 * v5 primary navigation.
 *
 * Desktop (md+): a 56px icon rail on the far left with 5 primary tabs.
 *   Each tab deep-links into a v4 screen that belongs to that v5 tab's
 *   domain. A "More" button at the bottom navigates to /more.
 *
 * Mobile (<md): a 56px bottom nav with the same 5 tabs. The mobile
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

interface TabDef {
  /** v5 tab id (also used as the i18n key suffix). */
  id: "home" | "games" | "files" | "console" | "tasks";
  /** lucide icon component. */
  icon: LucideIcon;
  /** Current v4 route to link to (will become /<id> as tabs are built). */
  to: string;
  /** Additional v4 prefixes that count as "active" for this tab. */
  matches: string[];
}

const TABS: TabDef[] = [
  {
    id: "home",
    icon: LayoutDashboard,
    to: "/home",
    matches: [
      "/home",
      "/dashboard",
      "/whats-new",
      "/connection",
      "/about",
      "/faq",
      "/settings",
      "/first-run",
    ],
  },
  {
    id: "games",
    icon: Gamepad2,
    to: "/games",
    matches: [
      "/games",
      "/library",
      "/installed",
      "/cheats",
      "/saves",
      "/screenshots",
      "/videos",
      "/game-activity",
      "/sdk-changer",
      "/search",
    ],
  },
  {
    id: "files",
    icon: FolderTree,
    to: "/files",
    matches: [
      "/files",
      "/upload",
      "/install-package",
      "/file-system",
      "/volumes",
      "/smb",
      "/smb-browser",
      "/disk-usage",
    ],
  },
  {
    id: "console",
    icon: Cpu,
    to: "/console",
    matches: [
      "/console",
      "/hardware",
      "/fan-curve",
      "/profile",
      "/backup",
      "/local-image",
      "/health",
      "/remote-play",
      "/notifications",
      "/fw-spoof",
      "/ftp-server",
      "/nanodns",
      "/nano-dns",
      "/payloads",
      "/send-payload",
      "/processes",
      "/shell",
      "/stats",
    ],
  },
  {
    id: "tasks",
    icon: Activity,
    to: "/tasks",
    matches: [
      "/tasks",
      "/activity",
      "/logs",
      "/kernel-log",
      "/audit-log",
      "/bug-report",
    ],
  },
];

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
      className="absolute right-0 top-0 h-2 w-2 animate-pulse rounded-full bg-[var(--color-good)] ring-2 ring-[var(--color-surface-2)]"
    />
  );
}

function useActiveTab(): string | null {
  const { pathname } = useLocation();
  for (const tab of TABS) {
    if (
      tab.matches.some(
        (p) => pathname === p || pathname.startsWith(p + "/"),
      )
    ) {
      return tab.id;
    }
  }
  return null;
}

/**
 * Desktop rail. 56px wide, icon + tooltip, vertically centered. Renders
 * only at md+ (the bottom nav takes over below md). The "More" button
 * at the bottom navigates to /more — the same screen the phone nav
 * uses, so the search box is available on every tier.
 */
export function TabRail() {
  const tr = useTr();
  const activeTab = useActiveTab();
  const playing = useAnyGameRunning();
  return (
    <>
      <nav
        aria-label={tr("v5_tab_primary_nav", undefined, "Primary")}
        className="hidden md:flex md:h-full md:w-14 flex-col items-center justify-center gap-1 border-r border-[var(--color-border)] bg-[var(--color-surface-2)] pt-[env(safe-area-inset-top)]"
      >
        {TABS.map((tab, i) => {
          const Icon = tab.icon;
          const active = activeTab === tab.id;
          const base = tr(`v5_tab_${tab.id}`, undefined, tab.id);
          const showPlaying = tab.id === "games" && playing;
          // The dot is decorative; the fact it stands for rides on the
          // accessible name so it is not silently lost.
          const label = showPlaying
            ? `${base} — ${tr("installed_now_playing", undefined, "Now playing")}`
            : base;
          const desc = tr(`v5_tab_${tab.id}_desc`, undefined, "");
          return (
            <NavLink
              key={tab.id}
              to={tab.to}
              aria-label={label}
              aria-current={active ? "page" : undefined}
              title={`${label}${desc ? " — " + desc : ""}`}
              accessKey={String(i + 1)}
              className={[
                "group relative flex h-11 w-11 items-center justify-center rounded-lg transition-colors",
                "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-accent)]",
                active
                  ? "bg-[var(--color-surface-3)] text-[var(--color-text)]"
                  : "text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]",
              ]
                .filter(Boolean)
                .join(" ")}
            >
              <Icon size={22} aria-hidden />
              {showPlaying && <PlayingDot />}
              {active && (
                <span
                  aria-hidden
                  className="absolute left-0 top-1/2 h-7 w-1 -translate-y-1/2 rounded-full bg-[var(--color-accent)]"
                />
              )}
              {/* Tooltip (CSS hover, title attr is the fallback) */}
              <span
                role="tooltip"
                className="pointer-events-none absolute left-full ml-3 z-50 hidden whitespace-nowrap rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-xs text-[var(--color-text)] elev-3 group-hover:block group-focus-visible:block"
              >
                {label}
                <span
                  aria-hidden
                  className="ml-1 text-[var(--color-muted)]"
                >
                  {`Alt+${i + 1}`}
                </span>
              </span>
            </NavLink>
          );
        })}

        {/* Spacer pushes More to the bottom. */}
        <div className="flex-1" />

        {/* More goes to the /more screen on every tier, not just phones.
            It used to open the desktop Sidebar in a drawer here, which
            meant tablets and desktops never got the search box the 5.1.0
            notes promised — a tablet is a touch device and was landing
            on the cramped drawer. One route, one code path. */}
        <NavLink
          to="/more"
          aria-label={tr("v5_tab_more", undefined, "More")}
          title={tr("v5_tab_more_desc", undefined, "All screens")}
          className={({ isActive }) =>
            [
              "mb-2 flex h-11 w-11 items-center justify-center rounded-lg transition-colors",
              "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-accent)]",
              isActive
                ? "bg-[var(--color-surface-3)] text-[var(--color-text)]"
                : "text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]",
            ].join(" ")
          }
        >
          <MoreHorizontal size={22} aria-hidden />
        </NavLink>
      </nav>

    </>
  );
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
        aria-label={tr("v5_tab_primary_nav", undefined, "Primary")}
        className="h-bottom-nav md:hidden fixed inset-x-0 bottom-0 z-40 flex items-stretch justify-around border-t border-[var(--color-border)] bg-[var(--color-surface-2)] pb-[env(safe-area-inset-bottom)]"
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
            <NavLink
              key={tab.id}
              to={tab.to}
              aria-label={label}
              aria-current={active ? "page" : undefined}
              className={[
                "relative flex flex-1 flex-col items-center justify-center gap-0.5 text-xs font-medium",
                "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-accent)]",
                active
                  ? "text-[var(--color-accent)]"
                  : "text-[var(--color-muted)]",
              ]
                .filter(Boolean)
                .join(" ")}
            >
              {/* The icon carries the dot, not the tab: a dot pinned to the
                  full-width tab box would float far from the glyph. */}
              <span className="relative">
                <Icon size={22} aria-hidden />
                {showPlaying && <PlayingDot />}
              </span>
              <span>{base}</span>
            </NavLink>
          );
        })}
        {/* More — a real route, not a sheet. That makes the Android
            hardware back button and the backStack treat it like any
            other screen (mobile-design §3.4), and it lets the screen
            use <main>'s scroller instead of nesting its own. */}
        <NavLink
          to="/more"
          aria-label={tr("v5_tab_more", undefined, "More")}
          className={({ isActive }) =>
            [
              "flex flex-1 flex-col items-center justify-center gap-0.5 text-xs font-medium",
              "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-accent)]",
              isActive
                ? "text-[var(--color-accent)]"
                : "text-[var(--color-muted)]",
            ].join(" ")
          }
        >
          <MoreHorizontal size={22} aria-hidden />
          <span>{tr("v5_tab_more", undefined, "More")}</span>
        </NavLink>
      </nav>
    </>
  );
}
