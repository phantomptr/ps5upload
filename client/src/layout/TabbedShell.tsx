import type { ReactNode } from "react";
import { useSearchParams } from "react-router";
import type { LucideIcon } from "lucide-react";

import { PageHeader } from "../components";
import { useTr } from "../state/lang";
import { makeTabKeyHandler } from "../lib/tabKeyboardNav";

/**
 * Generic URL-routed tab shell used by /logs and /payloads (and any
 * future merged-screen pair). Owns:
 *
 *   - the `?tab=<id>` URL contract (single source of truth so deep
 *     links + page refresh preserve selection)
 *   - WAI-ARIA tablist semantics (role=tablist + role=tab + role=
 *     tabpanel + aria-selected + aria-controls/aria-labelledby)
 *   - keyboard navigation (Left/Right cyclic + Home/End) via
 *     `lib/tabKeyboardNav`
 *   - the page header (one row of icon + title + optional desc)
 *
 * Caller supplies the tab metadata (id, label, icon, optional
 * per-tab description) and a render function that returns the
 * panel for the active tab. The render-function approach keeps the
 * panels as plain children of the screen file (not lazy boundaries
 * the shell has to know about), and lets the caller hold its own
 * state if needed.
 *
 * Pre-2.12.0 this was duplicated between Logs/index.tsx and
 * Payloads/index.tsx — same ~60 lines of URL+ARIA+keyboard wiring
 * in both places. Architecture-critic audit flagged it; this
 * extraction was the prescribed fix.
 */

export interface TabbedShellTab<Id extends string> {
  id: Id;
  icon: LucideIcon;
  /** i18n key for the tab label. */
  key: string;
  /** English fallback when the key is missing from the active locale. */
  fallback: string;
  /** Per-tab description rendered into the PageHeader. The shell
   *  picks the active tab's description. Optional — if any tab
   *  omits it, the description row collapses for that tab. */
  description?: string;
}

export interface TabbedShellProps<Id extends string> {
  /** ID prefix for DOM ids (`<idPrefix>-tab-<id>`, `<idPrefix>-panel-<id>`).
   *  Must be unique per shell instance to avoid id collisions when two
   *  shells mount under the same route tree. */
  idPrefix: string;
  /** Title icon shown in the page header. Most screens want a fixed
   *  icon regardless of tab — pass null if you'd rather use the active
   *  tab's icon. */
  titleIcon?: LucideIcon | null;
  /** i18n key for the page title (e.g. "logs", "payloads"). */
  titleKey: string;
  /** English fallback for the page title. */
  titleFallback: string;
  /** Ordered list of tabs. The first one is the default when no
   *  `?tab=...` is present in the URL. */
  tabs: ReadonlyArray<TabbedShellTab<Id>>;
  /** Render the active tab's content. Called once per render with the
   *  active id; caller switches on id and returns the panel JSX. */
  renderPanel: (activeId: Id) => ReactNode;
}

/**
 * Coerce a search-param value to a valid tab id, falling back to the
 * first tab if the URL says something unknown. Strict allowlist —
 * unknown values silently land on default rather than triggering an
 * error or letting React drop the panel.
 */
function coerceTabId<Id extends string>(
  raw: string | null,
  tabs: ReadonlyArray<TabbedShellTab<Id>>,
): Id {
  if (raw && tabs.some((t) => t.id === raw)) {
    return raw as Id;
  }
  return tabs[0].id;
}

export default function TabbedShell<Id extends string>({
  idPrefix,
  titleIcon,
  titleKey,
  titleFallback,
  tabs,
  renderPanel,
}: TabbedShellProps<Id>) {
  const tr = useTr();
  const [searchParams, setSearchParams] = useSearchParams();

  const activeTab = coerceTabId<Id>(searchParams.get("tab"), tabs);
  const setActiveTab = (next: Id) => {
    if (next === activeTab) return;
    // A tab here is a full user-facing view, not a transient filter. Push it
    // into history so the global Back/Forward controls restore the previous
    // view. Preserve unrelated query state owned by the screen.
    const nextParams = new URLSearchParams(searchParams);
    nextParams.set("tab", next);
    setSearchParams(nextParams);
  };
  const onTabKey = makeTabKeyHandler<Id>(
    tabs.map((t) => t.id),
    setActiveTab,
    (id) => `${idPrefix}-tab-${id}`,
  );

  const activeTabMeta = tabs.find((t) => t.id === activeTab) ?? tabs[0];
  // Title icon priority: explicit titleIcon > active tab's icon.
  // When titleIcon=null is passed (caller opts into per-tab icon),
  // fall through to the active tab's icon.
  const IconForTitle = titleIcon ?? activeTabMeta.icon;
  const description = activeTabMeta.description;

  return (
    <div className="flex h-full flex-col p-6">
      <PageHeader
        icon={IconForTitle}
        title={tr(titleKey, undefined, titleFallback)}
        description={description}
      />

      {/* Tab strip — underline style for low visual weight. The
          page header already announces the screen. A11y: tablist +
          tab + tabpanel per WAI-ARIA; aria-pressed is for toggle
          buttons (different semantics). aria-controls/labelledby
          link tab → its panel id for screen readers. tabIndex
          rotates focus so Tab from the previous element lands on
          the active tab, then arrow keys move within. */}
      <div
        role="tablist"
        aria-label={tr(titleKey, undefined, titleFallback)}
        className="mb-4 flex items-center gap-1 border-b border-[var(--color-border)]"
      >
        {tabs.map(({ id, icon: Icon, key, fallback }) => {
          const isActive = id === activeTab;
          return (
            <button
              key={id}
              type="button"
              role="tab"
              id={`${idPrefix}-tab-${id}`}
              aria-controls={`${idPrefix}-panel-${id}`}
              aria-selected={isActive}
              tabIndex={isActive ? 0 : -1}
              onClick={() => setActiveTab(id)}
              onKeyDown={(e) => onTabKey(e, id)}
              className={
                "flex items-center gap-1.5 border-b-2 px-3 py-2 text-sm transition-colors " +
                (isActive
                  ? "border-[var(--color-accent)] font-semibold text-[var(--color-accent)]"
                  : "border-transparent text-[var(--color-muted)] hover:text-[var(--color-text)]")
              }
            >
              <Icon size={14} strokeWidth={1.75} />
              {tr(key, undefined, fallback)}
            </button>
          );
        })}
      </div>

      <div
        role="tabpanel"
        id={`${idPrefix}-panel-${activeTab}`}
        aria-labelledby={`${idPrefix}-tab-${activeTab}`}
        className="flex min-h-0 flex-1 flex-col"
      >
        {renderPanel(activeTab)}
      </div>
    </div>
  );
}
