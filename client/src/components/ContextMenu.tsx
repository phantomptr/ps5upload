import {
  type ReactNode,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";

import { haptic } from "../lib/haptics";

/**
 * v5 ContextMenu primitive (§22.16).
 *
 * Right-click (desktop) / long-press (mobile). WAI-ARIA Menu pattern.
 *
 * Reuses the same MenuItem shape as OverflowMenu/Menu for consistency.
 * The trigger is the wrapped element itself — ContextMenu attaches
 * onContextMenu (desktop) and a long-press timer (mobile/touch) to its
 * child wrapper.
 *
 * Accessibility (§20.4.2):
 *   - The menu has role="menu"; items have role="menuitem".
 *   - Arrow keys move between items; Home/End jump to first/last.
 *   - Type-ahead: pressing a character focuses the first item whose
 *     label starts with that character.
 *   - Escape closes; clicking outside closes.
 *   - The menu is portaled to document.body and positioned at the
 *     cursor / long-press coordinates, clamped to the viewport.
 *
 * Note: This component shares the same visual menu styling and item
 * shape as Menu/OverflowMenu. In v5 these all converge on the same
 * Menu primitive; ContextMenu is the right-click/long-press trigger
 * variant.
 */

export interface ContextMenuItem {
  label: string;
  icon?: ReactNode;
  onSelect: () => void;
  disabled?: boolean;
  destructive?: boolean;
  /** Optional separator before this item. */
  separator?: boolean;
}

interface Position {
  x: number;
  y: number;
}

const LONG_PRESS_MS = 500;
const TOUCH_MOVE_THRESHOLD = 10;

export function ContextMenu({
  items,
  children,
  disabled = false,
}: {
  items: ContextMenuItem[];
  /** The element the menu is attached to. Must be a single element child. */
  children: ReactNode;
  /** Disable the context menu trigger entirely. */
  disabled?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState<Position>({ x: 0, y: 0 });
  const [activeIdx, setActiveIdx] = useState(-1);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const itemRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const touchTimer = useRef<number | null>(null);
  const touchStart = useRef<Position>({ x: 0, y: 0 });

  const enabledItems = items.filter((i) => !i.disabled);

  const close = useCallback(() => {
    setOpen(false);
    setActiveIdx(-1);
  }, []);

  const openAt = useCallback(
    (x: number, y: number) => {
      if (disabled || items.length === 0) return;
      haptic("tap");
      // Clamp to viewport so the menu doesn't paint off-screen. The
      // actual menu width/height isn't known until render, so we use
      // a rough estimate (220x320) and the layout effect adjusts.
      const vw = window.innerWidth;
      const vh = window.innerHeight;
      const estW = 220;
      const estH = 320;
      const clampedX = Math.min(x, vw - estW - 8);
      const clampedY = Math.min(y, vh - estH - 8);
      setPosition({ x: Math.max(8, clampedX), y: Math.max(8, clampedY) });
      setOpen(true);
      setActiveIdx(enabledItems.length > 0 ? items.indexOf(enabledItems[0]) : -1);
    },
    [disabled, items, enabledItems],
  );

  // ---- Desktop: right-click (contextmenu event) ----
  const handleContextMenu = useCallback(
    (e: React.MouseEvent) => {
      if (disabled) return;
      e.preventDefault();
      openAt(e.clientX, e.clientY);
    },
    [disabled, openAt],
  );

  // ---- Mobile: long-press (touchstart → timer) ----
  const handleTouchStart = useCallback(
    (e: React.TouchEvent) => {
      if (disabled) return;
      const t = e.touches[0];
      touchStart.current = { x: t.clientX, y: t.clientY };
      touchTimer.current = window.setTimeout(() => {
        openAt(t.clientX, t.clientY);
      }, LONG_PRESS_MS);
    },
    [disabled, openAt],
  );

  const handleTouchMove = useCallback((e: React.TouchEvent) => {
    if (touchTimer.current === null) return;
    const t = e.touches[0];
    const dx = Math.abs(t.clientX - touchStart.current.x);
    const dy = Math.abs(t.clientY - touchStart.current.y);
    if (dx > TOUCH_MOVE_THRESHOLD || dy > TOUCH_MOVE_THRESHOLD) {
      window.clearTimeout(touchTimer.current);
      touchTimer.current = null;
    }
  }, []);

  const handleTouchEnd = useCallback(() => {
    if (touchTimer.current !== null) {
      window.clearTimeout(touchTimer.current);
      touchTimer.current = null;
    }
  }, []);

  // Clean up timer on unmount.
  useEffect(() => {
    return () => {
      if (touchTimer.current !== null) {
        window.clearTimeout(touchTimer.current);
      }
    };
  }, []);

  // ---- Reposition: after the menu mounts, clamp its actual rect ----
  useLayoutEffect(() => {
    if (!open || !menuRef.current) return;
    const rect = menuRef.current.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    let needsReposition = false;
    let x = position.x;
    let y = position.y;
    if (rect.right > vw - 8) {
      x = Math.max(8, vw - rect.width - 8);
      needsReposition = true;
    }
    if (rect.bottom > vh - 8) {
      y = Math.max(8, vh - rect.height - 8);
      needsReposition = true;
    }
    if (needsReposition) {
      setPosition({ x, y });
    }
    // Only run once per open to avoid loops.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // ---- Outside click + Escape ----
  useEffect(() => {
    if (!open) return;
    function handlePointer(e: PointerEvent) {
      if (!menuRef.current) return;
      if (!menuRef.current.contains(e.target as Node)) close();
    }
    function handleKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.preventDefault();
        close();
        return;
      }
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setActiveIdx((idx) => {
          for (let i = idx + 1; i < items.length; i++) {
            if (!items[i].disabled) return i;
          }
          return idx;
        });
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setActiveIdx((idx) => {
          for (let i = idx - 1; i >= 0; i--) {
            if (!items[i].disabled) return i;
          }
          return idx;
        });
        return;
      }
      if (e.key === "Home") {
        e.preventDefault();
        const first = items.findIndex((i) => !i.disabled);
        if (first >= 0) setActiveIdx(first);
        return;
      }
      if (e.key === "End") {
        e.preventDefault();
        for (let i = items.length - 1; i >= 0; i--) {
          if (!items[i].disabled) {
            setActiveIdx(i);
            break;
          }
        }
        return;
      }
      // Type-ahead: single printable char → first matching label.
      if (e.key.length === 1 && /[a-z0-9]/i.test(e.key)) {
        const lower = e.key.toLowerCase();
        for (let i = 0; i < items.length; i++) {
          if (!items[i].disabled && items[i].label.toLowerCase().startsWith(lower)) {
            setActiveIdx(i);
            break;
          }
        }
      }
    }
    // pointerdown for immediate touch response; mousedown for desktop.
    document.addEventListener("pointerdown", handlePointer);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("pointerdown", handlePointer);
      document.removeEventListener("keydown", handleKey);
    };
  }, [open, items, close]);

  // ---- Focus the active item ----
  useEffect(() => {
    if (open && activeIdx >= 0) {
      itemRefs.current[activeIdx]?.focus();
    }
  }, [open, activeIdx]);

  if (items.length === 0 && !disabled) return <>{children}</>;

  return (
    <>
      <div
        onContextMenu={handleContextMenu}
        onTouchStart={handleTouchStart}
        onTouchMove={handleTouchMove}
        onTouchEnd={handleTouchEnd}
        onTouchCancel={handleTouchEnd}
        className="contents"
      >
        {children}
      </div>
      {open &&
        typeof document !== "undefined" &&
        createPortal(
          <div
            ref={menuRef}
            role="menu"
            className="anim-rise elev-3 fixed z-[70] min-w-[200px] max-w-[calc(100vw-1rem)] overflow-y-auto rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] py-1"
            style={{ left: position.x, top: position.y }}
            onClick={(e) => e.stopPropagation()}
          >
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
                  disabled={item.disabled}
                  onClick={() => {
                    if (item.disabled) return;
                    close();
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
                </button>
              </div>
            ))}
          </div>,
          document.body,
        )}
    </>
  );
}
