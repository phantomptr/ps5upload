import { describe, expect, it } from "vitest";

import {
  BANNER_FILE_COUNT_THRESHOLD,
  DEFAULT_COMMIT_MS_PER_FILE,
  DEFAULT_THROUGHPUT_MIBPS,
  SIMPLIFIED_BANNER_BAND_MIN,
  computeUploadEta,
  formatEtaSeconds,
  pickBannerMode,
} from "./uploadEta";

const MIB = 1024 * 1024;
const GIB = 1024 * MIB;

describe("computeUploadEta — happy path with defaults", () => {
  it("returns zeros for an empty folder", () => {
    const eta = computeUploadEta({ fileCount: 0, totalBytes: 0 });
    expect(eta.transferSeconds).toBe(0);
    expect(eta.commitSeconds).toBe(0);
    expect(eta.totalSeconds).toBe(0);
    expect(eta.usedDefaults).toBe(true);
  });

  it("estimates a small 1k-file / 1 GiB folder at gigabit default", () => {
    const eta = computeUploadEta({ fileCount: 1000, totalBytes: GIB });
    // 1 GiB / 100 MiB/s = 10.24 s
    expect(eta.transferSeconds).toBeCloseTo(10.24, 1);
    // 1000 files * 10 ms = 10 s
    expect(eta.commitSeconds).toBeCloseTo(10, 5);
    expect(eta.totalSeconds).toBeCloseTo(20.24, 1);
    expect(eta.usedDefaults).toBe(true);
  });

  it("matches the Ghost of Yotei reference case (84,216 files / 185 GiB)", () => {
    // The user-reported reference workload from the design doc.
    // Default throughput (100 MiB/s) + default commit (10 ms/file).
    // Expected: transfer ≈ 1894 s (~31 min), commit ≈ 842 s (~14 min),
    // total ≈ 2736 s (~45 min). The 45-minute target was the headline
    // number in the design's executive summary.
    const eta = computeUploadEta({
      fileCount: 84_216,
      totalBytes: 185 * GIB,
    });
    expect(eta.transferSeconds).toBeCloseTo(185 * 1024 / 100, 0); // 1894 s
    expect(eta.commitSeconds).toBeCloseTo(842.16, 1);
    expect(eta.totalSeconds).toBeCloseTo(2736.5, 0);
    // Round-to-minute sanity: total ~45 min
    expect(Math.round(eta.totalSeconds / 60)).toBe(46);
    expect(eta.usedDefaults).toBe(true);
  });
});

describe("computeUploadEta — measured per-host overrides", () => {
  it("uses measured throughput when supplied", () => {
    const eta = computeUploadEta({
      fileCount: 0,
      totalBytes: 100 * MIB,
      throughputMibps: 50, // half-gigabit, e.g. typical Wi-Fi
    });
    // 100 MiB / 50 MiB/s = 2 s
    expect(eta.transferSeconds).toBeCloseTo(2, 5);
    // No commit work, no files
    expect(eta.commitSeconds).toBe(0);
    // commitMsPerFile not provided → used default → flagged
    expect(eta.usedDefaults).toBe(true);
  });

  it("uses measured commit-ms when supplied", () => {
    const eta = computeUploadEta({
      fileCount: 10_000,
      totalBytes: 0,
      commitMsPerFile: 25, // slow USB stick
    });
    expect(eta.transferSeconds).toBe(0);
    // 10k * 25 ms = 250 s
    expect(eta.commitSeconds).toBeCloseTo(250, 5);
    // throughputMibps not provided → still flagged
    expect(eta.usedDefaults).toBe(true);
  });

  it("clears the usedDefaults flag only when both are supplied", () => {
    const eta = computeUploadEta({
      fileCount: 1000,
      totalBytes: GIB,
      throughputMibps: 120,
      commitMsPerFile: 8,
    });
    expect(eta.usedDefaults).toBe(false);
  });
});

