import type { LucideIcon } from "lucide-react";

import { Spinner } from "./Spinner";

/**
 * Canonical page header used by every screen. Enforces a single
 * typographic + spacing rhythm so the sidebar-to-content transition
 * feels consistent no matter which tab the user is on.
 *
 * Layout: [icon] [title] [count/status]  ...  [right-side action?]
 *
 * - title is always present, large and bold; the icon is optional and sits
 *   beside it in a small white circle.
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
  /** A small coral glyph beside the title. Optional: the big title alone
   *  says where you are. */
  icon?: LucideIcon;
  title: string;
  count?: number | string;
  loading?: boolean;
  description?: React.ReactNode;
  right?: React.ReactNode;
}) {
  return (
    <header className="mb-7">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex min-w-0 items-center gap-3">
          {Icon && (
            <span className="grid h-10 w-10 shrink-0 place-items-center rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] text-[var(--color-accent-bright)] shadow-[var(--edge-highlight),var(--shadow-1)]">
              <Icon size={18} />
            </span>
          )}
          {/* A phone has room for a two-line title, not for "Firmware Spoof Detecti…". */}
          <h1 className="min-w-0 text-[1.75rem] leading-[1.1] font-bold tracking-[-0.03em] [overflow-wrap:anywhere] sm:truncate sm:text-[2.25rem]">
            {title}
          </h1>
          {count !== undefined && (
            <span className="shrink-0 rounded-full border border-[var(--color-border-strong)] px-2.5 py-0.5 text-xs font-medium tabular-nums text-[var(--color-muted)]">
              {count}
            </span>
          )}
          {loading && <Spinner size={14} tone="accent" />}
        </div>
        {/* On a phone the actions sit under the title: let their row wrap, so a
            button keeps its label instead of shrinking to "Ref…". */}
        {right && (
          <div className="shrink-0 [&>div]:flex-wrap max-sm:[&>div]:justify-start">{right}</div>
        )}
      </div>
      {description && (
        <p className="mt-2 max-w-3xl text-sm leading-relaxed text-[var(--color-muted)]">
          {description}
        </p>
      )}
    </header>
  );
}
