import { describe, expect, it } from "vitest";
import { formatBootCycles, unavailableNote } from "./PowerTelemetryPanel";

describe("formatBootCycles", () => {
  it("shows a plausible count", () => {
    expect(formatBootCycles(1234)).toBe((1234).toLocaleString());
    expect(formatBootCycles(0)).toBe("0");
  });

  it("refuses a count the console cannot have reached", () => {
    // FW 5.10 returns 0x01010000 here — a misread field, not a reading.
    // Boot count is the one number on this panel a user might act on, so a
    // wrong one is worse than a blank.
    expect(formatBootCycles(16842752)).toBe("—");
    expect(formatBootCycles(-1)).toBe("—");
  });

  it("renders absent values as a dash", () => {
    expect(formatBootCycles(null)).toBe("—");
    expect(formatBootCycles(undefined)).toBe("—");
  });
});

describe("unavailableNote", () => {
  it("explains each way the readout can be empty", () => {
    expect(unavailableNote({ status: "unsupported_firmware" })).toContain(
      "doesn't expose",
    );
    expect(unavailableNote({ status: "calls_failed" })).toContain("reboot");
    expect(unavailableNote({ status: "partial" })).toContain("only some");
  });

  it("says nothing when there is nothing to explain", () => {
    // "ok", or a payload too old to report status. Inventing a reason would
    // be worse than staying quiet.
    expect(unavailableNote({ status: "ok" })).toBeNull();
    expect(unavailableNote({})).toBeNull();
    expect(unavailableNote({ status: null })).toBeNull();
  });
});
