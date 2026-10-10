import type { ReactNode } from "react";

import { Spinner } from "../../components";

/**
 * The line under the Captures tabs: what this tab lists, how many, and its actions.
 *
 * The screen's own header already names it, and the tab names the list, so a tab
 * repeating a full page header said the same thing three times.
 */
export function CaptureToolbar({
  count,
  loading,
  description,
  right,
}: {
  count?: number;
  loading?: boolean;
  description: ReactNode;
  right?: ReactNode;
}) {
  return (
    <div className="mb-4 flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
      <p className="flex min-w-0 max-w-3xl items-start gap-2.5 text-sm leading-relaxed text-[var(--color-muted)]">
        {count !== undefined && (
          <span className="mt-0.5 shrink-0 rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-2.5 py-0.5 text-xs font-semibold tabular-nums text-[var(--color-text)] shadow-[var(--edge-highlight)]">
            {count}
          </span>
        )}
        {loading && <Spinner size={14} tone="accent" className="mt-1 shrink-0" />}
        <span className="min-w-0 flex-1">{description}</span>
      </p>
      {right && <div className="flex shrink-0 flex-wrap items-center gap-2">{right}</div>}
    </div>
  );
}
