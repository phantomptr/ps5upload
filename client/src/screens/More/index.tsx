/**
 * More (v5, mobile).
 *
 * The mobile "everything else" hub. Replaces the old bottom sheet that
 * rendered the desktop <Sidebar> verbatim — measured at 448x997 that was
 * a 270px-wide, 1858px-tall column stranded in a height-capped sheet,
 * with 36px rows (every one under the 44px floor) and two nested scroll
 * containers.
 *
 * Three zones: console switcher + search (sticky), the screen list
 * (grouped, or flat while searching), and a utility footer.
 *
 * Every row can be pinned (phones too: Pinned shows here, under Bug report,
 * as it does in the desktop sidebar). Its Edit button shows Move up/down on
 * the pinned rows — the touch- and keyboard-friendly way to reorder them —
 * and otherwise they look like every other row.
 *
 * SCROLLING: this screen deliberately has NO scroll container of its own.
 * <main> in AppShell is already `overflow-y-auto` with the bottom-nav
 * padding applied; adding another would recreate the exact nested-scroll
 * bug this screen exists to fix. The sticky header works inside main's
 * scroll context.
 */
import { useEffect, useMemo, useState } from "react";
import { NavLink } from "react-router";
import {
  ArrowDown,
  ArrowUp,
  ChevronRight,
  Eye,
  EyeOff,
  LayoutGrid,
  Moon,
  Pin,
  PinOff,
  RotateCw,
  Search,
  Sun,
  X,
} from "lucide-react";

import { PageHeader, Input, EmptyState } from "../../components";
import { useTr } from "../../state/lang";
import { useNavSidebarStore } from "../../state/navSidebar";
import { useLogsStore } from "../../state/logs";
import { useUpdateStore } from "../../state/update";
import { isDarkTheme, useThemeStore } from "../../state/theme";
import { isTauriEnv } from "../../lib/tauriEnv";
import { getAppVersion } from "../../lib/appVersion";
import { isMacOrLinuxDesktop } from "../../lib/platform";
import RosterPicker from "../../layout/RosterPicker";
import { useBetaFeaturesStore } from "../../state/betaFeatures";
import NotificationInbox from "../../layout/NotificationInbox";
import { UpdateCheckButton } from "../../layout/UpdateCheckButton";
import {
  BUG_REPORT_NAV_ITEM,
  isFixedNav,
  MAX_PINNED,
  NAV_ITEMS,
  pinnedNavItems,
  groupNavItems,
  filterNavItems,
  navItemOffered,
  navItemVisible,
  type NavItem,
} from "../../layout/navItems";

