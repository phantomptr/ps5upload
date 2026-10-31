import { describe, expect, it } from "vitest";

import { ClockSync, LatencyStages, percentile, stagesLogText, stageValues } from "./rpLatency";

describe("percentile", () => {
  it("takes the nearest rank", () => {
    const s = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    expect(percentile(s, 0.5)).toBe(5);
    expect(percentile(s, 0.95)).toBe(10);
    expect(percentile([7], 0.95)).toBe(7);
    expect(percentile([], 0.5)).toBeNaN();
  });
});

describe("LatencyStages", () => {
  it("gives each stage's median and 95th percentile over the last 2 s", () => {
    const st = new LatencyStages();
    for (let i = 1; i <= 100; i++) st.add("decode", i, 10_000 + i);
    st.add("net", 6, 10_050);
    const s = st.summary(10_100);
    expect(s.decode).toEqual({ p50: 50, p95: 95 });
    expect(s.net).toEqual({ p50: 6, p95: 6 });
    expect(s.queue).toBeNull();
    expect(s.draw).toBeNull();
  });

  it("forgets samples older than the window", () => {
    const st = new LatencyStages();
    st.add("draw", 100, 1000);
    st.add("draw", 2, 4000);
    expect(st.summary(4500).draw).toEqual({ p50: 2, p95: 2 });
    expect(st.summary(7000).draw).toBeNull();
  });

  it("keeps a fixed number of samples, the newest", () => {
    const st = new LatencyStages();
    for (let i = 0; i < 1000; i++) st.add("queue", i < 900 ? 1000 : 1, 5000);
    expect(st.summary(5000).queue).toEqual({ p50: 1000, p95: 1000 });
    for (let i = 0; i < 256; i++) st.add("queue", 1, 5000);
    expect(st.summary(5000).queue).toEqual({ p50: 1, p95: 1 });
  });

  it("drops a value that is not a number and floors at zero", () => {
    const st = new LatencyStages();
    st.add("net", NaN, 0);
    st.add("net", -3, 0);
    expect(st.summary(0).net).toEqual({ p50: 0, p95: 0 });
    st.clear();
    expect(st.summary(0).net).toBeNull();
  });
});

describe("ClockSync", () => {
  it("lines the engine's clock up from the ping with the shortest round trip", () => {
    const c = new ClockSync();
    expect(c.offsetMs).toBeNull();
    // Engine clock = page clock + 5000. A slow pong (the engine stamped it early) is outdone by a
    // quick one.
    c.push(1000, (1000 + 5000 + 2) * 1000, 1040);
    c.push(2000, (2000 + 5000 + 1) * 1000, 2002);
    expect(c.offsetMs).toBe(5000);
  });

  it("ignores a pong from an engine that sends no clock", () => {
    const c = new ClockSync();
    c.push(1000, undefined, 1002);
    c.push("x", 5, 1002);
    expect(c.offsetMs).toBeNull();
  });
});

describe("stage text", () => {
  it("shows medians, and ? for a stage not measured (an older engine has no net)", () => {
    const s = { net: null, queue: { p50: 0.4, p95: 1 }, decode: { p50: 4.2, p95: 9 }, draw: { p50: 2, p95: 7.6 } };
    expect(stageValues(s)).toEqual({ net: "?", queue: "0", decode: "4", draw: "2" });
    expect(stagesLogText(s)).toBe("net ? · queue 0/1 · decode 4/9 · draw 2/8 ms (median/95th)");
    expect(stagesLogText(null)).toBe("no frames timed yet");
  });
});
