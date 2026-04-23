import type { LucideIcon } from "lucide-react";
import { Loader2 } from "lucide-react";

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
    <header className="mb-5">
      <div className="mb-1 flex items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-3">
          <Icon size={20} className="shrink-0 text-[var(--color-accent)]" />
          <h1 className="truncate text-xl font-semibold tracking-tight">
            {title}
          </h1>
          {count !== undefined && (
            <span className="shrink-0 rounded-full bg-[var(--color-surface-3)] px-2 py-0.5 text-[11px] tabular-nums text-[var(--color-muted)]">
              {count}
            </span>
          )}
          {loading && (
            <Loader2
              size={14}
              className="animate-spin text-[var(--color-accent)]"
            />
          )}
        </div>
        {right && <div className="shrink-0">{right}</div>}
      </div>
      {description && (
        <p className="text-sm leading-relaxed text-[var(--color-muted)]">
          {description}
        </p>
      )}
    </header>
  );
}
