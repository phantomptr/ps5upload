import { describe, it, expect } from "vitest";
import { formatFirmware } from "./index";

describe("formatFirmware", () => {
  it("decodes BCD rather than binary", () => {
    // Real magics from the two test consoles. Read as plain integers these
    // render as "5.16" and "9.96", which is the bug this pins.
    expect(formatFirmware(0x05100023)).toBe("5.10");
    expect(formatFirmware(0x09600004)).toBe("9.60");
  });

  it("does not turn a major of ten into sixteen", () => {
    // 10.00 is where per-user Remote Play arrives, so mislabelling it
    // would mislead about exactly the firmware that matters most.
    expect(formatFirmware(0x10000000)).toBe("10.00");
    expect(formatFirmware(0x12700000)).toBe("12.70");
    expect(formatFirmware(0x13200000)).toBe("13.20");
  });

  it("zero-pads a single-digit minor", () => {
    expect(formatFirmware(0x09000000)).toBe("9.00");
    expect(formatFirmware(0x07010000)).toBe("7.01");
  });
});
