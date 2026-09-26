/**
 * Keyboard-nav handler for WAI-ARIA tab strips.
 *
 * Per the spec [https://www.w3.org/WAI/ARIA/apg/patterns/tabs/],
 * when a `role="tab"` element has focus:
 *
 *   - ArrowLeft  → activate previous tab (cyclic; wraps to last)
 *   - ArrowRight → activate next tab     (cyclic; wraps to first)
 *   - Home       → activate first tab
 *   - End        → activate last tab
 *
 * Pass the ordered list of tab ids and a setter; the returned
 * `onKeyDown` handler is bound to each tab button. Activating a tab
 * also focuses its DOM node (looked up by the `id={tabIdFor(id)}`
 * convention used by both Logs and Payloads shells), so the focus
 * follows selection — what keyboard users expect.
 */
export function makeTabKeyHandler<T extends string>(
  tabs: ReadonlyArray<T>,
  setActive: (next: T) => void,
  /** How to derive the DOM id of each tab button — same shape both
   *  shells use, e.g. `id => `logs-tab-${id}``. */
  domIdFor: (id: T) => string,
): (e: React.KeyboardEvent<HTMLElement>, currentTab: T) => void {
  return (e, current) => {
    const idx = tabs.indexOf(current);
    let nextIdx: number | null = null;
    if (e.key === "ArrowLeft") nextIdx = (idx - 1 + tabs.length) % tabs.length;
    else if (e.key === "ArrowRight") nextIdx = (idx + 1) % tabs.length;
    else if (e.key === "Home") nextIdx = 0;
    else if (e.key === "End") nextIdx = tabs.length - 1;
    if (nextIdx === null) return;
    e.preventDefault();
    const nextTab = tabs[nextIdx];
    setActive(nextTab);
    // Follow-focus pattern: move focus to the newly-active tab so
    // the next keypress operates on it (otherwise focus stays on
    // the old tab and arrow keys feel "stuck").
    const el = document.getElementById(domIdFor(nextTab));
    el?.focus();
  };
}
