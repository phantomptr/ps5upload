import { describe, expect, it } from "vitest";
import { MAX_SPEED_CHANGE, RpRing } from "./rpAudioWorklet.js";

const RATE = 48000;
const FRAME = 480; // 10 ms, as the console sends
const BLOCK = 128; // a sound card's block
const HZ = 440;
const AMP = 0.5;
const W = (2 * Math.PI * HZ) / RATE;

/** Feed a 440 Hz tone in 10 ms frames from a source whose clock runs `k` times the sound card's,
 *  for `seconds`, pulling 128-sample blocks; `pauseAt`: the source stops for `pauseMs` there.
 *  The output, the fill after each block, and the ring. */
function run(o: { k?: number; seconds: number; targetMs?: number; pauseAt?: number; pauseMs?: number }) {
  const k = o.k ?? 1;
  const ring = new RpRing(RATE, RATE);
  ring.setTarget(((o.targetMs ?? 60) / 1000) * RATE);
  const blocks = Math.round((o.seconds * RATE) / BLOCK);
  const out = new Float32Array(blocks * BLOCK);
  const fill: number[] = [];
  let made = 0; // frames the source has sent
  let src = 0; // the source's sample count (its tone is continuous)
  for (let b = 0; b < blocks; b++) {
    const t = ((b + 1) * BLOCK) / RATE; // card time at the end of this block
    const paused = o.pauseAt !== undefined && t >= o.pauseAt && t < o.pauseAt + (o.pauseMs ?? 0) / 1000;
    // Everything the source would have sent by now (a paused one sends it all at once after).
    while (!paused && (made * FRAME) / (RATE * k) <= t) {
      const l = new Float32Array(FRAME);
      for (let i = 0; i < FRAME; i++) l[i] = AMP * Math.sin(W * (src + i));
      src += FRAME;
      made++;
      ring.push([l, l.slice()], 1);
    }
    const L = new Float32Array(BLOCK);
    const R = new Float32Array(BLOCK);
    ring.pull([L, R], BLOCK);
    out.set(L, b * BLOCK);
    fill.push((ring.n / RATE) * 1000);
  }
  return { out, fill, ring };
}

/** The largest second difference in `x` from `from` on: a clean tone's is AMP·ω² (~0.002 here);
 *  a skipped or repeated sample shows as a spike of about AMP·ω (~0.03), a click. */
function maxKink(x: Float32Array, from: number): number {
  let m = 0;
  for (let i = Math.max(1, from); i + 1 < x.length; i++) m = Math.max(m, Math.abs(x[i + 1] - 2 * x[i] + x[i - 1]));
  return m;
}
const CLEAN_KINK = AMP * W * W;

describe("RpRing (the Remote Play sound buffer)", () => {
  it("waits for its target, fades in, then plays the tone untouched when the clocks agree", () => {
    const { out, ring, fill } = run({ seconds: 5 });
    // Nothing until 60 ms are in.
    expect(out.slice(0, 2000).every((v) => v === 0)).toBe(true);
    const s = ring.stats();
    expect(s.under).toBe(0);
    expect(s.corrMs).toBe(0);
    // After the fade-in (10 ms), the tone is the source's own samples.
    expect(maxKink(out, 6000)).toBeLessThan(CLEAN_KINK * 1.5);
    const last = fill.slice(-200);
    expect(Math.min(...last)).toBeGreaterThan(45);
    expect(Math.max(...last)).toBeLessThan(75);
  });

  for (const k of [1.004, 0.996]) {
    it(`keeps the buffer at its target with a source ${k > 1 ? "faster" : "slower"} than the card (×${k}) without a click`, () => {
      const { out, ring, fill } = run({ k, seconds: 60 });
      const s = ring.stats();
      expect(s.under).toBe(0);
      expect(s.corrMs).toBeGreaterThan(1000);
      // Speed within its limit, and no skipped or repeated sample anywhere after the fade-in.
      expect(Math.abs(ring.speed - 1)).toBeLessThanOrEqual(MAX_SPEED_CHANGE + 1e-12);
      expect(maxKink(out, 6000)).toBeLessThan(CLEAN_KINK * 1.5);
      // Drift held: the last 10 s stay near 60 ms (a 0.4 % drift unchecked is 240 ms a minute).
      const last = fill.slice(-3750);
      expect(Math.min(...last)).toBeGreaterThan(35);
      expect(Math.max(...last)).toBeLessThan(85);
    });
  }

  it("corrects a real clock difference now and then, and plays at its own speed between (hysteresis)", () => {
    // 200 ppm: a sound card and the console differ by about that at most.
    const { ring, fill } = run({ k: 1.0002, seconds: 120 });
    const s = ring.stats();
    expect(s.corrMs).toBeGreaterThan(0);
    expect(s.corrMs).toBeLessThan(60_000);
    const last = fill.slice(-20_000);
    expect(Math.max(...last)).toBeLessThan(60 + 10 + 12);
    expect(run({ seconds: 30 }).ring.stats().corrMs).toBe(0);
  });

  it("fades out on running dry, counts it once, and fades back in", () => {
    const { out, ring } = run({ seconds: 3, pauseAt: 1, pauseMs: 200 });
    const s = ring.stats();
    expect(s.under).toBe(1);
    // No hard step into or out of the gap: no sample-to-sample jump beyond a tone's own.
    let jump = 0;
    for (let i = 1; i < out.length; i++) jump = Math.max(jump, Math.abs(out[i] - out[i - 1]));
    expect(jump).toBeLessThan(AMP * W * 1.5);
    // The minimum fill while playing is low, the average near the target.
    expect(s.minMs).toBeLessThan(5);
    expect(s.avgMs).toBeGreaterThan(30);
  });

  it("drops back to the target after a stall sent everything at once", () => {
    const ring = new RpRing(RATE, RATE);
    ring.setTarget(2880);
    const f = new Float32Array(FRAME).fill(0.1);
    for (let i = 0; i < 40; i++) ring.push([f], 1); // 400 ms at once
    expect(ring.n).toBeLessThanOrEqual(2880 * 3);
    expect(ring.stats().trimmedMs).toBeGreaterThan(0);
  });

  it("starts over on reset and keeps its stats", () => {
    const { ring } = run({ seconds: 2, pauseAt: 1, pauseMs: 300 });
    const under = ring.stats().under;
    ring.reset();
    expect(ring.n).toBe(0);
    expect(ring.primed).toBe(false);
    expect(ring.stats().under).toBe(under);
  });
});
