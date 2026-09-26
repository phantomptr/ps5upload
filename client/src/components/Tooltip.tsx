import { useId, useLayoutEffect, useRef, useState } from "react";

export interface TooltipProps {
  content: React.ReactNode;
  children: React.ReactElement;
  side?: "top" | "bottom" | "left" | "right";
  delay?: number;
}

/**
 * Hover/focus tooltip for supplementary information. Uses
 * `aria-describedby` (NOT `aria-label` — tooltip text is supplementary,
 * not the element's accessible name).
 *
 *   - Appears on hover AND focus-visible (keyboard users).
 *   - Esc dismisses.
 *   - 500ms show delay (default) to avoid flicker on rapid mouse moves.
 *   - Replaces all `title=""` attributes (which are ugly, un-themeable,
 *     and inaccessible).
 *
 * Auto-positioning: renders on the chosen side but flips if there isn't
 * room (checked in a layout effect after mount, before paint).
 *
 * The trigger is wrapped in a span that owns the aria-describedby +
 * event handlers. This avoids cloneElement (which the React Compiler's
 * ref-safety check flags) and keeps focus behavior native.
 */
export function Tooltip({
  content,
  children,
  side = "top",
  delay = 500,
}: TooltipProps) {
  const tipId = useId();
  const [visible, setVisible] = useState(false);
  const [effectiveSide, setEffectiveSide] = useState(side);
  const showTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const tipRef = useRef<HTMLSpanElement>(null);

  const show = () => {
    if (showTimer.current) clearTimeout(showTimer.current);
    showTimer.current = setTimeout(() => setVisible(true), delay);
  };

  const hide = () => {
    if (showTimer.current) {
      clearTimeout(showTimer.current);
      showTimer.current = null;
    }
    setVisible(false);
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") hide();
  };

  // Flip side if there isn't room — done in layout effect so the
  // adjustment happens before paint (no visible jump).
  useLayoutEffect(() => {
    if (!visible || !tipRef.current) {
      setEffectiveSide(side);
      return;
    }
    const r = tipRef.current.getBoundingClientRect();
    const margin = 8;
    let next = side;
    if (side === "top" && r.top < margin) next = "bottom";
    else if (side === "bottom" && r.bottom > window.innerHeight - margin)
      next = "top";
    else if (side === "left" && r.left < margin) next = "right";
    else if (side === "right" && r.right > window.innerWidth - margin)
      next = "left";
    setEffectiveSide(next);
  }, [visible, side]);

  const positionCls: Record<string, string> = {
    top: "bottom-full left-1/2 -translate-x-1/2 mb-1.5",
    bottom: "top-full left-1/2 -translate-x-1/2 mt-1.5",
    left: "right-full top-1/2 -translate-y-1/2 mr-1.5",
    right: "left-full top-1/2 -translate-y-1/2 ml-1.5",
  };

  return (
    <span
      className="relative inline-flex"
      aria-describedby={visible ? tipId : undefined}
      onMouseEnter={show}
      onMouseLeave={hide}
      onFocus={show}
      onBlur={hide}
      onKeyDown={handleKeyDown}
    >
      {children}
      {visible && (
        <span
          role="tooltip"
          id={tipId}
          ref={tipRef}
          className={[
            "pointer-events-none absolute z-50 max-w-xs rounded-md px-2 py-1",
            "text-xs text-[var(--color-accent-contrast)]",
            "bg-[var(--color-text)] shadow-lg",
            "animate-in fade-in zoom-in-95 duration-100",
            positionCls[effectiveSide],
          ].join(" ")}
        >
          {content}
        </span>
      )}
    </span>
  );
}
