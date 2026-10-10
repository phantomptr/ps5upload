import type { LucideIcon } from "lucide-react";

import { Orb } from "./Orb";

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
  // Pick the icon/hero node: hero wins, then icon. The icon sits on a small
  // orb, the app's soft focal point.
  const big = size === "hero" || fill;
  const heroNode = hero ?? (Icon ? (
    <span
      className={`relative mx-auto grid place-items-center ${big ? "mb-5 h-16 w-16" : "mb-3 h-11 w-11"}`}
    >
      <Orb size={big ? 64 : 44} className="[grid-area:1/1]" />
      <Icon
        size={big ? 26 : 18}
        className="relative [grid-area:1/1] text-white drop-shadow-[0_1px_2px_rgb(120_30_40/0.4)]"
        aria-hidden
      />
    </span>
  ) : null);

  if (size === "hero" || fill) {
    return (
      <div
        role={role}
        className={`rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] p-12 text-center shadow-[var(--edge-highlight),var(--shadow-1)] ${fillCls}`}
      >
        {heroNode}
        {title && <Heading className="mb-1.5 text-xl font-semibold tracking-[-0.01em]">{title}</Heading>}
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
      className="rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] p-6 text-center text-sm text-[var(--color-muted)] shadow-[var(--edge-highlight),var(--shadow-1)]"
    >
      {heroNode}
      {title && <Heading className="mb-1.5 text-base font-semibold text-[var(--color-text)]">{title}</Heading>}
      {content}
      {action && (
        <div className="mt-3 flex justify-center">{action}</div>
      )}
    </div>
  );
}
