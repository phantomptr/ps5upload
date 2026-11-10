import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";

/**
 * The heading of one part of a screen that joins several jobs (Shared Folders, Accounts):
 * an icon, a title, what the part is for, and its own actions on the right. The `id` goes on
 * the title, for the section's aria-labelledby.
 */
export function SectionHeading({
  id,
  icon: Icon,
  title,
  description,
  right,
}: {
  id: string;
  icon?: LucideIcon;
  title: ReactNode;
  description?: ReactNode;
  right?: ReactNode;
}) {
  return (
    <div className="flex flex-wrap items-start justify-between gap-3">
      {/* A 16rem basis: with a zero basis the actions never wrap and squeeze this to a word a line (#794). */}
      <div className="min-w-0 flex-1 basis-[16rem]">
        <h2 id={id} className="flex items-center gap-2 text-base font-semibold">
          {Icon && <Icon size={16} className="shrink-0 text-[var(--color-accent)]" aria-hidden />}
          {title}
        </h2>
        {description && <p className="mt-1 max-w-3xl text-sm text-[var(--color-muted)]">{description}</p>}
      </div>
      {right && <div className="flex flex-wrap gap-2">{right}</div>}
    </div>
  );
}
