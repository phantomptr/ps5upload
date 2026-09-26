import type { LucideIcon } from "lucide-react";

import { Spinner } from "./Spinner";

/**
 * Canonical page header used by every screen. Enforces a single
 * typographic + spacing rhythm so the sidebar-to-content transition
 * feels consistent no matter which tab the user is on.
 *
 * Layout: [icon] [title] [count/status]  ...  [right-side action?]
 *
 * - icon + title are always present; the accent 20px Lucide icon
 *   signals which screen we're on at a glance, especially for users
 *   with many tabs open.
 * - count is the lightweight "3 items" text that hangs off the title.
 *   It's optional — screens without a natural list count omit it.
 * - loading shows a small spinner next to the title, used while a
 *   background refresh is in flight but we already have stale data
 *   to render (so we don't want a full-page spinner).
 * - description is the one-sentence what-does-this-tab-do line that
 *   sits below the header bar. Kept as a prop rather than a separate
 *   component so screens can't forget to include it.
 * - right lets the screen drop its primary action (Refresh, etc) into
 *   the header without inventing a new layout each time.
 */
export function PageHeader({
  icon: Icon,
  title,
  count,
  loading,
  description,
  right,
}: {
  icon: LucideIcon;
  title: string;
  count?: number | string;
  loading?: boolean;
  description?: React.ReactNode;
  right?: React.ReactNode;
}) {
  return (
    <header className="mb-6">
      <div className="mb-1 flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div className="flex min-w-0 items-center gap-3">
          <span className="grid h-9 w-9 shrink-0 place-items-center rounded-[0.7rem] bg-[var(--color-accent-soft)] text-[var(--color-accent)]">
            <Icon size={18} />
          </span>
          <h1 className="truncate text-2xl font-bold tracking-[-0.025em]">
            {title}
          </h1>
          {count !== undefined && (
            <span className="shrink-0 rounded-full bg-[var(--color-surface-3)] px-2 py-0.5 text-xs tabular-nums text-[var(--color-muted)]">
              {count}
            </span>
          )}
          {loading && <Spinner size={14} tone="accent" />}
        </div>
        {right && <div className="shrink-0 sm:pt-0.5">{right}</div>}
      </div>
      {description && (
        <p className="max-w-3xl text-sm leading-relaxed text-[var(--color-muted)] sm:pl-12">
          {description}
        </p>
      )}
    </header>
  );
}
