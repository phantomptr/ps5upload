// Pure helpers for the Game frame rate panel (#774): the graph's geometry, the axis, merging a
// capture's rows as they arrive, and the file name of an export.

import type { FpsSample } from "../../api/gameFps";

/** Rows further apart than this are drawn as a gap (no game for a while, or no answer). */
export const GAP_MS = 3_500;

/** The top of the graph: the next of 30, 60, 120, 144, 240 at or above what was seen. */
export function fpsAxisMax(maxFps: number): number {
  for (const top of [30, 60, 120, 144, 240]) {
    if (maxFps <= top + 0.5) return top;
  }
  return 240;
}

/** Grid lines for an axis top: every 30 up to 120, then the top itself. */
export function fpsGridLines(top: number): number[] {
  const lines: number[] = [];
  for (let v = 30; v <= Math.min(top, 120); v += 30) lines.push(v);
  if (top > 120) lines.push(top);
  return lines;
}

/**
 * The capture's line as polyline point lists, one per stretch without a gap. At most about
 * `width` points in all: each pixel column keeps its lowest and highest row, in time order, so a
 * one-second dip in a six-hour capture still shows.
 */
export function fpsChartSegments(
  samples: FpsSample[],
  width: number,
  height: number,
  top: number,
): string[] {
  if (samples.length === 0) return [];
  const t0 = samples[0].elapsed_ms;
  const span = Math.max(1, samples[samples.length - 1].elapsed_ms - t0);
  const x = (s: FpsSample) => ((s.elapsed_ms - t0) / span) * width;
  const y = (fps: number) => height - (Math.min(Math.max(fps, 0), top) / top) * height;
  const segments: string[][] = [];
  let cur: string[] = [];
  let prev: FpsSample | null = null;
  let col = -1;
  let colLow: FpsSample | null = null;
  let colHigh: FpsSample | null = null;
  const flushCol = () => {
    if (!colLow || !colHigh) return;
    const low: FpsSample = colLow;
    const high: FpsSample = colHigh;
    const pts = low === high ? [low] : low.elapsed_ms <= high.elapsed_ms ? [low, high] : [high, low];
    for (const s of pts) cur.push(`${x(s).toFixed(1)},${y(s.fps).toFixed(1)}`);
    colLow = colHigh = null;
  };
  for (const s of samples) {
    if (prev && s.elapsed_ms - prev.elapsed_ms > GAP_MS) {
      flushCol();
      if (cur.length) segments.push(cur);
      cur = [];
      col = -1;
    }
    const c = Math.floor(x(s));
    if (c !== col) {
      flushCol();
      col = c;
    }
    if (!colLow || s.fps < colLow.fps) colLow = s;
    if (!colHigh || s.fps > colHigh.fps) colHigh = s;
    prev = s;
  }
  flushCol();
  if (cur.length) segments.push(cur);
  // a single point is no line: give it a tiny width so it shows
  return segments.map((pts) => (pts.length === 1 ? `${pts[0]} ${bump(pts[0])}` : pts.join(" ")));
}

function bump(pt: string): string {
  const [px, py] = pt.split(",").map(Number);
  return `${(px + 1).toFixed(1)},${py.toFixed(1)}`;
}

/** Appends rows that came after `known` rows; a reply that starts earlier replaces the overlap. */
export function mergeSamples(known: FpsSample[], since: number, rows: FpsSample[]): FpsSample[] {
  if (since <= 0) return rows.slice();
  return known.slice(0, Math.min(since, known.length)).concat(rows);
}

/** `ps5-fps-2026-11-09-12-34-56.csv` from the capture's start (local time, so the user knows it). */
export function fpsCsvFileName(startedMs: number | undefined): string {
  if (!startedMs) return "ps5-fps.csv";
  const d = new Date(startedMs);
  const p = (n: number) => String(n).padStart(2, "0");
  return `ps5-fps-${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}-${p(d.getHours())}-${p(d.getMinutes())}-${p(d.getSeconds())}.csv`;
}

/** `1:02:03` or `2:03` from seconds. */
export function formatCaptureLength(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  const p = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${p(m)}:${p(r)}` : `${m}:${p(r)}`;
}

/** A frame rate as the panel shows it: whole frames, or one decimal for an average. */
export function formatFps(v: number | null | undefined, decimals = 0): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return "—";
  return v.toFixed(decimals);
}
