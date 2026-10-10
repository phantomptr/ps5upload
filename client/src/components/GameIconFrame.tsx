import type { ReactNode } from "react";

/**
 * A game cover in the "Recommended" frame of the Sense reference: a thick
 * frame with a large radius and a soft shadow around the art. The frame is
 * the pill colour, so it is solid white by day and a glass rim at night.
 *
 * `children` is the art (an <img>, GameIcon, CollectionCover…); `overlay` sits
 * on top of it, clipped to the inner radius, for corner badges. The art is
 * square; the caption, when wanted, goes below with CoverCaption.
 */
export function CoverFrame({
  children,
  overlay,
  className = "",
  interactive = false,
}: {
  children: ReactNode;
  overlay?: ReactNode;
  className?: string;
  /** Lifts on hover (the frame is, or sits in, something clickable). */
  interactive?: boolean;
}) {
  return (
    <div
      className={`rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-pill)] p-1.5 shadow-[var(--edge-highlight),var(--shadow-1)] sm:p-2 ${
        interactive
          ? "transition-[transform,box-shadow] duration-200 ease-out group-hover:-translate-y-0.5 group-hover:shadow-[var(--edge-highlight),var(--shadow-2)] motion-reduce:transition-none motion-reduce:group-hover:translate-y-0"
          : ""
      } ${className}`}
    >
      <div className="relative aspect-square overflow-hidden rounded-[calc(var(--radius-card)-0.5rem)] bg-[var(--color-surface-3)]">
        {children}
        {overlay}
      </div>
    </div>
  );
}

/** The title and the muted line under a framed cover ("Astro Bot" over
 *  "PS5 · 31 GB"), as under the reference's Recommended images. */
export function CoverCaption({
  title,
  meta,
  titleAttr,
}: {
  title: ReactNode;
  meta?: ReactNode;
  /** Full title for the tooltip when the visible one is clamped. */
  titleAttr?: string;
}) {
  return (
    <div className="min-w-0 px-1 pt-2.5">
      <div
        className="line-clamp-2 text-sm leading-5 font-medium tracking-[-0.005em]"
        title={titleAttr}
      >
        {title}
      </div>
      {meta ? (
        <div className="mt-0.5 flex min-w-0 items-center gap-1.5 text-xs text-[var(--color-muted)]">
          {meta}
        </div>
      ) : null}
    </div>
  );
}

/** The small dot between two parts of a caption's meta line. */
export function MetaDot() {
  return (
    <span aria-hidden className="h-1 w-1 shrink-0 rounded-full bg-[var(--color-muted)] opacity-60" />
  );
}

export type CoverTagTone = "neutral" | "ps4" | "ps5" | "good" | "warn" | "accent" | "bad";

const TAG_COLOR: Record<CoverTagTone, string> = {
  neutral: "var(--color-text)",
  ps4: "var(--color-ps4)",
  ps5: "var(--color-ps5)",
  good: "var(--color-good)",
  warn: "var(--color-warn)",
  accent: "var(--color-accent)",
  bad: "var(--color-bad)",
};

/**
 * A small label that sits ON cover art (platform, Playing, Installed…). The
 * soft tinted badges used on glass vanish against a colourful cover, so this
 * one stands on the near-opaque float surface with the tone in its text.
 */
export function CoverTag({
  tone = "neutral",
  icon,
  children,
  title,
}: {
  tone?: CoverTagTone;
  icon?: ReactNode;
  children: ReactNode;
  title?: string;
}) {
  return (
    <span
      title={title}
      className="inline-flex max-w-full shrink-0 items-center gap-1 truncate whitespace-nowrap rounded-full bg-[var(--color-float)] px-2 py-0.5 text-[0.6875rem] leading-4 font-semibold shadow-[var(--shadow-1)]"
      style={{ color: TAG_COLOR[tone] }}
    >
      {icon}
      {children}
    </span>
  );
}

/** "ps4" / "ps5" as a CoverTag; nothing for anything else (system, homebrew). */
export function PlatformTag({ platform }: { platform?: string | null }) {
  const p = platform?.toLowerCase();
  if (p !== "ps4" && p !== "ps5") return null;
  return <CoverTag tone={p}>{p === "ps5" ? "PS5" : "PS4"}</CoverTag>;
}
