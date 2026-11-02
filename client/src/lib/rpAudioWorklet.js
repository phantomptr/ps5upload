// The Remote Play sound player's audio worklet (see remotePlayStream.ts): a ring buffer the
// decoded sound goes into and the sound card reads from, on the audio thread.
//
// A file of the app's own, not a blob: URL: the desktop and phone apps' Content-Security-Policy
// allows scripts only from the app itself (default-src 'self'), and a worklet made from a blob
// was refused there, so the apps played every 10 ms of sound as its own buffer scheduled from
// the page's main thread, which crackled whenever the page was busy.
//
// Plain JavaScript as the browser runs it (copied as it is, not built): nothing newer than
// Safari 13 has (no ?. or ??). RpRing is the whole player and has no audio-thread parts, so it
// is tested on its own (rpAudioWorklet.test.ts).
/* global AudioWorkletProcessor, registerProcessor, sampleRate */

/** The furthest the playback speed moves from 1 to correct drift: ±0.5 %, a pitch change of
 *  under a tenth of a semitone, which nobody hears (a sound card and the console differ by far
 *  less than that). */
export const MAX_SPEED_CHANGE = 0.005;
/** The buffer this far (ms) off its target, smoothed, starts a correction; back within EXIT_MS
 *  ends it. Hysteresis: the buffer moves by a whole 10 ms frame at each push, and a correction
 *  that went on and off with it would wobble. */
export const ENTER_MS = 10;
export const EXIT_MS = 2;
/** How fast the smoothed fill follows the real one, per 128-sample block (~130 ms). */
const AVG_PER_BLOCK = 0.02;
/** The most the speed changes per block: a glide, not a step. */
const SPEED_SLEW = 0.0002;
/** Fade in over 10 ms after a gap (48 kHz), out by 5 % a sample. */
const FADE_IN = 480;

export class RpRing {
  /** `cap` samples per channel; `rate` the sound card's sample rate. */
  constructor(cap, rate) {
    this.cap = cap;
    this.rate = rate || 48000;
    this.buf = [new Float32Array(cap), new Float32Array(cap)];
    this.r = 0;
    this.w = 0;
    this.n = 0;
    // The read position between sample r and r + 1.
    this.frac = 0;
    this.target = 2880;
    this.primed = false;
    this.gain = 0;
    this.last = [0, 0];
    this.speed = 1;
    this.avg = -1;
    // 0, or the direction of the correction under way (1: ahead, playing faster; -1: behind).
    this.correcting = 0;
    // For the stats, since the start.
    this.under = 0;
    this.corrSamples = 0;
    this.sumFill = 0;
    this.blocks = 0;
    this.minFill = -1;
    this.trimmed = 0;
  }

  /** The fill to keep, in samples. */
  setTarget(samples) {
    this.target = Math.max(48, Math.min(Math.floor(this.cap / 4), Math.round(samples)));
    this.avg = -1;
  }

  /** Empty, waiting for `target` again (a new stream; the stats go on). */
  reset() {
    this.r = this.w = this.n = 0;
    this.frac = 0;
    this.primed = false;
    this.avg = -1;
    this.correcting = 0;
    this.speed = 1;
  }

  /** Add one decoded frame: its channels (one or two), times `scale`. */
  push(planes, scale) {
    const len = planes[0].length;
    const cap = this.cap;
    for (let i = 0; i < len; i++) {
      if (this.n === cap) {
        this.r = (this.r + 1) % cap;
        this.n--;
      }
      for (let c = 0; c < 2; c++) this.buf[c][this.w] = planes[c % planes.length][i] * scale;
      this.w = (this.w + 1) % cap;
      this.n++;
    }
    // Far ahead after a stall (the page stopped, then everything came at once): back to the
    // target in one go rather than seconds of late sound.
    if (this.n > this.target * 3) {
      const drop = this.n - this.target;
      this.r = (this.r + drop) % cap;
      this.n -= drop;
      this.trimmed += drop;
      this.avg = -1;
    }
  }

  /** The speed for the next block, from the smoothed fill against the target. */
  steer() {
    if (this.avg < 0) this.avg = this.n;
    else this.avg += (this.n - this.avg) * AVG_PER_BLOCK;
    const err = this.avg - this.target;
    const perMs = this.rate / 1000;
    if (!this.correcting && Math.abs(err) > ENTER_MS * perMs) this.correcting = err > 0 ? 1 : -1;
    else if (this.correcting && err * this.correcting <= EXIT_MS * perMs) this.correcting = 0;
    // 20 ms off: the full ±0.5 %; less, proportionally less.
    let want = 1;
    if (this.correcting) {
      want = 1 + (err / (20 * perMs)) * MAX_SPEED_CHANGE;
      if (want > 1 + MAX_SPEED_CHANGE) want = 1 + MAX_SPEED_CHANGE;
      if (want < 1 - MAX_SPEED_CHANGE) want = 1 - MAX_SPEED_CHANGE;
    }
    const d = want - this.speed;
    this.speed = d > SPEED_SLEW ? this.speed + SPEED_SLEW : d < -SPEED_SLEW ? this.speed - SPEED_SLEW : want;
  }

