import type { LucideIcon } from "lucide-react";

/**
 * v5 EmptyState primitive (§22.29).
 *
 * "Nothing here yet" / "waiting" / "error" card. Used when a screen has
 * loaded but has no data to show.
 *
 * v5 evolution (§22.29):
 *   - `min-height: 55vh` canonical (fixes the 72vh bug in v4 code).
 *   - `title` renders as `<h2>` (or `<h3>` when `headingTag="h3"`).
 *   - New `body` prop (ReactNode) replaces `message` (string). For
 *     backward compat, `message` is still accepted and renders as body.
 *   - New `hero` prop (ReactNode) replaces `icon` for richer empty art.
 *     For backward compat, `icon` is still accepted.
 *   - New `role` prop: "status" (default, polite) or "alert" (assertive,
 *     for error-driven empties).
 *   - Single action only — multi-action empty states use a Menu.
 *
 * Sizes:
 *   - compact (default) — small card with a single-line message.
 *   - hero — taller card with icon/title/message, fills the container.
 *
 * `fill` makes the card tall (min 55vh) and centres its content.
 */
export function EmptyState({
  icon: Icon,
  hero,
  title,
  message,
  body,
  size = "compact",
  fill = false,
  action,
  role = "status",
  headingTag = "h3",
}: {
  /** (Legacy) Lucide icon rendered above the title. Prefer `hero` for new code. */
  icon?: LucideIcon;
  /** (v5) Rich ReactNode rendered above the title (image, illustration, etc.). */
  hero?: React.ReactNode;
  title?: string;
  /** (Legacy) String body. Use `body` for ReactNode content. */
  message?: string;
  /** (v5) ReactNode body content. Takes precedence over `message`. */
  body?: React.ReactNode;
  size?: "compact" | "hero";
  /** Fill the container vertically (min-h-55vh). `hero` size implies fill. */
  fill?: boolean;
  action?: React.ReactNode;
  /** ARIA role: "status" (polite, default) or "alert" (assertive, for errors). */
  role?: "status" | "alert";
  /** Heading element. Defaults to h3; use h2 for screen-level empties. */
  headingTag?: "h2" | "h3";
}) {
  const wantFill = fill || size === "hero";
  // v5 canonical: 55vh (not the v4 72vh bug). When not filling, no min-height.
  const fillCls = wantFill
    ? "flex min-h-[55vh] flex-col items-center justify-center"
    : "";

  const content = body ?? message;
  const Heading = headingTag;
  // Pick the icon/hero node: hero wins, then icon.
  const heroNode = hero ?? (Icon ? (
    <Icon
      size={size === "hero" || fill ? 40 : 20}
      className={size === "hero" || fill
        ? "mx-auto mb-4 text-[var(--color-muted)] opacity-60"
        : "mx-auto mb-2 text-[var(--color-muted)] opacity-60"}
      aria-hidden
    />
  ) : null);

  if (size === "hero" || fill) {
    return (
      <div
        role={role}
        className={`rounded-[var(--radius-panel)] border border-dashed border-[var(--color-border)] bg-[var(--color-surface-raised)] p-12 text-center ${fillCls}`}
      >
        {heroNode}
        {title && <Heading className="mb-1.5 text-lg font-semibold">{title}</Heading>}
        {content && (
          <div className="mx-auto max-w-xl text-sm leading-relaxed text-[var(--color-muted)]">
            {content}
          </div>
        )}
        {action && <div className="mt-5">{action}</div>}
      </div>
    );
  }

  return (
    <div
      role={role}
      className="rounded-[var(--radius-panel)] border border-dashed border-[var(--color-border)] bg-[var(--color-surface-raised)] p-6 text-center text-sm text-[var(--color-muted)]"
    >
      {heroNode}
      {title && <Heading className="mb-1.5 text-base font-semibold">{title}</Heading>}
      {content}
      {action && (
        <div className="mt-3 flex justify-center">{action}</div>
      )}
    </div>
  );
}
