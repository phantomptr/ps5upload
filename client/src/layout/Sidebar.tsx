import { useEffect, useMemo, useRef, useState } from "react";
import { NavLink } from "react-router";
import { ChevronDown, ChevronLeft, ChevronRight, EyeOff, LayoutGrid, Pin, PinOff } from "lucide-react";

import { getAppVersion } from "../lib/appVersion";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { useTr } from "../state/lang";
import { useLogsStore } from "../state/logs";
import { useUpdateStore } from "../state/update";
import { useNavSidebarStore } from "../state/navSidebar";
import { isTauriEnv } from "../lib/tauriEnv";
import { useBetaFeaturesStore } from "../state/betaFeatures";
import { AppLogo } from "../components/AppLogo";
import RosterPicker from "./RosterPicker";
import {
  FIXED_NAV_ITEMS,
  isFixedNav,
  MAX_PINNED,
  pinnedNavItems,
  sidebarGroups,
  type NavItem,
} from "./navItems";
import { isMacOrLinuxDesktop, isWindows } from "../lib/platform";

const COLLAPSED_KEY = "ps5upload.desktop-sidebar.collapsed.v1";

/**
 * Labeled desktop navigation. v5.1 replaced this with an icon-only rail, which
 * made every non-primary screen require remembering an icon and then opening
 * More. The expanded sidebar is intentionally the default; collapse remains an
 * explicit, persisted choice for people who prefer more content width.
 */
