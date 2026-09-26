import { useTr } from "../state/lang";

/**
 * The app's signature loading mark: the four PlayStation face symbols
 * (triangle, circle, cross, square) pulsing in sequence. Used for
 * screen-level "waiting for the console" moments — connection probes,
 * library scans — where a generic spinner would be anonymous. Inline
 * row-level loading keeps the lightweight Loader2 spinner.
 *
 * Colors follow the classic DualShock palette but are mapped onto theme
 * tokens (good / bad / accent / ps5-violet) so all three themes — and
 * the light theme especially — keep contrast.
 *
 * Pure SVG + the shared ps-soft-pulse keyframe with staggered delays;
 * no JS animation, and prefers-reduced-motion freezes it via the global
 * motion guard.
 */
export function ShapesLoader({
  size = 18,
  className = "",
  label,
}: {
  /** Per-glyph size in px. */
  size?: number;
  className?: string;
  /** Optional caption rendered under the shapes. */
  label?: string;
}) {
  const tr = useTr();
  const common = {
    width: size,
    height: size,
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 2.4,
    strokeLinejoin: "round" as const,
    strokeLinecap: "round" as const,
    "aria-hidden": true,
  };
  const glyphs: Array<{ color: string; node: React.ReactNode }> = [
    {
      color: "var(--color-good)",
      node: <path d="M12 4.5 21 19.5 H3 Z" />, // triangle
    },
    {
      color: "var(--color-bad)",
      node: <circle cx={12} cy={12} r={8.5} />, // circle
    },
    {
      color: "var(--color-accent)",
      node: (
        <>
          <path d="M5.5 5.5 18.5 18.5" />
          <path d="M18.5 5.5 5.5 18.5" />
        </>
      ), // cross
    },
    {
      color: "var(--color-ps5)",
      node: <rect x={4.5} y={4.5} width={15} height={15} rx={1.5} />, // square
    },
  ];

  return (
    <div
      role="status"
      aria-label={label ?? tr("shapes_loader_default", "Loading")}
      className={`inline-flex flex-col items-center gap-3 ${className}`}
    >
      <div className="flex items-center" style={{ gap: size * 0.55 }}>
        {glyphs.map((g, i) => (
          <svg
            key={i}
            {...common}
            className="anim-status-pulse"
            style={{
              color: g.color,
              animationDelay: `${i * 220}ms`,
              animationDuration: "1.76s",
            }}
          >
            {g.node}
          </svg>
        ))}
      </div>
      {label && (
        <span className="text-xs text-[var(--color-muted)]">{label}</span>
      )}
    </div>
  );
}
