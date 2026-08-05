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
 * SCROLLING: this screen deliberately has NO scroll container of its own.
 * <main> in AppShell is already `overflow-y-auto` with the bottom-nav
 * padding applied; adding another would recreate the exact nested-scroll
 * bug this screen exists to fix. The sticky header works inside main's
 * scroll context.
 */
import { useEffect, useMemo, useState } from "react";
import { NavLink } from "react-router";
import { ChevronRight, LayoutGrid, Search, X } from "lucide-react";

import { PageHeader, Input, EmptyState } from "../../components";
import { useTr } from "../../state/lang";
import { useLogsStore } from "../../state/logs";
import { useUpdateStore } from "../../state/update";
import { useThemeStore } from "../../state/theme";
import { isTauriEnv } from "../../lib/tauriEnv";
import { getAppVersion } from "../../lib/appVersion";
import RosterPicker from "../../layout/RosterPicker";
import NotificationInbox from "../../layout/NotificationInbox";
import {
  NAV_ITEMS,
  groupNavItems,
  filterNavItems,
  type NavItem,
} from "../../layout/navItems";

export default function MoreScreen() {
  const tr = useTr();
  const [query, setQuery] = useState("");
  const { theme, toggleTheme } = useThemeStore();
  const [version, setVersion] = useState("");

  useEffect(() => {
    getAppVersion()
      .then(setVersion)
      .catch(() => setVersion(""));
  }, []);

  const errorCount = useLogsStore(
    (s) => s.entries.filter((e) => e.level === "error").length,
  );
  const updateAvailable = useUpdateStore((s) => s.phase.kind === "available");

  const visible = useMemo(
    () => NAV_ITEMS.filter((i) => !i.hideInBrowser || isTauriEnv()),
    [],
  );
  const matches = useMemo(
    () => filterNavItems(visible, query, tr),
    [visible, query, tr],
  );
  const groups = useMemo(() => groupNavItems(matches), [matches]);
  const searching = query.trim().length > 0;

  return (
    <div className="mx-auto w-full max-w-3xl px-4 pb-6">
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
      <div className="sticky top-0 z-10 -mx-4 bg-[var(--color-bg)] px-4 pb-0 pt-1 backdrop-blur-[3px]">
        <div className="bg-[var(--color-surface)]">
          <RosterPicker />
        </div>
        <div className="mt-3">
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
                  aria-label={tr("more_search_clear", undefined, "Clear search")}
                  className="flex h-11 w-11 items-center justify-center rounded-md text-[var(--color-muted)] hover:text-[var(--color-text)]"
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
        <ul className="mt-1">
          {matches.map((item) => (
            <MoreRow
              key={item.to}
              item={item}
              errorCount={errorCount}
              updateAvailable={updateAvailable}
            />
          ))}
        </ul>
      ) : (
        groups.map((group) => (
          <section key={group.section.key} className="mt-4 first:mt-1">
            <h2 className="px-1 pb-1 text-xs font-semibold uppercase tracking-wider text-[var(--color-muted)]">
              {tr(group.section.key, undefined, group.section.fallback)}
            </h2>
            <ul>
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
        ))
      )}

      {/* Utility footer — theme, notifications, version. */}
      <div className="mt-6 flex items-center justify-between border-t border-[var(--color-border)] pt-3">
        <button
          type="button"
          onClick={toggleTheme}
          className="flex h-11 items-center gap-2 rounded-md px-3 text-sm text-[var(--color-muted)] transition-colors hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
          aria-label={tr(
            "switch_theme",
            { current: theme },
            `Switch theme (current: ${theme})`,
          )}
        >
          {tr("more_theme", undefined, "Theme")}
        </button>
        <div className="flex items-center gap-2">
          <NotificationInbox />
          <span className="text-xs tabular-nums text-[var(--color-muted)]">
            {version ? `v${version}` : "—"}
          </span>
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
function MoreRow({
  item,
  errorCount,
  updateAvailable,
}: {
  item: NavItem;
  errorCount: number;
  updateAvailable: boolean;
}) {
  const tr = useTr();
  const Icon = item.icon;
  const showErrors = item.to === "/logs" && errorCount > 0;
  const showUpdate = item.to === "/settings" && updateAvailable;
  return (
    <li>
      <NavLink
        to={item.to}
        className={({ isActive }) =>
          [
            // 56px on touch (the §4.1 floor with room for the chevron);
            // tighter above md where the pointer is a mouse and the
            // extra height just costs rows-per-screen.
            "flex min-h-14 w-full items-center gap-3 rounded-lg px-3 text-[15px] transition-colors md:min-h-10 md:text-sm",
            "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-accent)]",
            isActive
              ? "bg-[var(--color-accent)] font-medium text-[var(--color-accent-contrast)]"
              : "text-[var(--color-text)] active:bg-[var(--color-surface-3)]",
          ].join(" ")
        }
      >
        <Icon size={22} strokeWidth={1.75} className="shrink-0" />
        <span className="min-w-0 flex-1 truncate">
          {tr(item.key, undefined, item.fallback)}
        </span>
        {showErrors && (
          <span
            className="rounded-full bg-[var(--color-bad)] px-2 py-0.5 text-xs font-semibold tabular-nums text-white"
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
        <ChevronRight
          size={18}
          aria-hidden
          className="shrink-0 text-[var(--color-muted)]"
        />
      </NavLink>
    </li>
  );
}
