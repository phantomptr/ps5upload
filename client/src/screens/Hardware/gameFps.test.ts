import { describe, expect, it } from "vitest";
import type { FpsSample } from "../../api/gameFps";
import { parseFpsLive } from "../../api/gameFps";
import {
  fpsAxisMax,
  fpsChartSegments,
  fpsCsvFileName,
  fpsGridLines,
  formatCaptureLength,
  formatFps,
  mergeSamples,
} from "./gameFps";

const row = (elapsed_s: number, fps: number): FpsSample => ({
  t_ms: 1_000_000 + elapsed_s * 1000,
  elapsed_ms: elapsed_s * 1000,
  fps,
  frames: Math.round(fps),
  interval_ms: 1000,
});

describe("game frame rate panel helpers", () => {
  it("picks an axis top that holds what was seen", () => {
    expect(fpsAxisMax(0)).toBe(30);
    expect(fpsAxisMax(30.2)).toBe(30);
    expect(fpsAxisMax(59.9)).toBe(60);
    expect(fpsAxisMax(61)).toBe(120);
    expect(fpsAxisMax(500)).toBe(240);
    expect(fpsGridLines(60)).toEqual([30, 60]);
    expect(fpsGridLines(144)).toEqual([30, 60, 90, 120, 144]);
  });

  it("draws one line per stretch, with a gap where rows are missing", () => {
    const rows = [row(0, 60), row(1, 60), row(2, 30), row(10, 60), row(11, 60)];
    const segs = fpsChartSegments(rows, 110, 60, 60);
    expect(segs).toHaveLength(2);
    expect(segs[0].split(" ")).toEqual(["0.0,0.0", "10.0,0.0", "20.0,30.0"]);
    expect(segs[1].split(" ")).toEqual(["100.0,0.0", "110.0,0.0"]);
    expect(fpsChartSegments([], 100, 60, 60)).toEqual([]);
    // a single row still shows as a short line
    expect(fpsChartSegments([row(0, 60)], 100, 60, 60)).toEqual(["0.0,0.0 1.0,0.0"]);
  });

  it("keeps a short dip in a long capture", () => {
    const rows: FpsSample[] = [];
    for (let s = 0; s < 3600; s++) rows.push(row(s, s === 1800 ? 12 : 60));
    const segs = fpsChartSegments(rows, 100, 60, 60);
    expect(segs).toHaveLength(1);
    const pts = segs[0].split(" ");
    expect(pts.length).toBeLessThanOrEqual(2 * 101);
    // 12 FPS on a 60 FPS axis 60 px tall sits at y = 48
    expect(pts.some((p) => p.endsWith(",48.0"))).toBe(true);
  });

  it("merges the rows that arrive after the ones known", () => {
    const a = [row(0, 60), row(1, 59)];
    expect(mergeSamples(a, 2, [row(2, 58)]).map((r) => r.fps)).toEqual([60, 59, 58]);
    expect(mergeSamples(a, 0, [row(0, 1)]).map((r) => r.fps)).toEqual([1]);
    expect(mergeSamples(a, 1, [row(1, 2)]).map((r) => r.fps)).toEqual([60, 2]);
  });

  it("formats lengths, rates and file names", () => {
    expect(formatCaptureLength(65)).toBe("1:05");
    expect(formatCaptureLength(3723)).toBe("1:02:03");
    expect(formatFps(59.6)).toBe("60");
    expect(formatFps(57.25, 1)).toBe("57.3");
    expect(formatFps(null)).toBe("—");
    expect(fpsCsvFileName(undefined)).toBe("ps5-fps.csv");
    expect(fpsCsvFileName(new Date(2026, 10, 9, 8, 5, 3).getTime())).toBe("ps5-fps-2026-11-09-08-05-03.csv");
  });

  it("reads a live reply however odd", () => {
    const live = parseFpsLive({
      state: "ok",
      fps: 60,
      at_ms: 5,
      capture: { active: true, samples: 3, stats: { min_fps: 1 } },
    });
    expect(live.state).toBe("ok");
    expect(live.fps).toBe(60);
    expect(live.capture.active).toBe(true);
    expect(live.capture.samples).toBe(3);
    const odd = parseFpsLive({ state: "weird", fps: "x" });
    expect(odd.state).toBe("failed");
    expect(odd.fps).toBeNull();
    expect(odd.capture).toMatchObject({ active: false, samples: 0 });
    expect(parseFpsLive(null).state).toBe("failed");
  });
});
