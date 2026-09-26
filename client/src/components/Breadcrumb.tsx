import { useTr } from "../state/lang";

export interface Crumb {
  label: string;
  onClick?: () => void;
}

export interface BreadcrumbProps {
  items: Crumb[];
  className?: string;
}

/**
 * Path breadcrumb for File Browser / SMB navigation.
 *
 *   <nav aria-label="Breadcrumb">
 *     <ol>
 *       <li><button>root</button> <span aria-hidden>/</span></li>
 *       ...
 *       <li aria-current="page">current</li>
 *
 * The last item (no onClick) is the current page — rendered as plain
 * text with `aria-current="page"`. Separators are `/` and hidden from
 * SR (decorative).
 */
export function Breadcrumb({ items, className = "" }: BreadcrumbProps) {
  const tr = useTr();
  return (
    <nav
      aria-label={tr("breadcrumb", "Breadcrumb")}
      className={["min-w-0", className].join(" ")}
    >
      <ol className="flex flex-wrap items-center gap-1 text-xs">
        {items.map((crumb, i) => {
          const isLast = i === items.length - 1;
          return (
            <li key={`${crumb.label}-${i}`} className="flex items-center gap-1">
              {crumb.onClick && !isLast ? (
                <button
                  type="button"
                  onClick={crumb.onClick}
                  className="text-[var(--color-muted)] transition-colors hover:text-[var(--color-text)]"
                >
                  {crumb.label}
                </button>
              ) : (
                <span
                  aria-current={isLast ? "page" : undefined}
                  className={isLast ? "text-[var(--color-text)]" : "text-[var(--color-muted)]"}
                >
                  {crumb.label}
                </span>
              )}
              {!isLast && (
                <span aria-hidden="true" className="text-[var(--color-muted)]">
                  /
                </span>
              )}
            </li>
          );
        })}
      </ol>
    </nav>
  );
}