export default function MoreScreen() {
  const tr = useTr();
  const [query, setQuery] = useState("");
  const [editingPins, setEditingPins] = useState(false);
  const { theme, toggleTheme } = useThemeStore();
  const [version, setVersion] = useState("");

  useEffect(() => {
    getAppVersion()
      .then(setVersion)
      .catch(() => setVersion(""));
  }, []);

  const errorCount = useLogsStore((s) => s.errorCount);
  const updateAvailable = useUpdateStore(
    // Not for a version the user skipped.
    (s) => s.phase.kind === "available" && s.phase.result.latest_version !== s.skippedVersion,
  );
  const betaEnabled = useBetaFeaturesStore((s) => s.enabled);

  const visible = useMemo(
    () =>
      NAV_ITEMS.filter(
        (i) =>
          navItemOffered(i, { inBrowser: !isTauriEnv(), macOrLinux: isMacOrLinuxDesktop() }) &&
          navItemVisible(i, betaEnabled),
      ),
    [betaEnabled],
  );
  const matches = useMemo(
    () => filterNavItems([BUG_REPORT_NAV_ITEM, ...visible], query, tr),
    [visible, query, tr],
  );
  const groups = useMemo(() => groupNavItems(matches.filter((i) => !isFixedNav(i.to))), [matches]);
  const pinned = useNavSidebarStore((s) => s.pinned);
  const pinnedItems = useMemo(
    () => pinnedNavItems(pinned, betaEnabled, !isTauriEnv(), isMacOrLinuxDesktop()),
    [pinned, betaEnabled],
  );
  // Unpinning the last one ends editing: the group (and its Done) is gone, and pinning again
  // later should show plain rows. Adjusted during render, React's pattern for derived state.
  if (editingPins && pinnedItems.length === 0) setEditingPins(false);
  const searching = query.trim().length > 0;
  return (
    <div className="app-page pb-6!">
      <PageHeader
        icon={LayoutGrid}
        title={tr("more_title", undefined, "More")}
        description={tr(
          "more_description",
          undefined,
          "Every screen, plus your consoles and app settings.",
        )}
      />

      {/* Sticky zone: console switcher + search. Sticks inside <main>'s
          scroll context — this screen adds no scroller of its own. */}
      <div className="glass-float sticky top-2 z-10 rounded-[var(--radius-panel)] px-1 py-3 [&>div:first-child]:mx-2">
        <RosterPicker />
        <div className="mt-3 px-2">
          <Input
            type="search"
            inputMode="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            aria-label={tr(
              "more_search_placeholder",
              undefined,
              "Search screens",
            )}
            placeholder={tr(
              "more_search_placeholder",
              undefined,
              "Search screens",
            )}
            leftIcon={<Search size={18} />}
            // Suppress the native search clear affordance: it renders as
            // a second, differently-styled X beside our own, it's well
            // under the 44px touch floor, and Android WebView doesn't
            // draw it consistently. Ours (rightSlot) is the real one.
            className="h-12 [&::-webkit-search-cancel-button]:appearance-none"
            rightSlot={
              query ? (
                <button
                  type="button"
                  onClick={() => setQuery("")}
                  aria-label={tr(
                    "more_search_clear",
                    undefined,
                    "Clear search",
                  )}
                  className="flex h-11 w-11 items-center justify-center rounded-full text-[var(--color-muted)] hover:text-[var(--color-text)]"
                >
                  <X size={18} />
                </button>
              ) : undefined
            }
          />
        </div>
      </div>

      {matches.length === 0 ? (
        <EmptyState
          icon={Search}
          title={tr("more_no_results", undefined, "No screens match")}
          message={tr(
            "more_no_results_desc",
            undefined,
            "Try a different word — screen names also match their English titles.",
          )}
        />
      ) : searching ? (
        /* Flat results — grouping is noise once the list is narrowed. */
        <ul className="surface-panel mt-4 overflow-hidden p-1.5">
          {matches.map((item) => (
            <MoreRow
              key={item.to}
              item={item}
              fixed={isFixedNav(item.to)}
              errorCount={errorCount}
              updateAvailable={updateAvailable}
            />
          ))}
        </ul>
      ) : (
        <>
          {/* Pinned, like the sidebar: the way to report a problem is always first. */}
          <ul className="surface-panel mt-4 overflow-hidden p-1.5">
            <MoreRow
              item={BUG_REPORT_NAV_ITEM}
              errorCount={errorCount}
              updateAvailable={updateAvailable}
              fixed
            />
          </ul>
          {/* The user's pins, in pin order, like the sidebar's Pinned group; absent until
              something is pinned. Each row also stays in its section below. */}
          {pinnedItems.length > 0 && (
            <section data-testid="more-pinned" className="mt-6">
              <div className="flex items-center justify-between px-4 pb-1">
                <h2 className="text-[0.6875rem] font-medium uppercase tracking-[0.08em] text-[var(--color-muted)]">
                  {tr("nav_section_pinned", undefined, "Pinned")}
                </h2>
                {/* Reordering is a mode: the arrows would crowd every row (names cut to
                    "Install Pa…" at phone width) for something done now and then. */}
                <button
                  type="button"
                  onClick={() => setEditingPins((v) => !v)}
                  aria-pressed={editingPins}
                  className="-me-3 min-h-11 rounded-full px-3 text-sm font-medium text-[var(--color-accent)] hover:bg-[var(--color-surface-3)]"
                >
                  {editingPins
                    ? tr("nav_pinned_done", undefined, "Done")
                    : tr("nav_pinned_edit", undefined, "Edit")}
                </button>
              </div>
              <ul className="surface-panel overflow-hidden p-1.5">
                {pinnedItems.map((item, i) => (
                  <MoreRow
                    key={item.to}
                    item={item}
                    errorCount={errorCount}
                    updateAvailable={updateAvailable}
                    order={
                      editingPins ? { first: i === 0, last: i === pinnedItems.length - 1 } : undefined
                    }
                  />
                ))}
              </ul>
            </section>
          )}
          {groups.map((group) => (
            <section key={group.section.key} className="mt-6 first:mt-1">
              <h2 className="px-4 pb-2 text-[0.6875rem] font-medium uppercase tracking-[0.08em] text-[var(--color-muted)]">
                {tr(group.section.key, undefined, group.section.fallback)}
              </h2>
              <ul className="surface-panel overflow-hidden p-1.5">
                {group.items.map((item) => (
                  <MoreRow
                    key={item.to}
                    item={item}
                    errorCount={errorCount}
                    updateAvailable={updateAvailable}
                  />
                ))}
              </ul>
            </section>
          ))}
        </>
      )}

      {/* Utility footer — theme, notifications, version. */}
      <div className="mt-6 flex items-center justify-between pt-1">
        <button
          type="button"
          onClick={toggleTheme}
          className="chip h-11 gap-2 px-4 text-sm font-medium"
          aria-label={tr(
            "switch_theme",
            { current: theme },
            `Switch theme (current: ${theme})`,
          )}
        >
          {!isDarkTheme(theme) ? <Moon size={16} aria-hidden /> : <Sun size={16} aria-hidden />}
          {tr("more_theme", undefined, "Theme")}
        </button>
        <div className="flex items-center gap-2">
          {/* Bottom-RIGHT of the screen here, unlike the desktop sidebar
              footer — so the panel must grow leftward or it lands off the
              edge of the phone. */}
          <NotificationInbox align="right" />
          {/* The app on a phone has no browser reload: this is it. */}
          <button
            type="button"
            onClick={() => window.location.reload()}
            aria-label={tr("topbar_reload_page", undefined, "Reload page")}
            title={tr("topbar_reload_page", undefined, "Reload page")}
            className="chip h-11 w-11 justify-center text-[var(--color-muted)]"
          >
            <RotateCw size={15} aria-hidden />
          </button>
          {isTauriEnv() ? (
            <UpdateCheckButton variant="chip" />
          ) : (
            <span className="text-xs tabular-nums text-[var(--color-muted)]">
              {version ? `v${version}` : "—"}
            </span>
          )}
        </div>
      </div>
    </div>
  );
}

