import { describe, expect, it } from "vitest";

import { PictureStats } from "./watchPlayer";

describe("picture stats (a Play Together guest's report to the host)", () => {
  it("counts pictures, late arrivals, drops, key waits, decode time and stalls", () => {
    let t = 1000;
    const s = new PictureStats("webcodecs", () => t);
    // Two pictures 16 ms apart, decoded in 4 ms each.
    for (let i = 0; i < 2; i++) {
      s.arrived();
      s.decoding();
      t += 4;
      s.drawn(true);
      t += 12;
    }
    // A 300 ms hole: late, and a stall when the next picture is drawn.
    t += 300;
    s.arrived();
    s.drop();
    s.drop(); // still the same wait for a keyframe
    s.arrived();
    s.decoding();
    t += 4;
    s.drawn(true);
    const v = s.snapshot();
    expect(v).toMatchObject({ frames: 3, dropped: 2, late: 1, stalls: 1, key_waits: 1, decoder: "webcodecs" });
    expect(v.decode_ms).toBe(4);
    expect(v.stall_ms).toBe(316);
    expect(v.worst_stall_ms).toBe(316);
    // The decode time is per report: none since.
    expect(s.snapshot().decode_ms).toBe(0);
  });

  it("counts a stall still going up to now, and never counts it twice", () => {
    let t = 0;
    const s = new PictureStats("mse", () => t);
    t = 10;
    s.drawn();
    t = 1010;
    const during = s.snapshot();
    expect(during).toMatchObject({ stalls: 1, stall_ms: 1000 });
    t = 1510;
    s.drawn();
    const after = s.snapshot();
    expect(after).toMatchObject({ stalls: 1, stall_ms: 1500, frames: 2 });
    expect(Number(after.stall_ms)).toBeGreaterThanOrEqual(Number(during.stall_ms));
  });
});
