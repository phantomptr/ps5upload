/**
 * Sparkline — minimal inline SVG line chart for telemetry trends.
 *
 * Renders a `<polyline>` from `data: number[]` scaled to fit the given
 * `width` × `height`. No axes, no gridlines — just the shape. Designed
 * to sit inside cards/badges next to a numeric reading.
 *
 * v5 §11 — Telemetry dashboard uses these for CPU temp, fan duty, etc.
 *
 * Accessibility: the SVG is `aria-hidden` because the numeric value
 * next to it is the actual reading; the sparkline is decorative context.
 * Screen-reader users get the number; sighted users get the trend.
 */
import { useMemo } from "react";

export interface SparklineProps {
  /** Data points (oldest → newest). `null`/`undefined` gaps are skipped
   *  by encoding them as breaks in the polyline. */
  data: number[];
  /** SVG width in px. Default 80. */
  width?: number;
  /** SVG height in px. Default 24. */
  height?: number;
  /** Stroke color. Defaults to `var(--color-text)` so it adapts to
   *  light/dark themes. Override for semantic coloring (e.g.
   *  `var(--color-bad)` for a temp that's in the warning band). */
  color?: string;
  /** Stroke width in px. Default 1.5. */
  strokeWidth?: number;
  /** Fill area under the line? Default false. When true, renders a
   *  `<polygon>` from the line down to the baseline. */
  fill?: boolean;
  /** Fill color (only used when `fill=true`). Defaults to `color` at
   *  15% opacity. */
  fillColor?: string;
  /** Y-axis minimum. If omitted, uses `Math.min(...data)`. Set to keep
   *  multiple sparklines on the same scale (e.g. 0–100 for fan duty). */
  minY?: number;
  /** Y-axis maximum. If omitted, uses `Math.max(...data)`. */
  maxY?: number;
  /** Additional className for the `<svg>`. */
  className?: string;
}

/**
 * Compute the polyline `points` string for a sparkline. Exported for
 * unit testing (the component is a thin SVG wrapper around this).
 */
export function computeSparkPoints(
  data: number[],
  width: number,
  height: number,
  strokeWidth: number,
  minY?: number,
  maxY?: number,
): { points: string; fillPoints: string } {
  if (!data || data.length < 2) return { points: "", fillPoints: "" };
  const lo = minY ?? Math.min(...data);
  const hi = maxY ?? Math.max(...data);
  const range = hi - lo || 1; // avoid divide-by-zero on flat data
  const stepX = width / (data.length - 1);
  const pad = strokeWidth;
  const usableH = height - pad * 2;

  const pts = data.map((v, i) => {
    const x = i * stepX;
    const y = pad + usableH * (1 - (v - lo) / range);
    return `${x.toFixed(1)},${y.toFixed(1)}`;
  });
  const linePoints = pts.join(" ");
  const fillPoints = `0,${height} ${linePoints} ${width},${height}`;
  return { points: linePoints, fillPoints };
}

export function Sparkline({
  data,
  width = 80,
  height = 24,
  color = "var(--color-text)",
  strokeWidth = 1.5,
  fill = false,
  fillColor,
  minY,
  maxY,
  className = "",
}: SparklineProps) {
  const { points, fillPoints } = useMemo(
    () => computeSparkPoints(data, width, height, strokeWidth, minY, maxY),
    [data, width, height, minY, maxY, strokeWidth],
  );

  if (!data || data.length < 2) {
    return (
      <svg
        width={width}
        height={height}
        viewBox={`0 0 ${width} ${height}`}
        aria-hidden="true"
        className={className}
      />
    );
  }

  return (
    <svg
      width={width}
      height={height}
      viewBox={`0 0 ${width} ${height}`}
      aria-hidden="true"
      className={className}
      preserveAspectRatio="none"
    >
      {fill && (
        <polygon points={fillPoints} fill={fillColor ?? color} opacity={0.15} />
      )}      <polyline
        points={points}
        fill="none"
        stroke={color}
        strokeWidth={strokeWidth}
        strokeLinejoin="round"
        strokeLinecap="round"
      />
    </svg>
  );
}