/**
 * One 56px navigation row. Full-bleed tappable, chevron affordance, and
 * the same Logs error / Settings update badges the desktop sidebar shows
 * — losing them on mobile would hide the only signal that something
 * needs attention.
 */
export function MoreRow({
  item,
  errorCount,
  updateAvailable,
  fixed = false,
  order,
}: {
  item: NavItem;
  errorCount: number;
  updateAvailable: boolean;
  /** Home/Bug report, always in the sidebar: no hide or pin switch. */
  fixed?: boolean;
  /** A row of the Pinned group in edit mode: offers Move up/down (not past the ends) in
   *  place of the chevron. */
  order?: { first: boolean; last: boolean };
}) {
  const tr = useTr();
  const Icon = item.icon;
  // App log errors: Logs is a tab of Activity & logs (/tasks).
  const showErrors = item.to === "/tasks" && errorCount > 0;
  const showUpdate = item.to === "/settings" && updateAvailable;
  const hidden = useNavSidebarStore((s) => !fixed && s.hidden.includes(item.to));
  const toggleHidden = useNavSidebarStore((s) => s.toggleHidden);
  const isPinned = useNavSidebarStore((s) => !fixed && s.pinned.includes(item.to));
  const pinnedAll = useNavSidebarStore((s) => s.pinned);
  const betaEnabled = useBetaFeaturesStore((s) => s.enabled);
  // The pins the Pinned group shows: only these count toward the limit, and moves step over
  // the rest (a beta screen's pin while beta is off).
  const shownPins = useMemo(
    () =>
      pinnedNavItems(pinnedAll, betaEnabled, !isTauriEnv(), isMacOrLinuxDesktop()).map((i) => i.to),
    [pinnedAll, betaEnabled],
  );
  const pinFull = shownPins.length >= MAX_PINNED;
  const togglePinned = useNavSidebarStore((s) => s.togglePinned);
  const movePinned = useNavSidebarStore((s) => s.movePinned);
  /** Move, then keep keyboard focus on this row (the list re-orders under it): on the same
   *  arrow if it can still move that way, else the other arrow. */
  const move = (delta: -1 | 1) => {
    movePinned(item.to, delta, shownPins);
    window.setTimeout(() => {
      const row = document.querySelector(`[data-pin-row="${CSS.escape(item.to)}"]`);
      const same = row?.querySelector<HTMLButtonElement>(`[data-move="${delta}"]:not(:disabled)`);
      const other = row?.querySelector<HTMLButtonElement>(`[data-move="${-delta}"]:not(:disabled)`);
      (same ?? other ?? row?.querySelector<HTMLButtonElement>("[data-pin-toggle]"))?.focus();
    }, 0);
  };
  const label = tr(item.key, undefined, item.fallback);
  return (
    <li className="flex items-center" data-pin-row={order ? item.to : undefined}>
      <NavLink
        to={item.to}
        className={({ isActive }) =>
          [
            // 56px on touch (the §4.1 floor with room for the chevron);
            // tighter above md where the pointer is a mouse and the
            // extra height just costs rows-per-screen.
            "flex min-h-14 w-full items-center gap-3 rounded-full px-4 text-[15px] transition-colors md:min-h-11 md:text-sm",
            "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-accent)]",
            isActive
              ? "bg-[var(--color-pill)] font-semibold text-[var(--color-text)] shadow-[var(--edge-highlight),var(--shadow-1)] [&>svg:first-child]:text-[var(--color-accent-bright)]"
              : "text-[var(--color-text)] hover:bg-[var(--color-surface-3)] active:bg-[var(--color-surface-3)]",
          ].join(" ")
        }
      >
        <Icon size={22} strokeWidth={1.75} className="shrink-0 text-[var(--color-muted)]" />
        <span className="min-w-0 flex-1 truncate">
          {tr(item.key, undefined, item.fallback)}
        </span>
        {hidden && (
          <span className="hidden shrink-0 text-xs text-[var(--color-muted)] md:inline">
            {tr("nav_hidden_marker", undefined, "Hidden from the sidebar")}
          </span>
        )}
        {showErrors && (
          <span
            className="rounded-full bg-[var(--color-bad)] px-2 py-0.5 text-xs font-semibold tabular-nums text-[var(--color-accent-contrast)]"
            title={tr(
              errorCount === 1 ? "logged_error_one" : "logged_error_many",
              { count: errorCount },
              `${errorCount} logged error${errorCount === 1 ? "" : "s"}`,
            )}
          >
            {errorCount > 99 ? "99+" : errorCount}
          </span>
        )}
        {showUpdate && (
          <span
            className="h-2 w-2 rounded-full bg-[var(--color-accent)]"
            aria-label={tr(
              "update_available_short",
              undefined,
              "Update available",
            )}
          />
        )}
        {!order && (
          <ChevronRight
            size={18}
            aria-hidden
            className="shrink-0 text-[var(--color-muted)] rtl:rotate-180"
          />
        )}
      </NavLink>
      {/* Sibling of the link, not a child: a <button> inside an <a> is
          invalid markup and the click would navigate instead of toggling. */}
      {/* Pinned group only: reorder without dragging (touch, keyboard, screen readers). */}
      {order && (
        <>
          <MoreIconButton
            label={tr("nav_move_up_item", { name: label }, "Move {name} up")}
            disabled={order.first}
            data-move="-1"
            onClick={() => move(-1)}
          >
            <ArrowUp size={16} aria-hidden />
          </MoreIconButton>
          <MoreIconButton
            label={tr("nav_move_down_item", { name: label }, "Move {name} down")}
            disabled={order.last}
            data-move="1"
            onClick={() => move(1)}
          >
            <ArrowDown size={16} aria-hidden />
          </MoreIconButton>
        </>
      )}
      {/* Pin: phones and desktop alike — the Pinned group is on this screen too. */}
      {!fixed && (
        <MoreIconButton
          label={
            isPinned
              ? tr("nav_unpin_item", { name: label }, "Unpin {name}")
              : tr("nav_pin_item", { name: label }, "Pin {name}")
          }
          title={
            !isPinned && pinFull
              ? tr("nav_pin_full", { max: MAX_PINNED }, "You can pin up to {max} screens")
              : undefined
          }
          pressed={isPinned}
          disabled={!isPinned && pinFull}
          data-pin-toggle=""
          onClick={() => togglePinned(item.to, shownPins)}
          className={isPinned ? "text-[var(--color-accent)]" : "text-[var(--color-muted)]"}
        >
          {isPinned ? <PinOff size={16} aria-hidden /> : <Pin size={16} aria-hidden />}
        </MoreIconButton>
      )}
      {/* Desktop only: phones navigate by tabs and this list, and have no sidebar to change. A
          fixed screen has no switch: it is always there. */}
      {!fixed && !order && (
        <button
          type="button"
          onClick={() => toggleHidden(item.to)}
          aria-pressed={!hidden}
          aria-label={
            hidden
              ? tr("nav_show_item", { name: label }, "Show {name} in the sidebar")
              : tr("nav_hide_item", { name: label }, "Hide {name} from the sidebar")
          }
          title={
            hidden
              ? tr("nav_show_item", { name: label }, "Show {name} in the sidebar")
              : tr("nav_hide_item", { name: label }, "Hide {name} from the sidebar")
          }
          className={`mr-2 hidden h-11 w-11 shrink-0 items-center justify-center rounded-full transition-colors hover:bg-[var(--color-surface-3)] md:flex ${
            hidden ? "text-[var(--color-muted)] opacity-60 hover:opacity-100" : "text-[var(--color-accent)]"
          }`}
        >
          {hidden ? <EyeOff size={16} aria-hidden /> : <Eye size={16} aria-hidden />}
        </button>
      )}
    </li>
  );
}

/** A 44px round icon button at the end of a More row. */
function MoreIconButton({
  label,
  title,
  pressed,
  disabled = false,
  onClick,
  className = "text-[var(--color-muted)]",
  children,
  ...data
}: {
  label: string;
  title?: string;
  pressed?: boolean;
  disabled?: boolean;
  onClick: () => void;
  className?: string;
  children: React.ReactNode;
  /** `data-*` attributes, passed through (focus lookup after a move). */
  [data: `data-${string}`]: string | undefined;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      {...data}
      aria-pressed={pressed}
      aria-label={label}
      title={title ?? label}
      className={`flex h-11 w-11 shrink-0 items-center justify-center rounded-full transition-colors hover:bg-[var(--color-surface-3)] disabled:opacity-35 disabled:hover:bg-transparent ${className}`}
    >
      {children}
    </button>
  );
}
