import type { ReactNode } from "react";
import { X } from "lucide-react";

/** Class of each action button in the selection bar. On a phone the buttons
 *  keep their one-line label and size, and the strip scrolls sideways. */
export const SELECTION_ACTION =
  "flex items-center gap-1 rounded-full border px-3 py-1 shadow-[var(--edge-highlight),var(--shadow-1)] hover:bg-[var(--color-float)] bg-[var(--color-surface)] disabled:opacity-50 max-sm:shrink-0 max-sm:whitespace-nowrap max-sm:px-2.5";

/** The bar over the Files list while entries are selected: "N selected", the
 *  bulk actions, and a clear button at the end. One line at every width: the
 *  count never wraps, and on a phone the actions scroll sideways in their own
 *  strip instead of squeezing their labels to a letter per line. */
export function SelectionBar({
  countLabel,
  clearLabel,
  onClear,
  children,
}: {
  countLabel: string;
  clearLabel: string;
  onClear: () => void;
  /** The action buttons (use SELECTION_ACTION for their class). */
  children: ReactNode;
}) {
  return (
    <div
      data-testid="fs-selection-bar"
      className="mb-3 flex items-center gap-2 rounded-[var(--radius-card)] border border-[color-mix(in_srgb,var(--color-accent)_35%,transparent)] bg-[var(--color-accent-soft)] p-2 text-xs max-sm:gap-1.5 max-sm:p-1.5"
    >
      <span className="shrink-0 whitespace-nowrap font-medium">{countLabel}</span>
      <div
        data-testid="fs-selection-actions"
        className="flex min-w-0 flex-1 items-center gap-2 max-sm:gap-1 max-sm:overflow-x-auto"
      >
        {children}
      </div>
      <button
        type="button"
        onClick={onClear}
        aria-label={clearLabel}
        title={clearLabel}
        className="ms-auto shrink-0 rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] p-1 shadow-[var(--edge-highlight),var(--shadow-1)] hover:bg-[var(--color-float)]"
      >
        <X size={12} />
      </button>
    </div>
  );
}
