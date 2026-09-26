/**
 * Shared progress bar — replaces the hand-rolled `h-1.5 rounded-full …`
 * fills that Upload, QueuePanel and FileSystem each wrote separately
 * (each with slightly different heights, colors and transitions).
 *
 * Two modes:
 *   determinate   — pass `value` (0..1); the fill animates width changes.
 *   indeterminate — omit `value`; renders a shimmer sweep so "working,
 *                   amount unknown" still reads as motion, not a frozen bar.
 *
 * `tone` recolors the fill for terminal states (good = finished,
 * bad = failed) so completion can reuse the same bar instead of
 * swapping components.
 *
 * `label` is REQUIRED for accessibility — it becomes the bar's
 * accessible name so screen readers announce *what* is progressing
 * ("Upload payload" vs. an unnamed bar). Existing call sites pass
 * nothing; new call sites must pass one.
 *
 * `paused` renders a striped overlay and announces "paused" to SRs.
 */
export function ProgressBar({
  value,
  tone = "accent",
  size = "md",
  label,
  paused = false,
  className = "",
}: {
  /** 0..1 fraction. Omit for an indeterminate shimmer. */
  value?: number | null;
  tone?: "accent" | "good" | "warn" | "bad";
  size?: "sm" | "md";
  /** Accessible name for the bar — announces what is progressing. */
  label?: string;
  /** Renders a striped overlay and announces "paused" to SRs. */
  paused?: boolean;
  className?: string;
}) {
  const h = size === "sm" ? "h-1" : "h-1.5";
  const toneVar = {
    accent: "var(--color-accent)",
    good: "var(--color-good)",
    warn: "var(--color-warn)",
    bad: "var(--color-bad)",
  }[tone];

  const determinate = typeof value === "number" && Number.isFinite(value);
  const pct = determinate ? Math.min(100, Math.max(0, value * 100)) : 0;

  const ariaLabel = paused && label ? `${label} (paused)` : label;

  return (
    <div
      role="progressbar"
      aria-label={ariaLabel}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={determinate ? Math.round(pct) : undefined}
      className={`${h} relative w-full overflow-hidden rounded-full bg-[var(--color-surface-3)] ${className}`}
    >
      {determinate ? (
        <div
          className="h-full rounded-full transition-[width] duration-300 ease-out"
          style={{ width: `${pct}%`, background: toneVar }}
        />
      ) : (
        <div className="anim-skeleton h-full w-full" />
      )}
      {paused && (
        <div
          aria-hidden="true"
          className="absolute inset-0 opacity-40"
          style={{
            backgroundImage:
              "repeating-linear-gradient(45deg, var(--color-surface) 0 4px, transparent 4px 8px)",
          }}
        />
      )}
    </div>
  );
}
