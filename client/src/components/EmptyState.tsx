import type { LucideIcon } from "lucide-react";

/**
 * Dashed-border "nothing here yet" card. Used when a screen has
 * successfully loaded but has no data to show — different from the
 * error state, which means "loading failed."
 *
 * Two visual modes:
 *   - compact (default) — small card with a single-line message,
 *     appropriate when the screen has other content around it.
 *   - hero (size="hero") — taller card with icon + title + message,
 *     for screens whose ONLY state is "nothing yet" (empty Library
 *     after a first upload, pre-connection Hardware, etc).
 */
export function EmptyState({
  icon: Icon,
  title,
  message,
  size = "compact",
  action,
}: {
  icon?: LucideIcon;
  title?: string;
  message: string;
  size?: "compact" | "hero";
  action?: React.ReactNode;
}) {
  if (size === "hero") {
    return (
      <div className="rounded-lg border border-dashed border-[var(--color-border)] bg-[var(--color-surface-2)] p-10 text-center">
        {Icon && (
          <Icon
            size={32}
            className="mx-auto mb-3 text-[var(--color-muted)] opacity-60"
          />
        )}
        {title && (
          <h3 className="mb-1 text-sm font-semibold">{title}</h3>
        )}
        <p className="mx-auto max-w-lg text-xs leading-relaxed text-[var(--color-muted)]">
          {message}
        </p>
        {action && <div className="mt-4">{action}</div>}
      </div>
    );
  }
  return (
    <div className="rounded-md border border-dashed border-[var(--color-border)] bg-[var(--color-surface-2)] p-4 text-center text-xs text-[var(--color-muted)]">
      {message}
    </div>
  );
}
