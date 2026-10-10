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
      <p className="flex min-w-0 max-w-3xl flex-wrap items-center gap-2 text-sm leading-relaxed text-[var(--color-muted)]">
        {count !== undefined && (
          <span className="shrink-0 rounded-full bg-[var(--color-surface-3)] px-2 py-0.5 text-xs tabular-nums">
            {count}
          </span>
        )}
        {loading && <Spinner size={14} tone="accent" />}
        <span className="min-w-0">{description}</span>
      </p>
      {right && <div className="flex shrink-0 flex-wrap items-center gap-2">{right}</div>}
    </div>
  );
}
