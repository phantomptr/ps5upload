import { useEffect, useId, useRef, type ReactNode } from "react";
import { X } from "lucide-react";

import { useTr } from "../state/lang";

/**
 * Shared modal chrome — scrim + centered panel, Escape-to-close,
 * click-scrim-to-close, focus restore, and aria wiring.
 *
 * Before this, every screen rolled its own `fixed inset-0 … bg-black/NN`
 * overlay; most handled none of Escape / focus management / aria, and the
 * scrim opacity jittered between screens. Routing them all through this one
 * component makes modals behave consistently and accessibly. The confirm/
 * alert/prompt hooks in ConfirmDialog keep their own (imperative) copy of the
 * same pattern — this is for the declarative, conditionally-rendered modals.
 *
 * Usage:
 *   {showThing && (
 *     <Modal open onClose={() => setShowThing(false)} title="Thing" size="lg">
 *       …body…
 *     </Modal>
 *   )}
 */
const SIZES = {
  sm: "max-w-sm",
  md: "max-w-md",
  lg: "max-w-xl",
  xl: "max-w-3xl",
} as const;

export function Modal({
  open,
  onClose,
  title,
  titleIcon,
  children,
  footer,
  size = "md",
  closeOnScrim = true,
  role = "dialog",
  ariaLabel,
  panelClassName = "",
  bodyClassName = "",
}: {
  open: boolean;
  onClose: () => void;
  /** Header title. When set, the header renders with a close button. */
  title?: ReactNode;
  titleIcon?: ReactNode;
  children: ReactNode;
  /** Optional footer row (buttons), rendered with a top border. */
  footer?: ReactNode;
  size?: keyof typeof SIZES;
  /** Whether clicking the backdrop closes the modal. Default true. */
  closeOnScrim?: boolean;
  role?: "dialog" | "alertdialog";
  /** aria-label for modals with no visible title. */
  ariaLabel?: string;
  panelClassName?: string;
  bodyClassName?: string;
}) {
  const tr = useTr();
  const titleId = useId();
  const panelRef = useRef<HTMLDivElement | null>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);

  // Escape-to-close + focus capture/restore. Both gated on `open` so a
  // closed modal adds no listeners and doesn't fight for focus.
  useEffect(() => {
    if (!open) return;
    if (typeof document !== "undefined") {
      const active = document.activeElement;
      previousFocusRef.current = active instanceof HTMLElement ? active : null;
    }
    // Move focus into the panel so keyboard/screen-reader users land inside
    // the modal rather than staying on the now-inert trigger behind it.
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
      const prev = previousFocusRef.current;
      previousFocusRef.current = null;
      if (prev && typeof document !== "undefined") {
        queueMicrotask(() => prev.focus({ preventScroll: true }));
      }
    };
  }, [open, onClose]);

  if (!open) return null;

  return (
    // z-50 is the modal layer. The confirm/alert/prompt dialogs (ConfirmDialog)
    // sit at z-[60] so a confirm opened from WITHIN a Modal (e.g. the Manage
    // PS5s delete) paints ON TOP of the panel instead of being hidden behind
    // it — previously both were z-50 and the wider panel occluded the confirm.
    <div
      className="anim-scrim fixed inset-0 z-50 flex items-center justify-center bg-[var(--overlay-scrim)] p-4"
      onClick={closeOnScrim ? onClose : undefined}
    >
      <div
        ref={panelRef}
        role={role}
        aria-modal="true"
        aria-labelledby={title ? titleId : undefined}
        aria-label={!title ? ariaLabel : undefined}
        tabIndex={-1}
        onClick={(e) => e.stopPropagation()}
        className={`anim-pop elev-3 flex max-h-[90vh] w-full ${SIZES[size]} flex-col overflow-hidden rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] ${panelClassName}`}
      >
        {title && (
          <header className="flex items-center justify-between gap-3 border-b border-[var(--color-border)] px-4 py-3">
            <h2
              id={titleId}
              className="flex min-w-0 items-center gap-2 truncate text-sm font-semibold"
            >
              {titleIcon}
              {title}
            </h2>
            <button
              type="button"
              onClick={onClose}
              aria-label={tr("close", undefined, "Close")}
              title={tr("close", undefined, "Close")}
              className="shrink-0 rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
            >
              <X size={16} />
            </button>
          </header>
        )}
        <div className={`min-h-0 flex-1 overflow-y-auto ${bodyClassName}`}>
          {children}
        </div>
        {footer && (
          <footer className="flex items-center justify-end gap-2 border-t border-[var(--color-border)] px-4 py-3">
            {footer}
          </footer>
        )}
      </div>
    </div>
  );
}

export default Modal;
