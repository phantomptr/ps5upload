import {
  type ReactNode,
  type CSSProperties,
  useEffect,
  useRef,
  useState,
} from "react";

/**
 * v5 Menu primitive (§22.27).
 *
 * Evolves OverflowMenu by adding WAI-ARIA Menu keyboard navigation:
 * arrow keys, type-ahead, Home/End. The existing OverflowMenu component
 * becomes a thin wrapper that renders a Menu via its trigger button.
 *
 * This is the shared dropdown/popover menu used by:
 *   - OverflowMenu (the "more actions" trigger)
 *   - ContextMenu (right-click / long-press)
 *   - Any future trigger that needs a WAI-ARIA Menu pattern
 *
 * Accessibility (§20.4.2):
 *   - role="menu" on the dropdown, role="menuitem" on each item.
 *   - ArrowDown/ArrowUp move between enabled items.
 *   - Home/End jump to first/last enabled item.
 *   - Type-ahead: single printable char focuses the first enabled
 *     item whose label starts with that character.
 *   - Escape closes and returns focus to the trigger.
 *   - Selecting an item closes the menu and calls onSelect.
 *
 * The menu is positioned relative to `anchorRef.current`. The parent
 * is responsible for providing a stable anchor element.
 */

export interface MenuItem {
  label: string;
  icon?: ReactNode;
  onSelect: () => void;
  disabled?: boolean;
  loading?: boolean;
  destructive?: boolean;
  title?: string;
  /** Render a separator line before this item. */
  separator?: boolean;
}

export function MenuList({
  items,
  onClose,
  autoFocus = true,
}: {
  items: MenuItem[];
  /** Called when the menu should close (item selected, Escape pressed). */
  onClose: () => void;
  /** Focus the first enabled item on mount. Default true. */
  autoFocus?: boolean;
}) {
  const [activeIdx, setActiveIdx] = useState(-1);
  const itemRefs = useRef<Array<HTMLButtonElement | null>>([]);

  const enabledIndexes = items
    .map((item, i) => ({ item, i }))
    .filter(({ item }) => !item.disabled && !item.loading);

  // Auto-focus first enabled item.
  useEffect(() => {
    if (autoFocus && enabledIndexes.length > 0) {
      setActiveIdx(enabledIndexes[0].i);
    }
    // Only on mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Focus the active item when it changes.
  useEffect(() => {
    if (activeIdx >= 0) {
      itemRefs.current[activeIdx]?.focus();
    }
  }, [activeIdx]);

  if (items.length === 0) return null;

  const nextEnabled = (from: number, dir: 1 | -1): number => {
    for (let i = from + dir; i >= 0 && i < items.length; i += dir) {
      if (!items[i].disabled && !items[i].loading) return i;
    }
    return from;
  };

  const firstEnabled = (): number => {
    for (let i = 0; i < items.length; i++) {
      if (!items[i].disabled && !items[i].loading) return i;
    }
    return -1;
  };

  const lastEnabled = (): number => {
    for (let i = items.length - 1; i >= 0; i--) {
      if (!items[i].disabled && !items[i].loading) return i;
    }
    return -1;
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        setActiveIdx((idx) => nextEnabled(idx, 1));
        break;
      case "ArrowUp":
        e.preventDefault();
        setActiveIdx((idx) => nextEnabled(idx, -1));
        break;
      case "Home":
        e.preventDefault();
        setActiveIdx(firstEnabled());
        break;
      case "End":
        e.preventDefault();
        setActiveIdx(lastEnabled());
        break;
      case "Escape":
        e.preventDefault();
        onClose();
        break;
      case "Tab":
        // Tab closes the menu (prevents focus from escaping into the page).
        e.preventDefault();
        onClose();
        break;
      default:
        // Type-ahead: single printable char → first matching label.
        if (e.key.length === 1 && /[a-z0-9]/i.test(e.key)) {
          e.preventDefault();
          const lower = e.key.toLowerCase();
          for (let i = 0; i < items.length; i++) {
            if (
              !items[i].disabled &&
              !items[i].loading &&
              items[i].label.toLowerCase().startsWith(lower)
            ) {
              setActiveIdx(i);
              break;
            }
          }
        }
        break;
    }
  };

  return (
    <div role="menu" onKeyDown={handleKeyDown}>
      {items.map((item, i) => (
        <div key={`${item.label}-${i}`}>
          {item.separator && i > 0 && (
            <div className="my-1 border-t border-[var(--color-border)]" />
          )}
          <button
            ref={(el) => {
              itemRefs.current[i] = el;
            }}
            type="button"
            role="menuitem"
            disabled={item.disabled || item.loading}
            title={item.title}
            onMouseEnter={() => {
              if (!item.disabled && !item.loading) setActiveIdx(i);
            }}
            onClick={() => {
              if (item.disabled || item.loading) return;
              onClose();
              item.onSelect();
            }}
            className={[
              "flex w-full items-center gap-2 whitespace-nowrap px-3 py-2 text-left text-xs transition-colors",
              "disabled:cursor-not-allowed disabled:opacity-50",
              activeIdx === i
                ? "bg-[var(--color-surface-3)]"
                : "hover:bg-[var(--color-surface-3)]",
              item.destructive
                ? "text-[var(--color-bad)]"
                : "text-[var(--color-text)]",
            ].join(" ")}
          >
            {item.icon && (
              <span className="flex h-4 w-4 shrink-0 items-center justify-center">
                {item.icon}
              </span>
            )}
            <span className="flex-1">{item.label}</span>
            {item.loading && (
              <span className="ml-2 text-xs text-[var(--color-muted)]">…</span>
            )}
          </button>
        </div>
      ))}
    </div>
  );
}

/**
 * Position-aware dropdown menu shell. Wraps MenuList with positioning,
 * outside-click, and the visual container (border, elevation, animation).
 * Used by OverflowMenu and can be used directly.
 */
export function MenuDropdown({
  items,
  onClose,
  align = "right",
  placement,
  style,
}: {
  items: MenuItem[];
  onClose: () => void;
  align?: "left" | "right";
  /** Placement overrides from the parent (e.g. dropUp). */
  placement?: { dropUp: boolean; maxHeight: number };
  style?: CSSProperties;
}) {
  const menuPositionStyle: CSSProperties =
    align === "right" ? { right: 0 } : { left: 0 };

  return (
    <div
      className="anim-rise elev-2 absolute z-30 my-1 min-w-[200px] max-w-[calc(100vw-1rem)] overflow-y-auto overflow-x-hidden rounded-md border border-[var(--color-border)] bg-[var(--color-surface)]"
      style={{
        ...menuPositionStyle,
        ...style,
        ...(placement?.dropUp ? { bottom: "100%" } : { top: "100%" }),
        maxHeight: placement?.maxHeight || undefined,
      }}
    >
      <MenuList items={items} onClose={onClose} />
    </div>
  );
}