  /** Fill `out` (one array per output channel) with `len` samples. */
  pull(out, len) {
    if (!this.primed && this.n >= this.target) {
      this.primed = true;
      this.gain = 0;
      this.avg = -1;
      // More came in than the target before playing started (the first frames arrive in a
      // burst): start at the target, silently, rather than spend seconds catching up.
      if (this.n > this.target) {
        const drop = this.n - this.target;
        this.r = (this.r + drop) % this.cap;
        this.n -= drop;
        this.frac = 0;
      }
    }
    if (this.primed) this.steer();
    const cap = this.cap;
    const chans = out.length;
    const b0 = this.buf[0];
    const b1 = this.buf[1];
    for (let i = 0; i < len; i++) {
      // Two samples to read between (linear interpolation).
      if (!this.primed || this.n < 2) {
        if (this.primed) {
          this.primed = false;
          this.under++;
          this.correcting = 0;
          this.speed = 1;
        }
        // Ease the last sample to silence instead of a hard step (a click).
        this.last[0] *= 0.95;
        this.last[1] *= 0.95;
        for (let c = 0; c < chans; c++) out[c][i] = this.last[c % 2];
        continue;
      }
      const r1 = this.r + 1 === cap ? 0 : this.r + 1;
      const f = this.frac;
      const g = this.gain;
      const v0 = (b0[this.r] + (b0[r1] - b0[this.r]) * f) * g;
      const v1 = (b1[this.r] + (b1[r1] - b1[this.r]) * f) * g;
      if (chans > 0) out[0][i] = v0;
      if (chans > 1) out[1][i] = v1;
      for (let c = 2; c < chans; c++) out[c][i] = c % 2 ? v1 : v0;
      this.last[0] = v0;
      this.last[1] = v1;
      if (g < 1) this.gain = Math.min(1, g + 1 / FADE_IN);
      this.frac += this.speed;
      while (this.frac >= 1 && this.n > 1) {
        this.frac -= 1;
        this.r = this.r + 1 === cap ? 0 : this.r + 1;
        this.n--;
      }
    }
    if (this.speed !== 1) this.corrSamples += len;
    if (this.primed) {
      this.blocks++;
      this.sumFill += this.n;
      if (this.minFill < 0 || this.n < this.minFill) this.minFill = this.n;
    }
  }

  /** What the page shows and logs: ms waiting now, underruns, the average and lowest fill (ms)
   *  while playing, ms played at a corrected speed, ms dropped after stalls. */
  stats() {
    const ms = (s) => (s * 1000) / this.rate;
    return {
      ms: ms(this.n),
      under: this.under,
      avgMs: this.blocks ? ms(this.sumFill / this.blocks) : 0,
      minMs: this.minFill < 0 ? 0 : ms(this.minFill),
      corrMs: ms(this.corrSamples),
      trimmedMs: ms(this.trimmed),
    };
  }
}

if (typeof AudioWorkletProcessor !== "undefined" && typeof registerProcessor === "function") {
  class RpAudio extends AudioWorkletProcessor {
    constructor() {
      super();
      this.ring = new RpRing(48000, sampleRate);
      this.tick = 0;
      this.feed = null;
      this.port.onmessage = (e) => this.take(e.data);
    }
    /** From the page (target, reset, frames), or from the decoding worker over `feed`. */
    take(d) {
      if (d.target) return this.ring.setTarget(d.target);
      if (d.reset) return this.ring.reset();
      if (d.feed) {
        if (this.feed) this.feed.close();
        this.feed = d.feed;
        this.feed.onmessage = (e) => this.take(e.data);
        return;
      }
      if (d.close) {
        if (this.feed) this.feed.close();
        this.feed = null;
        return;
      }
      if (d.planes) this.ring.push(d.planes, d.scale || 1);
    }
    process(_inputs, outputs) {
      const out = outputs[0];
      if (out && out.length) this.ring.pull(out, out[0].length);
      if (++this.tick % 24 === 0) this.port.postMessage(this.ring.stats());
      return true;
    }
  }
  registerProcessor("rp-audio", RpAudio);
}
