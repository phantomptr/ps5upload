import type { LucideIcon } from "lucide-react";

/**
 * Standard content card. Replaces the ad-hoc `rounded-lg border ...`
 * snippet that every screen had been writing inline.
 *
 * Default styling:
 *   rounded-lg, border-[var(--color-border)], bg-[var(--color-surface-2)],
 *   p-4.
 *
 * When `title` is provided, renders a section header above the
 * children. `icon` is optional; if present it appears left of the
 * title in the accent color so it reads as a mini-section.
 *
 * v5 additions:
 *   - `actions` renders a footer row with a top border (for action buttons).
 *   - `interactive` + `onClick` renders the card as a `<button>` with
 *     `.hover-lift` for clickable cards (game tiles, task rows).
 *   - `interactive` + `href` renders the card as an `<a>`.
 */
export function Card({
  title,
  icon: Icon,
  right,
  actions,
  padded = true,
  accent = false,
  interactive = false,
  onClick,
  href,
  children,
  className = "",
}: {
  title?: string;
  icon?: LucideIcon;
  right?: React.ReactNode;
  /** Footer actions row (top border). */
  actions?: React.ReactNode;
  /** Set false to drop the inner padding (useful when the child renders
   *  a full-width list that has its own internal padding). */
  padded?: boolean;
  /** Accent border — highlights a card the user should notice first. */
  accent?: boolean;
  /** Adds hover-lift and renders as a button (onClick) or link (href). */
  interactive?: boolean;
  onClick?: () => void;
  href?: string;
  children: React.ReactNode;
  className?: string;
}) {
  const border = accent
    ? "border-[var(--color-accent)]"
    : "border-[var(--color-border)]";
  const pad = padded ? "p-4" : "";
  const liftCls = interactive
    ? "transition-[transform,box-shadow,border-color] hover:-translate-y-px hover:border-[var(--color-border-strong)] hover:elev-2 cursor-pointer text-left"
    : "";

  const header = title && (
    <header className="mb-3 flex items-center justify-between gap-2">
      <div className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
        {Icon && <Icon size={14} />}
        <span>{title}</span>
      </div>
      {right && <div className="shrink-0">{right}</div>}
    </header>
  );

  const footer = actions && (
    <footer className="mt-3 flex items-center justify-end gap-2 border-t border-[var(--color-border)] pt-3">
      {actions}
    </footer>
  );

  const inner = (
    <>
      {header}
      {children}
      {footer}
    </>
  );

  const cls = `elev-1 rounded-[var(--radius-panel)] border ${border} bg-[var(--color-surface-raised)] ${pad} ${liftCls} ${className}`;

  if (interactive && onClick) {
    return (
      <button type="button" onClick={onClick} className={cls}>
        {inner}
      </button>
    );
  }
  if (interactive && href) {
    return (
      <a href={href} className={cls}>
        {inner}
      </a>
    );
  }
  return <section className={cls}>{inner}</section>;
}