describe("computeUploadEta — defensive against garbage inputs", () => {
  it("clamps negative file count to zero", () => {
    const eta = computeUploadEta({ fileCount: -5, totalBytes: GIB });
    expect(eta.commitSeconds).toBe(0);
  });

  it("clamps negative byte count to zero", () => {
    const eta = computeUploadEta({ fileCount: 10, totalBytes: -1 });
    expect(eta.transferSeconds).toBe(0);
  });

  it("falls back to default throughput on NaN", () => {
    const eta = computeUploadEta({
      fileCount: 0,
      totalBytes: 100 * MIB,
      throughputMibps: NaN,
    });
    // NaN → default 100 MiB/s → 1 s
    expect(eta.transferSeconds).toBeCloseTo(1, 5);
    expect(eta.usedDefaults).toBe(true);
  });

  it("falls back to default throughput on Infinity", () => {
    const eta = computeUploadEta({
      fileCount: 0,
      totalBytes: 100 * MIB,
      throughputMibps: Infinity,
    });
    expect(eta.transferSeconds).toBeCloseTo(1, 5);
    expect(eta.usedDefaults).toBe(true);
  });

  it("falls back to default throughput on zero (would divide by zero)", () => {
    const eta = computeUploadEta({
      fileCount: 0,
      totalBytes: 100 * MIB,
      throughputMibps: 0,
    });
    // Zero is treated as "not a valid measurement"
    expect(eta.transferSeconds).toBeCloseTo(1, 5);
    expect(eta.usedDefaults).toBe(true);
  });

  it("falls back to default commit-ms on negative", () => {
    const eta = computeUploadEta({
      fileCount: 100,
      totalBytes: 0,
      commitMsPerFile: -50,
    });
    // -50 ms is nonsense → default 10 ms → 1 s
    expect(eta.commitSeconds).toBeCloseTo(1, 5);
    expect(eta.usedDefaults).toBe(true);
  });
});

describe("pickBannerMode", () => {
  it("hides the banner for tiny folders", () => {
    expect(pickBannerMode(0)).toBe("hidden");
    expect(pickBannerMode(999)).toBe("hidden");
  });

  it("simplifies for medium folders", () => {
    expect(pickBannerMode(1_000)).toBe("simplified");
    expect(pickBannerMode(5_000)).toBe("simplified");
    expect(pickBannerMode(9_999)).toBe("simplified");
  });

  it("uses the detailed table for huge folders", () => {
    expect(pickBannerMode(10_000)).toBe("detailed");
    expect(pickBannerMode(84_216)).toBe("detailed"); // GoY reference
    expect(pickBannerMode(500_000)).toBe("detailed");
  });

  it("pins the thresholds to the exported constants", () => {
    // Defends against future drift between the constants and the
    // banner-mode boundaries.
    expect(pickBannerMode(SIMPLIFIED_BANNER_BAND_MIN - 1)).toBe("hidden");
    expect(pickBannerMode(SIMPLIFIED_BANNER_BAND_MIN)).toBe("simplified");
    expect(pickBannerMode(BANNER_FILE_COUNT_THRESHOLD - 1)).toBe("simplified");
    expect(pickBannerMode(BANNER_FILE_COUNT_THRESHOLD)).toBe("detailed");
  });
});

describe("formatEtaSeconds", () => {
  it("rounds sub-minute durations to whole seconds", () => {
    expect(formatEtaSeconds(0)).toBe("0 sec");
    expect(formatEtaSeconds(30)).toBe("30 sec");
    expect(formatEtaSeconds(59.4)).toBe("59 sec");
  });

  it("formats minutes for sub-hour durations", () => {
    expect(formatEtaSeconds(60)).toBe("1 min");
    expect(formatEtaSeconds(750)).toBe("13 min");
    expect(formatEtaSeconds(45 * 60)).toBe("45 min");
  });

  it("formats hours + minutes for multi-hour durations", () => {
    expect(formatEtaSeconds(3600)).toBe("1 h");
    expect(formatEtaSeconds(3600 + 23 * 60)).toBe("1 h 23 min");
    expect(formatEtaSeconds(2 * 3600 + 30 * 60)).toBe("2 h 30 min");
  });

  it("drops minutes once we cross 24 h", () => {
    expect(formatEtaSeconds(26 * 3600)).toBe("26 h");
  });

  it("returns dash for nonsense input", () => {
    expect(formatEtaSeconds(-5)).toBe("—");
    expect(formatEtaSeconds(NaN)).toBe("—");
    expect(formatEtaSeconds(Infinity)).toBe("—");
  });
});

describe("DEFAULT_* constants — pin documented values", () => {
  // If these change, every banner copy that hardcodes "~100 MiB/s" or
  // "~10 ms per file" goes stale. Pin so the docstring stays honest.
  it("throughput default is 100 MiB/s", () => {
    expect(DEFAULT_THROUGHPUT_MIBPS).toBe(100);
  });
  it("commit-ms default is 10 ms per file", () => {
    expect(DEFAULT_COMMIT_MS_PER_FILE).toBe(10);
  });
});
