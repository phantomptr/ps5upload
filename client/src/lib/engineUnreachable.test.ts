import { describe, expect, it } from "vitest";
import { engineProbeVerdict, isEngineUnreachable } from "./engineUnreachable";

describe("engineProbeVerdict", () => {
  it("does not call a running engine down on one slow probe", () => {
    expect(engineProbeVerdict("up", false, 1)).toBeNull();
    expect(engineProbeVerdict("up", false, 2)).toBe("down");
  });

  it("answers at once otherwise", () => {
    expect(engineProbeVerdict("up", true, 0)).toBe("up");
    expect(engineProbeVerdict("down", true, 0)).toBe("up");
    expect(engineProbeVerdict("down", false, 1)).toBe("down");
    expect(engineProbeVerdict("unknown", false, 1)).toBe("down");
  });
});

describe("isEngineUnreachable", () => {
  it("knows the desktop proxy's wording", () => {
    expect(isEngineUnreachable("error sending request for url (http://127.0.0.1:19113/api/x)")).toBe(true);
    expect(isEngineUnreachable("connection refused")).toBe(false);
  });
});
