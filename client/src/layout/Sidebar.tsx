import { useEffect, useMemo, useState } from "react";
import { NavLink } from "react-router";
import { ChevronDown, ChevronLeft, ChevronRight, EyeOff, LayoutGrid } from "lucide-react";

import { getAppVersion } from "../lib/appVersion";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { useTr } from "../state/lang";
import { useLogsStore } from "../state/logs";
import { useUpdateStore } from "../state/update";
import { useNavSidebarStore } from "../state/navSidebar";
import { isTauriEnv } from "../lib/tauriEnv";
import { useBetaFeaturesStore } from "../state/betaFeatures";
import { Orb } from "../components/Orb";
import RosterPicker from "./RosterPicker";
import { isPinnedNav, PINNED_NAV_ITEMS, sidebarGroups, type NavItem } from "./navItems";
import { isMacOrLinuxDesktop } from "../lib/platform";

const COLLAPSED_KEY = "ps5upload.desktop-sidebar.collapsed.v1";

/**
 * Labeled desktop navigation. v5.1 replaced this with an icon-only rail, which
 * made every non-primary screen require remembering an icon and then opening
 * More. The expanded sidebar is intentionally the default; collapse remains an
 * explicit, persisted choice for people who prefer more content width.
 */
export default function Sidebar() {
  const tr = useTr();
  const [collapsed, setCollapsed] = useState(
    () => safeGetItem(COLLAPSED_KEY) === "1",
  );
  // Brand eyebrow. Resolved once on mount; `getAppVersion` hits the engine
  // over HTTP in browser mode, so a failure has to degrade to an empty
  // string rather than throw. The span below renders unconditionally so it
  // keeps reserving its line — otherwise the title would shift vertically
  // when the version lands, and shift again if the lookup failed.
  const [appVersion, setAppVersion] = useState("");
  useEffect(() => {
    let cancelled = false;
    getAppVersion()
      .then((v) => {
        if (!cancelled) setAppVersion(v);
      })
      .catch(() => {
        if (!cancelled) setAppVersion("");
      });
    return () => {
      cancelled = true;
    };
  }, []);
  const errorCount = useLogsStore(
    (s) => s.entries.filter((e) => e.level === "error").length,
  );
  const updateAvailable = useUpdateStore((s) => s.phase.kind === "available");
  // Subscribed, not read once: flipping the switch in Settings must add or
  // remove the row without a reload.
  const betaEnabled = useBetaFeaturesStore((s) => s.enabled);
  // Every screen, in its sections, minus what the user hid — see `sidebarGroups`.
  const hidden = useNavSidebarStore((s) => s.hidden);
  // A pinned screen hidden by an older version stays shown, so it is not counted.
  const hiddenCount = hidden.filter((to) => !isPinnedNav(to)).length;
  const closedSections = useNavSidebarStore((s) => s.closedSections);
  const toggleHidden = useNavSidebarStore((s) => s.toggleHidden);
  const toggleSection = useNavSidebarStore((s) => s.toggleSection);
  const groups = useMemo(
    () => sidebarGroups(hidden, betaEnabled, !isTauriEnv(), isMacOrLinuxDesktop()),
    [hidden, betaEnabled],
  );

  const toggleCollapsed = () => {
    setCollapsed((current) => {
      const next = !current;
      safeSetItem(COLLAPSED_KEY, next ? "1" : "0");
      return next;
    });
  };

  const renderItem = (item: NavItem, hideable: boolean) => {
    const Icon = item.icon;
    const label = tr(item.key, undefined, item.fallback);
    const showErrors = item.to === "/logs" && errorCount > 0;
    const showUpdate = item.to === "/settings" && updateAvailable;
    return (
      <li key={item.to} className="group relative">
        <NavLink
          to={item.to}
          title={collapsed ? label : undefined}
          aria-label={collapsed ? label : undefined}
          // Pills: the active one is the white pill with a coral icon
          // (`.nav-pill` in index.css keys off aria-current).
          className={[
            "nav-pill relative flex min-h-11 items-center rounded-full text-[0.875rem] transition-[background-color,color,box-shadow]",
            "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-[var(--color-accent)]",
            collapsed ? "justify-center px-2" : "gap-3 px-4",
          ].join(" ")}
        >
          <Icon size={19} strokeWidth={1.7} className="nav-pill-icon shrink-0" />
          {!collapsed && <span className="min-w-0 flex-1 truncate">{label}</span>}
          {showErrors && (
            <span
              className={
                collapsed
                  ? "absolute right-1 top-1 h-2 w-2 rounded-full bg-[var(--color-bad)]"
                  : "rounded-full bg-[var(--color-bad)] px-1.5 py-0.5 text-[10px] font-semibold tabular-nums text-[var(--color-accent-contrast)]"
              }
              aria-label={`${errorCount} logged errors`}
            >
              {!collapsed && (errorCount > 99 ? "99+" : errorCount)}
            </span>
          )}
          {showUpdate && (
            <span
              className="h-2 w-2 shrink-0 rounded-full bg-[var(--color-accent)]"
              aria-label={tr("update_available_short", undefined, "Update available")}
            />
          )}
        </NavLink>
        {/* A sibling of the link (a button inside <a> is invalid and would navigate), shown on
            hover or keyboard focus so the list stays quiet. */}
        {hideable && !collapsed && (
          <button
            type="button"
            onClick={() => toggleHidden(item.to)}
            aria-label={tr("nav_hide_item", { name: label }, "Hide {name} from the sidebar")}
            title={tr("nav_hide", undefined, "Hide from the sidebar")}
            className="absolute right-2 top-1/2 flex h-7 w-7 -translate-y-1/2 items-center justify-center rounded-full bg-[var(--color-float)] text-[var(--color-muted)] opacity-0 hover:text-[var(--color-text)] focus-visible:opacity-100 group-hover:opacity-100"
          >
            <EyeOff size={14} aria-hidden />
          </button>
        )}
      </li>
    );
  };

  return (
    <aside
      data-testid="desktop-sidebar"
      data-collapsed={collapsed ? "true" : "false"}
      // Sits inside the shell's glass canvas, so it has no fill of its own.
      className={`hidden min-h-0 shrink-0 flex-col transition-[width] duration-200 md:flex ${
        collapsed ? "w-[5rem]" : "w-[17rem]"
      }`}
    >
      <div
        className={`flex h-[5.5rem] shrink-0 items-center ${
          collapsed ? "justify-center px-2" : "gap-2 ps-6 pe-3"
        }`}
      >
        <NavLink
          to="/home"
          aria-label={tr("v5_tab_home", undefined, "Home")}
          className={`flex min-w-0 items-center gap-3 rounded-full ${
            collapsed ? "justify-center" : "flex-1"
          }`}
        >
          <Orb size={collapsed ? 34 : 40} />
          {!collapsed && (
            <span className="min-w-0">
              <span className="block truncate text-[1.25rem] leading-tight font-semibold tracking-[-0.02em]">
                PS5Upload
              </span>
              {/* Deliberately NOT `uppercase` like the nav section headings:
                  that would render "v5.4.19" as "V5.4.19". Tabular figures
                  keep the digits from shifting when the version changes. */}
              <span className="block truncate text-[0.6875rem] font-medium tabular-nums tracking-[0.01em] text-[var(--color-muted)]">
                {appVersion ? `v${appVersion}` : "\u00a0"}
              </span>
            </span>
          )}
        </NavLink>
        {!collapsed && (
          <button
            type="button"
            onClick={toggleCollapsed}
            aria-label={tr(
              "sidebar_collapse",
              undefined,
              "Collapse navigation",
            )}
            title={tr("sidebar_collapse", undefined, "Collapse navigation")}
            className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full text-[var(--color-muted)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
          >
            <ChevronLeft size={18} aria-hidden />
          </button>
        )}
      </div>

      {!collapsed && <RosterPicker />}

      <nav
        aria-label={tr("v5_tab_primary_nav", undefined, "Primary")}
        className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden px-4 py-3 [overscroll-behavior:contain]"
      >
        <ul className="space-y-1">{PINNED_NAV_ITEMS.map((item) => renderItem(item, false))}</ul>
        {groups.map((group) => {
          const title = tr(group.section.key, undefined, group.section.fallback);
          // The icon rail has no header to reopen a folded section from, so it shows them all.
          const open = collapsed || !closedSections.includes(group.section.key);
          return (
            <section key={group.section.key} className="mt-5">
              {collapsed ? (
                <div aria-hidden className="mx-3 mb-3 border-t border-[var(--color-border)]" />
              ) : (
                // The small uppercase captions of the reference (GENERAL, OTHERS).
                <button
                  type="button"
                  onClick={() => toggleSection(group.section.key)}
                  aria-expanded={open}
                  className="flex w-full items-center gap-1 rounded-full px-2 pb-2 text-left text-[0.6875rem] font-medium uppercase tracking-[0.08em] text-[var(--color-muted)] hover:text-[var(--color-text)]"
                >
                  <span className="min-w-0 flex-1 truncate">{title}</span>
                  {open ? <ChevronDown size={12} aria-hidden /> : <ChevronRight size={12} aria-hidden />}
                </button>
              )}
              {open && <ul className="space-y-1">{group.items.map((item) => renderItem(item, true))}</ul>}
            </section>
          );
        })}

        {/* The way back for anything hidden: More lists every screen with its switch. */}
        {hiddenCount > 0 && !collapsed && (
          <NavLink
            to="/more"
            className="mt-4 block rounded-full px-4 py-2 text-[0.6875rem] text-[var(--color-muted)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
          >
            {tr("nav_hidden_count", { count: hiddenCount }, "{count} hidden — show them from More")}
          </NavLink>
        )}
      </nav>

      <div
        className={`flex shrink-0 items-center px-4 pb-4 pt-2 ${
          collapsed ? "flex-col gap-1" : "gap-1"
        }`}
      >
        <NavLink
          to="/more"
          title={tr("v5_tab_more_desc", undefined, "All screens")}
          aria-label={
            collapsed ? tr("v5_tab_more", undefined, "More") : undefined
          }
          className={`flex h-11 items-center rounded-full text-[0.875rem] text-[var(--color-text)] hover:bg-[var(--color-surface)] ${
            collapsed ? "w-11 justify-center" : "min-w-0 flex-1 gap-3 px-4"
          }`}
        >
          <LayoutGrid size={19} strokeWidth={1.7} className="text-[var(--color-muted)]" aria-hidden />
          {!collapsed && (
            <span className="truncate">
              {tr("v5_tab_more", undefined, "More")}
            </span>
          )}
        </NavLink>
        {collapsed && (
          <button
            type="button"
            onClick={toggleCollapsed}
            aria-label={tr("sidebar_expand", undefined, "Expand navigation")}
            title={tr("sidebar_expand", undefined, "Expand navigation")}
            className="flex h-10 w-10 items-center justify-center rounded-full text-[var(--color-muted)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
          >
            <ChevronRight size={18} aria-hidden />
          </button>
        )}
      </div>
    </aside>
  );
}
