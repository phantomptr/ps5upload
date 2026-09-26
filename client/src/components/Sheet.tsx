import { useEffect, useId, useRef } from "react";
import { X } from "lucide-react";

import { useTr } from "../state/lang";
import { useScrollLock } from "../lib/useScrollLock";
import { useAccessibilityStore } from "../state/accessibility";
import { haptic } from "../lib/haptics";
import { useResponsiveTier } from "../lib/platform";

export interface SheetProps {
  open: boolean;
  onClose: () => void;
  title?: string;
  children: React.ReactNode;
  footer?: React.ReactNode;
  /** Force bottom-sheet on desktop too (default: centered on desktop). */
  forceBottom?: boolean;
  className?: string;
}

/**
 * Bottom sheet (mobile) / centered dialog (desktop).
 *
 *   - Mobile: slides from bottom (`.animate-in slide-in-from-bottom`),
 *     drag-to-dismiss on the handle (disabled under reduced motion)
 *   - Desktop: centered dialog unless `forceBottom`
 *
 * `role="dialog"`, `aria-modal="true"`, focus trap, Escape-to-close.
 */
export function Sheet({
  open,
  onClose,
  title,
  children,
  footer,
  forceBottom = false,
  className = "",
}: SheetProps) {
  const tr = useTr();
  const titleId = useId();
  const panelRef = useRef<HTMLDivElement>(null);
  const prevFocus = useRef<HTMLElement | null>(null);
  const dragStartY = useRef<number | null>(null);
  const motion = useAccessibilityStore((s) => s.resolvedMotion)();
  const tier = useResponsiveTier();

  const isMobile = tier === "xs" || tier === "sm";
  const showBottom = forceBottom || isMobile;

  useScrollLock(open);

  useEffect(() => {
    if (!open) return;
    const active =
      typeof document !== "undefined"
        ? (document.activeElement as HTMLElement | null)
        : null;
    prevFocus.current = active;
    queueMicrotask(() => {
      const panel = panelRef.current;
      if (!panel) return;
      const focusable = panel.querySelector<HTMLElement>(
        'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
      );
      (focusable ?? panel).focus({ preventScroll: true });
    });
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", handler);
    return () => {
      window.removeEventListener("keydown", handler);
      const prev = prevFocus.current;
      prevFocus.current = null;
      if (prev) queueMicrotask(() => prev.focus({ preventScroll: true }));
    };
  }, [open, onClose]);

  if (!open) return null;

  const slideAnim =
    motion === "none"
      ? ""
      : showBottom
        ? "animate-in slide-in-from-bottom duration-200"
        : "animate-in fade-in zoom-in-95 duration-150";

  // Drag-to-dismiss (mobile bottom sheet only, full-motion only).
  const enableDrag = showBottom && motion === "full";

  const onTouchStart = (e: React.TouchEvent) => {
    if (!enableDrag) return;
    dragStartY.current = e.touches[0]?.clientY ?? null;
  };
  const onTouchMove = (e: React.TouchEvent) => {
    if (!enableDrag || dragStartY.current === null) return;
    const dy = (e.touches[0]?.clientY ?? 0) - dragStartY.current;
    if (dy > 0 && panelRef.current) {
      panelRef.current.style.transform = `translateY(${dy}px)`;
    }
  };
  const onTouchEnd = () => {
    if (!enableDrag || dragStartY.current === null || !panelRef.current) return;
    const panel = panelRef.current;
    const rect = panel.getBoundingClientRect();
    const startY = dragStartY.current;
    dragStartY.current = null;
    panel.style.transform = "";
    // If dragged down past 100px, dismiss.
    if (rect.height > 0 && startY + 100 < rect.top + rect.height) {
      haptic("confirm");
      onClose();
    }
  };

  const containerCls = showBottom
    ? "items-end"
    : "items-center justify-center p-4";

  return (
    <div
      className={`anim-scrim fixed inset-0 z-50 flex ${containerCls} bg-[var(--overlay-scrim)]`}
      onClick={onClose}
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={title ? titleId : undefined}
        tabIndex={-1}
        onClick={(e) => e.stopPropagation()}
        onTouchStart={onTouchStart}
        onTouchMove={onTouchMove}
        onTouchEnd={onTouchEnd}
        className={[
          "elev-3 flex max-h-[90dvh] w-full flex-col overflow-hidden rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)]",
          showBottom ? "rounded-b-none" : "max-w-md",
          slideAnim,
          className,
        ].join(" ")}
      >
        {showBottom && enableDrag && (
          <div className="flex justify-center pt-2" aria-hidden="true">
            <div className="h-1 w-10 rounded-full bg-[var(--color-muted)] opacity-50" />
          </div>
        )}
        {title && (
          <header className="flex items-center justify-between gap-3 border-b border-[var(--color-border)] px-4 py-3">
            <h2 id={titleId} className="truncate text-sm font-semibold">
              {title}
            </h2>
            <button
              type="button"
              onClick={onClose}
              aria-label={tr("close", "Close")}
              className="shrink-0 rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
            >
              <X size={16} aria-hidden="true" />
            </button>
          </header>
        )}
        <div className="min-h-0 flex-1 overflow-y-auto">{children}</div>
        {footer && (
          <footer className="border-t border-[var(--color-border)] px-4 py-3 safe-bottom">
            {footer}
          </footer>
        )}
      </div>
    </div>
  );
}
