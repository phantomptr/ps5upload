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
 */
export function Card({
  title,
  icon: Icon,
  right,
  padded = true,
  accent = false,
  children,
  className = "",
}: {
  title?: string;
  icon?: LucideIcon;
  right?: React.ReactNode;
  /** Set false to drop the inner padding (useful when the child renders
   *  a full-width list that has its own internal padding). */
  padded?: boolean;
  /** Accent border — highlights a card the user should notice first. */
  accent?: boolean;
  children: React.ReactNode;
  className?: string;
}) {
  const border = accent
    ? "border-[var(--color-accent)]"
    : "border-[var(--color-border)]";
  const pad = padded ? "p-4" : "";
  return (
    <section
      className={`rounded-lg border ${border} bg-[var(--color-surface-2)] ${pad} ${className}`}
    >
      {title && (
        <header className="mb-3 flex items-center justify-between gap-2">
          <div className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
            {Icon && <Icon size={14} />}
            <span>{title}</span>
          </div>
          {right && <div className="shrink-0">{right}</div>}
        </header>
      )}
      {children}
    </section>
  );
}
