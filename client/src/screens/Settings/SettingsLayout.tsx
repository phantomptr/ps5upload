/** Full-width band heading that visually groups the cards that follow it.
 *  Implemented as a `col-span-full` sibling in the same grid (not a wrapper)
 *  so it forces a new row and the cards under it flow as their own band. */
export function GroupHeading({ children }: { children: React.ReactNode }) {
  return (
    <h2 className="mt-4 border-b border-[var(--color-border)] pb-1.5 text-sm font-semibold text-[var(--color-text)] md:col-span-2 first:mt-0">
      {children}
    </h2>
  );
}

export function Section({
  title,
  children,
  /** When true, the section spans the full grid width instead of sharing a
   *  row. For settings whose body is tall enough (a warning card, a long
   *  description) that half-width would look cramped. */
  full = false,
  /** A tag after the title ("Experimental"). */
  badge,
  testId,
}: {
  title: string;
  children: React.ReactNode;
  full?: boolean;
  badge?: React.ReactNode;
  testId?: string;
}) {
  return (
    <section
      className={
        "surface-panel !rounded-[var(--radius-card)] flex h-full flex-col p-5 " +
        (full ? "md:col-span-2" : "")
      }
      data-testid={testId}
    >
      <h2 className="mb-3 flex flex-wrap items-center gap-2 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
        {title}
        {badge}
      </h2>
      {children}
    </section>
  );
}
