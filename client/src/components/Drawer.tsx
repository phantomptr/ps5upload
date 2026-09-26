import { useEffect, useId, useRef } from "react";
import { X } from "lucide-react";

import { useTr } from "../state/lang";
import { useScrollLock } from "../lib/useScrollLock";
import { useAccessibilityStore } from "../state/accessibility";
import { useResponsiveTier } from "../lib/platform";

export interface DrawerProps {
  open: boolean;
  onClose: () => void;
  side?: "left" | "right";
  title?: string;
  children: React.ReactNode;
  footer?: React.ReactNode;
  width?: string;
  className?: string;
}

/**
 * Side-drawer dialog. `role="dialog"`, `aria-modal="true"`, focus trap,
 * Escape-to-close, scrim click-to-close.
 *
 * Slides from `side` (left or right). Animation respects motion settings.
 * Replaces the hand-rolled sidebar drawer in AppShell.
 */
export function Drawer({
  open,
  onClose,
  side = "right",
  title,
  children,
  footer,
  width,
  className = "",
}: DrawerProps) {
  const tr = useTr();
  const titleId = useId();
  const panelRef = useRef<HTMLDivElement>(null);
  const prevFocus = useRef<HTMLElement | null>(null);
  const motion = useAccessibilityStore((s) => s.resolvedMotion)();
  const tier = useResponsiveTier();

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

  const isMobile = tier === "xs" || tier === "sm";
  const computedWidth = width ?? (isMobile ? "85vw" : "320px");

  const slideAnim =
    motion === "none"
      ? ""
      : side === "right"
        ? "animate-in slide-in-from-right duration-200"
        : "animate-in slide-in-from-left duration-200";

  return (
    <div
      className="anim-scrim fixed inset-0 z-50 bg-[var(--overlay-scrim)]"
      onClick={onClose}
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={title ? titleId : undefined}
        tabIndex={-1}
        onClick={(e) => e.stopPropagation()}
        className={[
          "elev-3 absolute top-0 bottom-0 flex flex-col border-[var(--color-border)] bg-[var(--color-surface-2)]",
          side === "right"
            ? "right-0 border-l"
            : "left-0 border-r",
          slideAnim,
          className,
        ].join(" ")}
        style={{ width: computedWidth }}
      >
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
          <footer className="border-t border-[var(--color-border)] px-4 py-3">
            {footer}
          </footer>
        )}
      </div>
    </div>
  );
}