export default function Sidebar() {
  const tr = useTr();
  // Until a choice is saved, a narrow window (a tablet held upright) starts with the icon rail,
  // which leaves the screen its width; the labeled sidebar fits from 1024 px.
  const [collapsed, setCollapsed] = useState(() => {
    const saved = safeGetItem(COLLAPSED_KEY);
    if (saved === "1" || saved === "0") return saved === "1";
    return typeof window !== "undefined" && window.innerWidth < 1024;
  });
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
  const errorCount = useLogsStore((s) => s.errorCount);
  const updateAvailable = useUpdateStore(
    // Not for a version the user skipped.
    (s) => s.phase.kind === "available" && s.phase.result.latest_version !== s.skippedVersion,
  );
  // Subscribed, not read once: flipping the switch in Settings must add or
  // remove the row without a reload.
  const betaEnabled = useBetaFeaturesStore((s) => s.enabled);
  // Every screen, in its sections, minus what the user hid — see `sidebarGroups`.
  const hidden = useNavSidebarStore((s) => s.hidden);
  const pinned = useNavSidebarStore((s) => s.pinned);
  // Home/Bug report hidden by an older version stay shown, and a pinned screen shows in
  // Pinned: neither is counted.
  const hiddenCount = hidden.filter((to) => !isFixedNav(to) && !pinned.includes(to)).length;
  const closedSections = useNavSidebarStore((s) => s.closedSections);
  const toggleHidden = useNavSidebarStore((s) => s.toggleHidden);
  const toggleSection = useNavSidebarStore((s) => s.toggleSection);
  const togglePinned = useNavSidebarStore((s) => s.togglePinned);
  const movePinnedTo = useNavSidebarStore((s) => s.movePinnedTo);
  const groups = useMemo(
    () => sidebarGroups(hidden, betaEnabled, !isTauriEnv(), isMacOrLinuxDesktop()),
    [hidden, betaEnabled],
  );
  // The user's Pinned group: copies of screens, in pin order; not rendered while empty.
  const pinnedItems = useMemo(
    () => pinnedNavItems(pinned, betaEnabled, !isTauriEnv(), isMacOrLinuxDesktop()),
    [pinned, betaEnabled],
  );
  const shownPins = useMemo(() => pinnedItems.map((i) => i.to), [pinnedItems]);
  const pinFull = shownPins.length >= MAX_PINNED;
  // The Windows app's window takes every drag for its own file drop (uploads), so no in-page
  // drag reaches a row there (as in Files): no drag there; More's Edit reorders everywhere.
  const dragReorderWorks = !(isTauriEnv() && isWindows());
  // The pinned screen being dragged to a new place in the Pinned group.
  const dragging = useRef<string | null>(null);

  const toggleCollapsed = () => {
    setCollapsed((current) => {
      const next = !current;
      safeSetItem(COLLAPSED_KEY, next ? "1" : "0");
      return next;
    });
  };

  /** The arrow key that points from a row's link to its buttons (pin, hide), which sit at
   *  the inline end of the row. */
  const endKey = () => (document.documentElement.dir === "rtl" ? "ArrowLeft" : "ArrowRight");
  const startKey = () => (document.documentElement.dir === "rtl" ? "ArrowRight" : "ArrowLeft");
  const rowButtons = (el: HTMLElement) =>
    Array.from(el.closest("li")?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? []);
  const towardButtons = (e: React.KeyboardEvent<HTMLAnchorElement>) => {
    if (e.key !== endKey()) return;
    const button = rowButtons(e.currentTarget)[0];
    if (!button) return;
    e.preventDefault();
    button.focus();
  };
  /** Between a row's buttons with the arrow keys; past the first (or Escape), back to the link. */
  const betweenButtons = (e: React.KeyboardEvent<HTMLButtonElement>) => {
    const buttons = rowButtons(e.currentTarget);
    const at = buttons.indexOf(e.currentTarget);
    if (e.key === endKey()) {
      e.preventDefault();
      buttons[at + 1]?.focus();
      return;
    }
    if (e.key !== startKey() && e.key !== "Escape") return;
    e.preventDefault();
    if (e.key === startKey() && at > 0) buttons[at - 1].focus();
    else e.currentTarget.closest("li")?.querySelector<HTMLElement>("a")?.focus();
  };
  /** Focus the link of the row after `el`'s (else the one before). */
  const focusNeighbour = (el: HTMLElement) => {
    const row = el.closest("li");
    const next = row?.nextElementSibling ?? row?.previousElementSibling;
    next?.querySelector<HTMLElement>("a")?.focus();
  };

  /**
   * One row. `fixed`: Home and Bug report, no buttons. `section`: a screen in its section, with
   * pin and hide buttons. `pinned`: its copy in the Pinned group, with unpin, draggable to
   * reorder (`index` is its place).
   */
  const renderItem = (item: NavItem, kind: "fixed" | "section" | "pinned", index = 0) => {
    const Icon = item.icon;
    const label = tr(item.key, undefined, item.fallback);
    // App log errors: Logs is a tab of Activity & logs (/tasks).
    const showErrors = item.to === "/tasks" && errorCount > 0;
    const showUpdate = item.to === "/settings" && updateAvailable;
    const hasButtons = kind !== "fixed" && !collapsed;
    const isPinned = pinned.includes(item.to);
    const draggable = kind === "pinned" && !collapsed && dragReorderWorks;
    const buttonClass =
      "flex h-7 w-7 items-center justify-center rounded-full bg-[var(--color-float)] text-[var(--color-muted)] opacity-0 hover:text-[var(--color-text)] focus-visible:opacity-100 group-hover:opacity-100 [@media(hover:none)]:group-has-[[aria-current=page]]:opacity-100 disabled:cursor-not-allowed disabled:hover:text-[var(--color-muted)]";
    const pinLabel = isPinned
      ? tr("nav_unpin_item", { name: label }, "Unpin {name}")
      : tr("nav_pin_item", { name: label }, "Pin {name}");
    const pinDisabled = !isPinned && pinFull;
    return (
      <li
        key={item.to}
        className="group relative"
        data-pinned-row={kind === "pinned" ? item.to : undefined}
        draggable={draggable || undefined}
        onDragStart={
          draggable
            ? (e) => {
                dragging.current = item.to;
                e.dataTransfer.effectAllowed = "move";
                e.dataTransfer.setData("text/plain", item.to);
              }
            : undefined
        }
        onDragOver={
          draggable
            ? (e) => {
                if (!dragging.current) return;
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
              }
            : undefined
        }
        onDrop={
          draggable
            ? (e) => {
                const from = dragging.current;
                dragging.current = null;
                if (!from) return;
                e.preventDefault();
                movePinnedTo(from, index);
              }
            : undefined
        }
        onDragEnd={draggable ? () => (dragging.current = null) : undefined}
      >
        <NavLink
          to={item.to}
          title={collapsed ? label : undefined}
          aria-label={collapsed ? label : undefined}
          onKeyDown={hasButtons ? towardButtons : undefined}
          // Pills: the active one is a tinted row with an accent icon
          // (`.nav-pill` in index.css keys off aria-current).
          className={[
            "nav-pill relative flex min-h-9 items-center rounded-[var(--radius-field)] text-[0.875rem] transition-[background-color,color,box-shadow]",
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
        {/* Siblings of the link (a button inside <a> is invalid and would navigate), shown on
            hover or keyboard focus so the list stays quiet.
            Not Tab stops: one after every link made the sidebar twice as long to tab
            through. From a focused link the arrow key toward the buttons reaches them (and the
            other arrow goes back); More has the same switches for every screen. */}
        {hasButtons && (
          <span className="absolute end-2 top-1/2 flex -translate-y-1/2 items-center gap-1">
            <button
              type="button"
              tabIndex={-1}
              disabled={pinDisabled}
              onKeyDown={betweenButtons}
              onClick={(e) => {
                // Unpinning from the Pinned group removes this row: keep focus in the list.
                if (kind === "pinned" && e.currentTarget === document.activeElement) {
                  focusNeighbour(e.currentTarget);
                }
                togglePinned(item.to, shownPins);
              }}
              aria-pressed={kind === "section" ? isPinned : undefined}
              aria-label={pinLabel}
              title={pinDisabled ? tr("nav_pin_full", { max: MAX_PINNED }, "You can pin up to {max} screens") : pinLabel}
              className={buttonClass}
            >
              {isPinned ? <PinOff size={14} aria-hidden /> : <Pin size={14} aria-hidden />}
            </button>
            {kind === "section" && (
              <button
                type="button"
                tabIndex={-1}
                onKeyDown={betweenButtons}
                onClick={(e) => {
                  // This row is about to go: keep keyboard focus in the list.
                  if (e.currentTarget === document.activeElement) focusNeighbour(e.currentTarget);
                  toggleHidden(item.to);
                }}
                aria-label={tr("nav_hide_item", { name: label }, "Hide {name} from the sidebar")}
                title={tr("nav_hide", undefined, "Hide from the sidebar")}
                className={buttonClass}
              >
                <EyeOff size={14} aria-hidden />
              </button>
            )}
          </span>
        )}
      </li>
    );
  };

  return (
    <aside
      data-testid="desktop-sidebar"
      data-collapsed={collapsed ? "true" : "false"}
      className={`shell-sidebar hidden min-h-0 shrink-0 flex-col transition-[width] duration-200 md:flex ${
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
          <AppLogo size={collapsed ? 34 : 38} />
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
            <ChevronLeft size={18} aria-hidden className="rtl:rotate-180" />
          </button>
        )}
      </div>

      {!collapsed && <RosterPicker />}

      <nav
        aria-label={tr("v5_tab_primary_nav", undefined, "Primary")}
        className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden px-4 py-3 [overscroll-behavior:contain]"
      >
        <ul className="space-y-1">{FIXED_NAV_ITEMS.map((item) => renderItem(item, "fixed"))}</ul>
        {/* The user's pins, right under Home/Bug report. Not there at all until something is
            pinned: someone who never pins sees no change. */}
        {pinnedItems.length > 0 && (
          <section data-testid="nav-pinned" className="mt-5">
            {collapsed ? (
              <div aria-hidden className="mx-3 mb-3 border-t border-[var(--color-border)]" />
            ) : (
              <h2 className="px-2 pb-2 text-[0.6875rem] font-medium uppercase tracking-[0.08em] text-[var(--color-muted)]">
                {tr("nav_section_pinned", undefined, "Pinned")}
              </h2>
            )}
            <ul className="space-y-1">{pinnedItems.map((item) => renderItem(item, "pinned", pinned.indexOf(item.to)))}</ul>
          </section>
        )}
        {/* A pinned screen is listed once, in Pinned: its section leaves it out (and a section
            whose screens are all pinned is not shown). More still lists every screen. */}
        {groups
          .map((group) => ({ ...group, items: group.items.filter((item) => !shownPins.includes(item.to)) }))
          .filter((group) => group.items.length > 0)
          .map((group) => {
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
                  {open ? <ChevronDown size={12} aria-hidden /> : <ChevronRight size={12} aria-hidden className="rtl:rotate-180" />}
                </button>
              )}
              {open && <ul className="space-y-1">{group.items.map((item) => renderItem(item, "section"))}</ul>}
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
          className={`flex h-9 items-center rounded-[var(--radius-field)] text-[0.875rem] text-[var(--color-text)] hover:bg-[var(--color-surface)] ${
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
            <ChevronRight size={18} aria-hidden className="rtl:rotate-180" />
          </button>
        )}
      </div>
    </aside>
  );
}
