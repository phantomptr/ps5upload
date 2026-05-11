import { describe, expect, it } from "vitest";
import { formatBytes, formatDuration } from "./format";

describe("formatBytes", () => {
  it("returns dash for negative or non-finite input", () => {
    expect(formatBytes(-1)).toBe("—");
    expect(formatBytes(NaN)).toBe("—");
    expect(formatBytes(Infinity)).toBe("—");
    expect(formatBytes(-Infinity)).toBe("—");
  });

  it("renders bytes plainly under 1 KiB", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(1)).toBe("1 B");
    expect(formatBytes(1023)).toBe("1023 B");
  });

  it("rolls over to KiB at 1024 bytes", () => {
    expect(formatBytes(1024)).toBe("1.00 KiB");
    expect(formatBytes(1536)).toBe("1.50 KiB");
  });

  it("uses IEC binary units (not SI MB/GB)", () => {
    // 1 GiB exact; the prior buggy local formatBytes labeled this as MB
    expect(formatBytes(1024 * 1024 * 1024)).toBe("1.00 GiB");
    expect(formatBytes(1024 * 1024 * 1024 * 1024)).toBe("1.00 TiB");
  });

  it("scales precision with magnitude", () => {
    // <10: two decimals
    expect(formatBytes(1024 + 512)).toBe("1.50 KiB");
    // 10-99: one decimal
    expect(formatBytes(50 * 1024)).toBe("50.0 KiB");
    // ≥100: zero decimals
    expect(formatBytes(500 * 1024)).toBe("500 KiB");
  });

  it("caps at TiB instead of overflowing into PiB", () => {
    // 5 PiB rendered as the largest defined unit
    const fivePiB = 5 * 1024 * 1024 * 1024 * 1024 * 1024;
    expect(formatBytes(fivePiB)).toMatch(/TiB$/);
  });
});

describe("formatDuration", () => {
  it("returns dash for negative or non-finite", () => {
    expect(formatDuration(-1)).toBe("—");
    expect(formatDuration(NaN)).toBe("—");
    expect(formatDuration(Infinity)).toBe("—");
  });

  it("renders seconds under a minute", () => {
    expect(formatDuration(0)).toBe("0s");
    expect(formatDuration(1)).toBe("1s");
    expect(formatDuration(59)).toBe("59s");
    // sub-second rounds up so 0.1s never renders as 0s
    expect(formatDuration(0.1)).toBe("1s");
  });

  it("rolls over to minutes+seconds at 60s", () => {
    expect(formatDuration(60)).toBe("1m 0s");
    expect(formatDuration(125)).toBe("2m 5s");
    expect(formatDuration(3599)).toBe("59m 59s");
  });

  it("rolls over to hours+minutes at 3600s", () => {
    expect(formatDuration(3600)).toBe("1h 0m");
    expect(formatDuration(3661)).toBe("1h 1m");
    expect(formatDuration(7200)).toBe("2h 0m");
  });
});
