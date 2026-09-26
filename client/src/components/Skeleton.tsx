import { useEffect, useState } from "react";

/**
 * Loading placeholders. Before v3 every data screen jumped straight from
 * blank to fully-rendered content — on slow consoles (a 200k-file library
 * scan) that's seconds of dead space with no signal. Skeletons hold the
 * layout shape so arrival doesn't reflow the page, and the shimmer says
 * "working" without a spinner.
 *
 * `Skeleton` is one shimmering block; `SkeletonRows` is the common
 * list-screen arrangement (n list rows at a realistic height). SkeletonRows
 * is delay-gated so a fast scan doesn't flash a wall of shimmer.
 */
export function Skeleton({ className = "" }: { className?: string }) {
  return (
    <div
      aria-hidden
      className={`anim-skeleton rounded-md border border-[var(--color-border)] ${className}`}
    />
  );
}

export function SkeletonRows({
  rows = 6,
  rowClassName = "h-12",
  /** Wait this long before showing the skeleton. A scan that resolves (or a
   *  transient `entries === null` blip during a refresh) faster than this
   *  never flashes the placeholder — which is what users perceived as the
   *  list "flickering" below loaded content. */
  delayMs = 250,
}: {
  rows?: number;
  rowClassName?: string;
  delayMs?: number;
}) {
  const [show, setShow] = useState(delayMs <= 0);
  useEffect(() => {
    if (delayMs <= 0) return;
    const id = window.setTimeout(() => setShow(true), delayMs);
    return () => window.clearTimeout(id);
  }, [delayMs]);
  if (!show) return null;
  return (
    <div aria-hidden role="presentation" className="space-y-2">
      {Array.from({ length: rows }, (_, i) => (
        <div
          key={i}
          className={`anim-skeleton rounded-md border border-[var(--color-border)] ${rowClassName}`}
          // Stagger the shimmer start so the column doesn't strobe in
          // lockstep — reads as texture, not a blinking wall.
          style={{ animationDelay: `${i * 90}ms` }}
        />
      ))}
    </div>
  );
}
