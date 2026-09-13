import { useEffect, useMemo, useState } from "react";
import { NavLink } from "react-router";
import { ChevronLeft, ChevronRight, LayoutGrid, X } from "lucide-react";

import { getAppVersion } from "../lib/appVersion";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { useTr } from "../state/lang";
import { useLogsStore } from "../state/logs";
import { useUpdateStore } from "../state/update";
import { useNavFavoritesStore } from "../state/navFavorites";
import NotificationInbox from "./NotificationInbox";
import RosterPicker from "./RosterPicker";
import { groupNavItems, sidebarNavItems } from "./navItems";

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
  const favorites = useNavFavoritesStore((s) => s.favorites);
  const hintDismissed = useNavFavoritesStore((s) => s.hintDismissed);
  const dismissHint = useNavFavoritesStore((s) => s.dismissHint);
  // Home pinned at the top, About pinned at the bottom, the user's starred
  // screens in between — see `sidebarNavItems`.
  const groups = useMemo(
    () => groupNavItems(sidebarNavItems(favorites)),
    [favorites],
  );
  const showFavoritesHint = favorites.length === 0 && !hintDismissed;

  const toggleCollapsed = () => {
    setCollapsed((current) => {
      const next = !current;
      safeSetItem(COLLAPSED_KEY, next ? "1" : "0");
      return next;
    });
  };

  return (
    <aside
      data-testid="desktop-sidebar"
      data-collapsed={collapsed ? "true" : "false"}
      className={`hidden min-h-0 shrink-0 flex-col border-r border-[var(--color-border)] bg-[color-mix(in_oklab,var(--color-surface-2)_94%,var(--color-surface)_6%)] transition-[width] duration-200 md:flex ${
        collapsed ? "w-[4.25rem]" : "w-[15.5rem]"
      }`}
    >
      <div
        className={`flex h-[4.25rem] shrink-0 items-center ${
          collapsed ? "justify-center px-2" : "gap-2 px-3.5"
        }`}
      >
        <NavLink
          to="/home"
          aria-label={tr("v5_tab_home", undefined, "Home")}
          className={`flex min-w-0 items-center gap-2 rounded-md ${
            collapsed ? "justify-center" : "flex-1"
          }`}
        >
          <img
            src="/logo-square.png"
            alt=""
            className="h-8 w-8 shrink-0 rounded-[0.6rem] shadow-sm"
          />
          {!collapsed && (
            <span className="min-w-0">
              <span className="block truncate text-sm font-bold tracking-[-0.02em]">
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
            className="flex h-9 w-9 shrink-0 items-center justify-center rounded-[var(--radius-control)] text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
          >
            <ChevronLeft size={18} aria-hidden />
          </button>
        )}
      </div>

      {!collapsed && <RosterPicker />}

      <nav
        aria-label={tr("v5_tab_primary_nav", undefined, "Primary")}
        className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden px-2.5 py-3 [overscroll-behavior:contain]"
      >
        {groups.map((group, groupIndex) => (
          <section
            key={group.section.key}
            className={groupIndex === 0 ? "" : "mt-3"}
          >
            {collapsed ? (
              groupIndex > 0 && (
                <div
                  aria-hidden
                  className="mx-2 mb-2 border-t border-[var(--color-border)]"
                />
              )
            ) : (
              <h2 className="px-2.5 pb-1.5 text-[0.625rem] font-bold uppercase tracking-[0.12em] text-[var(--color-muted)]">
                {tr(group.section.key, undefined, group.section.fallback)}
              </h2>
            )}
            <ul className="space-y-1">
              {group.items.map((item) => {
                const Icon = item.icon;
                const label = tr(item.key, undefined, item.fallback);
                const showErrors = item.to === "/logs" && errorCount > 0;
                const showUpdate = item.to === "/settings" && updateAvailable;
                return (
                  <li key={item.to}>
                    <NavLink
                      to={item.to}
                      title={collapsed ? label : undefined}
                      aria-label={collapsed ? label : undefined}
                      className={({ isActive }) =>
                        [
                          "relative flex min-h-10 items-center rounded-[0.65rem] text-[0.8125rem] transition-[background-color,color,box-shadow]",
                          "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-[var(--color-accent)]",
                          collapsed ? "justify-center px-2" : "gap-2.5 px-2.5",
                          isActive
                            ? "bg-[var(--color-accent-soft)] font-semibold text-[var(--color-accent)] shadow-[inset_3px_0_0_var(--color-accent)]"
                            : "text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]",
                        ].join(" ")
                      }
                    >
                      <Icon size={18} strokeWidth={1.8} className="shrink-0" />
                      {!collapsed && (
                        <span className="min-w-0 flex-1 truncate">{label}</span>
                      )}
                      {showErrors && (
                        <span
                          className={
                            collapsed
                              ? "absolute right-1 top-1 h-2 w-2 rounded-full bg-[var(--color-bad)]"
                              : "rounded-full bg-[var(--color-bad)] px-1.5 py-0.5 text-[10px] font-semibold tabular-nums text-white"
                          }
                          aria-label={`${errorCount} logged errors`}
                        >
                          {!collapsed && (errorCount > 99 ? "99+" : errorCount)}
                        </span>
                      )}
                      {showUpdate && (
                        <span
                          className="h-2 w-2 shrink-0 rounded-full bg-[var(--color-accent)]"
                          aria-label={tr(
                            "update_available_short",
                            undefined,
                            "Update available",
                          )}
                        />
                      )}
                    </NavLink>
                  </li>
                );
              })}
            </ul>
          </section>
        ))}

        {/* Shown only until the user stars something (or dismisses it).
            Without it a fresh sidebar is a single Home row with no clue
            that the rest of the app is one click away in More. Hidden
            while collapsed — there is no room for prose in a 4.25rem rail. */}
        {showFavoritesHint && !collapsed && (
          <div className="mt-3 flex items-start gap-1.5 rounded-[0.65rem] border border-dashed border-[var(--color-border)] px-2.5 py-2 text-[0.6875rem] leading-snug text-[var(--color-muted)]">
            <span className="min-w-0 flex-1">
              {tr(
                "nav_favorites_hint",
                undefined,
                "Star screens in More to pin them here.",
              )}
            </span>
            <button
              type="button"
              onClick={dismissHint}
              aria-label={tr(
                "nav_favorites_hint_dismiss",
                undefined,
                "Dismiss",
              )}
              title={tr("nav_favorites_hint_dismiss", undefined, "Dismiss")}
              className="shrink-0 rounded p-0.5 hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
            >
              <X size={12} aria-hidden />
            </button>
          </div>
        )}
      </nav>

      <div
        className={`flex shrink-0 items-center border-t border-[var(--color-border)] p-2.5 ${
          collapsed ? "flex-col gap-1" : "gap-1"
        }`}
      >
        <NavLink
          to="/more"
          title={tr("v5_tab_more_desc", undefined, "All screens")}
          aria-label={
            collapsed ? tr("v5_tab_more", undefined, "More") : undefined
          }
          className={`flex h-10 items-center rounded-[0.65rem] text-[0.8125rem] text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)] ${
            collapsed ? "w-10 justify-center" : "min-w-0 flex-1 gap-2 px-2.5"
          }`}
        >
          <LayoutGrid size={18} aria-hidden />
          {!collapsed && (
            <span className="truncate">
              {tr("v5_tab_more", undefined, "More")}
            </span>
          )}
        </NavLink>
        <NotificationInbox />
        {collapsed && (
          <button
            type="button"
            onClick={toggleCollapsed}
            aria-label={tr("sidebar_expand", undefined, "Expand navigation")}
            title={tr("sidebar_expand", undefined, "Expand navigation")}
            className="flex h-10 w-10 items-center justify-center rounded-md text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
          >
            <ChevronRight size={18} aria-hidden />
          </button>
        )}
      </div>
    </aside>
  );
}
