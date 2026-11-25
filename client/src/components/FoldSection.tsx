import { useEffect, useRef, useState, type ReactNode } from "react";

import { OPEN_FOLD } from "../screens/Send/sendEvents";
import { ChevronRight } from "lucide-react";

/**
 * A section that folds to its heading line, for ways of doing something that most visits do
 * not need (Upload & Install's "From a link" and "Already on the PS5"). The heading line still says
 * what is inside (`summary`), and whether it is open is remembered per section in this browser.
 */
export function FoldSection({
  id,
  title,
  summary,
  defaultOpen = false,
  children,
}: {
  /** Unique per section: the remembered open state's key and the heading's id. */
  id: string;
  title: string;
  /** What is inside, shown beside the title when folded ("3 packages"). */
  summary?: ReactNode;
  defaultOpen?: boolean;
  children: ReactNode;
}) {
  const key = `ps5upload.fold.${id}`;
  const [open, setOpen] = useState<boolean>(() => {
    try {
      const v = window.localStorage.getItem(key);
      return v === null ? defaultOpen : v === "1";
    } catch {
      return defaultOpen;
    }
  });
  const toggle = () => {
    setOpen((o) => {
      try {
        window.localStorage.setItem(key, o ? "0" : "1");
      } catch {
        // Not remembered: the section still opens and closes.
      }
      return !o;
    });
  };
  // Opened from elsewhere on the page (Upload & Install's drop zone): open and come into view.
  const ref = useRef<HTMLElement>(null);
  useEffect(() => {
    const onOpen = (e: Event) => {
      if ((e as CustomEvent<string>).detail !== id) return;
      setOpen(true);
      ref.current?.scrollIntoView({ behavior: "smooth", block: "start" });
    };
    window.addEventListener(OPEN_FOLD, onOpen);
    return () => window.removeEventListener(OPEN_FOLD, onOpen);
  }, [id]);
  return (
    <section
      ref={ref}
      aria-labelledby={`${id}-heading`}
      className="mb-4"
      data-testid={`fold-${id}`}
    >
      <h2 id={`${id}-heading`} className="text-base font-semibold">
        <button
          type="button"
          aria-expanded={open}
          onClick={toggle}
          className="flex w-full items-center gap-2 rounded-md py-1 text-start focus-visible:outline focus-visible:outline-2 focus-visible:outline-[var(--color-accent)]"
        >
          <ChevronRight
            size={16}
            aria-hidden
            className={`shrink-0 text-[var(--color-muted)] transition-transform rtl:rotate-180 ${open ? "rotate-90 rtl:rotate-90" : ""}`}
          />
          {/* Side by side from `sm` up. On a phone the summary goes under the title: beside it,
              the two shared the row and a three-word title broke into three lines. */}
          <span className="min-w-0 flex-1 sm:flex sm:items-baseline sm:gap-2" data-testid="fold-heading-text">
            <span className="block sm:max-w-full sm:shrink-0">{title}</span>
            {!open && summary != null && (
              <span className="block min-w-0 truncate text-sm font-normal text-[var(--color-muted)]">
                <span className="max-sm:hidden">· </span>
                {summary}
              </span>
            )}
          </span>
        </button>
      </h2>
      {open && <div className="mt-3 grid gap-4">{children}</div>}
    </section>
  );
}
