import { describe, expect, it } from "vitest";

import { probeVerdict } from "./probeVerdict";

describe("probeVerdict", () => {
  it("blocks a file that couldn't be read, and says why", () => {
    const v = probeVerdict("payload_probe_read_error", false, "open: No such file");
    expect(v.blocked).toBe(true);
    expect(v.vars?.error).toBe("open: No such file");
    expect(v.fallback).not.toMatch(/looks OK/);
  });

  it("blocks an empty file", () => {
    const v = probeVerdict("payload_probe_too_small", false);
    expect(v.blocked).toBe(true);
    expect(v.fallback).toMatch(/empty/);
  });

  it("lets a readable file through, with or without our signature", () => {
    expect(probeVerdict("payload_probe_detected", true)).toMatchObject({ blocked: false, good: true });
    expect(probeVerdict("payload_probe_no_signature", false)).toMatchObject({ blocked: false, good: false });
    expect(probeVerdict("payload_probe_no_signature", true)).toMatchObject({ blocked: false, good: true });
    expect(probeVerdict("payload_probe_invalid_ext", false).blocked).toBe(false);
  });
});
