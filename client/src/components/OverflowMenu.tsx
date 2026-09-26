import {
  type ReactNode,
  useEffect,
  useRef,
  useState,
} from "react";
import { ChevronDown, MoreHorizontal } from "lucide-react";

import { Button, type ButtonVariant } from "./Button";
import { MenuDropdown } from "./Menu";

/**
 * One action inside an OverflowMenu.
 *
 * `loading` shows a spinner glyph and disables the row; the parent
 * decides which item is busy via the same boolean it already uses to
 * disable its primary buttons. `destructive` switches the label to
 * the warn color so deletes don't blend with neutral actions.
 *
 * `title` becomes the row's `title` attribute, providing the same
 * tooltip a Button would have surfaced. Keep it short — the menu
 * already includes the action label, so the title is for the why
 * (e.g. "Modifies the source file") not the what.
 */
export interface OverflowMenuItem {
  label: string;
  /** Optional left icon; when present, rendered at the same 12 px
   *  size the row Buttons use, so the menu lines up visually. */
  icon?: ReactNode;
  onSelect: () => void;
  disabled?: boolean;
  loading?: boolean;
  destructive?: boolean;
  title?: string;
}

/**
 * "More actions" trigger that opens a small popover menu of
 * secondary actions. Used in the Library row to keep the visible
 * action surface minimal — primary action stays as a Button, every
 * other action lives behind this menu so the row reads as one
 * verb at a glance instead of seven.
 *
 * v5: Now wraps the shared Menu primitive (§22.27) which adds
 * WAI-ARIA arrow-key navigation, type-ahead, and Home/End support.
 * OverflowMenu retains its trigger UI and positioning logic.
 *
 * Closes on:
 *   - selecting an item (after the onSelect runs)
 *   - clicking outside the menu
 *   - pressing Escape
 *   - the trigger losing focus to outside the popover
 *
 * Positioned relative to the trigger (`absolute right-0 top-full`),
 * which is reliable inside the surrounding scroll container without
 * needing a portal — Library rows live inside a scrollable section
 * so a portaled menu would detach from the row visually.
 */
export function OverflowMenu({
  items,
  ariaLabel = "More actions",
  buttonTitle = "More actions",
  align = "right",
  size = "sm",
  triggerLabel,
  triggerIcon,
  triggerVariant = "ghost",
}: {
  items: OverflowMenuItem[];
  ariaLabel?: string;
  buttonTitle?: string;
  align?: "left" | "right";
  size?: "sm" | "md";
  /** When set, the trigger renders as a labeled Button ("Browse ▾") with a
   *  chevron instead of the bare ⋮ icon — for cases where the menu IS the
   *  primary affordance (e.g. a unified file/folder picker) rather than a
   *  secondary "more actions" overflow. */
  triggerLabel?: string;
  triggerIcon?: ReactNode;
  triggerVariant?: ButtonVariant;
}) {
  const [open, setOpen] = useState(false);
  // Vertical flip + clamp: a menu anchored `top: 100%` can run off the bottom
  // of the viewport when its trigger sits low on screen (a Library row near the
  // bottom, a short mobile window). On open we measure the trigger and flip the
  // menu above it when there's more room there, and cap its height so a long
  // list scrolls internally instead of spilling past the edge.
  const [placement, setPlacement] = useState<{
    dropUp: boolean;
    maxHeight: number;
  }>({ dropUp: false, maxHeight: 0 });
  const wrapperRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open || !wrapperRef.current) return;
    const measure = () => {
      const el = wrapperRef.current;
      if (!el) return;
      const rect = el.getBoundingClientRect();
      const margin = 8;
      const vh = window.innerHeight;
      const below = vh - rect.bottom - margin;
      const above = rect.top - margin;
      // Flip up only when below is genuinely cramped and above has more room.
      const dropUp = below < 220 && above > below;
      setPlacement({
        dropUp,
        maxHeight: Math.max(120, Math.floor(dropUp ? above : below)),
      });
    };
    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, [open]);

  // Close on outside click + Escape. Both wired only when the menu
  // is open so we don't pay the listener cost for every closed menu
  // on every page (Library can render hundreds of rows).
  useEffect(() => {
    if (!open) return;
    function handlePointer(e: Event) {
      if (!wrapperRef.current) return;
      if (!wrapperRef.current.contains(e.target as Node)) setOpen(false);
    }
    function handleKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    // pointerdown (not mousedown) so touch taps outside dismiss the menu
    // crisply on Android/touch — mousedown is only synthesized after the
    // touch sequence, leaving the popover open a beat too long.
    document.addEventListener("pointerdown", handlePointer);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("pointerdown", handlePointer);
      document.removeEventListener("keydown", handleKey);
    };
  }, [open]);

  if (items.length === 0) return null;

  return (
    <div ref={wrapperRef} className="relative inline-block">
      {triggerLabel ? (
        <Button
          variant={triggerVariant}
          size={size}
          leftIcon={triggerIcon}
          rightIcon={<ChevronDown size={14} />}
          onClick={() => setOpen((v) => !v)}
          aria-haspopup="menu"
          aria-expanded={open}
          aria-label={ariaLabel}
          title={buttonTitle}
        >
          {triggerLabel}
        </Button>
      ) : (
        <Button
          variant={triggerVariant}
          size={size}
          leftIcon={<MoreHorizontal size={14} />}
          onClick={() => setOpen((v) => !v)}
          aria-haspopup="menu"
          aria-expanded={open}
          aria-label={ariaLabel}
          title={buttonTitle}
        />
      )}
      {open && (
        <MenuDropdown
          items={items}
          onClose={() => setOpen(false)}
          align={align}
          placement={placement}
        />
      )}
    </div>
  );
}
