import { useMemo, useState } from "react";
import { NavLink } from "react-router";
import { ChevronLeft, ChevronRight, LayoutGrid } from "lucide-react";

import { isTauriEnv } from "../lib/tauriEnv";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { useTr } from "../state/lang";
import { useLogsStore } from "../state/logs";
import { useUpdateStore } from "../state/update";
import NotificationInbox from "./NotificationInbox";
import RosterPicker from "./RosterPicker";
import { NAV_ITEMS, groupNavItems } from "./navItems";

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
  const errorCount = useLogsStore(
    (s) => s.entries.filter((e) => e.level === "error").length,
  );
  const updateAvailable = useUpdateStore((s) => s.phase.kind === "available");
  const groups = useMemo(
    () =>
      groupNavItems(
        NAV_ITEMS.filter((item) => !item.hideInBrowser || isTauriEnv()),
      ),
    [],
  );

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
      className={`hidden min-h-0 shrink-0 flex-col border-r border-[var(--color-border)] bg-[var(--color-surface-2)] transition-[width] duration-200 md:flex ${
        collapsed ? "w-16" : "w-64"
      }`}
    >
      <div
        className={`flex h-14 shrink-0 items-center border-b border-[var(--color-border)] ${
          collapsed ? "justify-center px-2" : "gap-2 px-3"
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
            className="h-8 w-8 shrink-0 rounded-md"
          />
          {!collapsed && (
            <span className="truncate text-sm font-bold tracking-tight">
              PS5Upload
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
            className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
          >
            <ChevronLeft size={18} aria-hidden />
          </button>
        )}
      </div>

      {!collapsed && <RosterPicker />}

      <nav
        aria-label={tr("v5_tab_primary_nav", undefined, "Primary")}
        className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden px-2 py-2 [overscroll-behavior:contain]"
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
              <h2 className="px-2 pb-1 text-[10px] font-semibold uppercase tracking-wider text-[var(--color-muted)]">
                {tr(group.section.key, undefined, group.section.fallback)}
              </h2>
            )}
            <ul className="space-y-0.5">
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
                          "relative flex min-h-10 items-center rounded-md text-sm transition-colors",
                          "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-[var(--color-accent)]",
                          collapsed ? "justify-center px-2" : "gap-2.5 px-2.5",
                          isActive
                            ? "bg-[var(--color-accent-soft)] font-medium text-[var(--color-accent)]"
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
      </nav>

      <div
        className={`flex shrink-0 items-center border-t border-[var(--color-border)] p-2 ${
          collapsed ? "flex-col gap-1" : "gap-1"
        }`}
      >
        <NavLink
          to="/more"
          title={tr("v5_tab_more_desc", undefined, "All screens")}
          aria-label={
            collapsed ? tr("v5_tab_more", undefined, "More") : undefined
          }
          className={`flex h-10 items-center rounded-md text-sm text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)] ${
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
