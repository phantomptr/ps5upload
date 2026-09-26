import { describe, expect, it } from "vitest";

import { consoleFirmware, firmwareParts } from "./firmware";

describe("consoleFirmware", () => {
  it("reads the packed kernel word as major.minor", () => {
    expect(consoleFirmware(0x05100000)).toBe("5.10");
    expect(consoleFirmware(0x09600010)).toBe("9.60");
    expect(consoleFirmware(0x10200000)).toBe("10.20");
  });
  it("is null when the payload could not read it", () => {
    expect(consoleFirmware(0)).toBeNull();
  });
});

describe("firmwareParts", () => {
  it("parses both the engine's and a user's spelling", () => {
    expect(firmwareParts("10.20")).toEqual([10, 20]);
    expect(firmwareParts("05.10")).toEqual([5, 10]);
    expect(firmwareParts("9.6")).toEqual([9, 60]);
  });
  it("rejects anything else", () => {
    expect(firmwareParts("0x0510000000000000")).toBeNull();
    expect(firmwareParts("")).toBeNull();
    expect(firmwareParts(null)).toBeNull();
  });
});
