// Remote Play latency, measured stage by stage rather than guessed. Each video frame is timed:
//   net    — the engine had it whole from the console → this page received it (the engine's
//            clock, lined up with the page's from pings; an engine without timed video has none)
//   queue  — received → handed to the decoder (waiting for the decoder to be set up, say)
//   decode — handed to the decoder → the decoder gave the picture
//   draw   — the decoder gave it → the next animation frame (when the browser shows it)
// The console's own leg to the engine has no clock both ends share, so it is not in here.
// What a press takes to show on screen is estimated from these and two round trips; see
// inputEstimate.

export const STAGES = ["net", "queue", "decode", "draw"] as const;
export type Stage = (typeof STAGES)[number];

/** A stage's median and 95th percentile over the window, ms. */
export interface StageTimes {
  p50: number;
  p95: number;
}

export type StageSummary = Record<Stage, StageTimes | null>;

/** Samples kept per stage: 2 s at 120 fps. */
const RING = 256;
/** How far back a summary looks. */
export const STAGE_WINDOW_MS = 2000;

/** The value at fraction `p` (0-1) of an ascending list, nearest rank. */
export function percentile(sorted: ArrayLike<number>, p: number): number {
  const n = sorted.length;
  if (n === 0) return NaN;
  const i = Math.min(n - 1, Math.max(0, Math.ceil(p * n) - 1));
  return sorted[i];
}

/** Rolling per-stage samples in fixed rings: adding one allocates nothing. */
export class LatencyStages {
  private vals = STAGES.map(() => new Float64Array(RING));
  private at = STAGES.map(() => new Float64Array(RING).fill(-Infinity));
  private next = STAGES.map(() => 0);
  private scratch = new Float64Array(RING);

  add(stage: Stage, ms: number, now: number): void {
    if (!Number.isFinite(ms)) return;
    const s = STAGES.indexOf(stage);
    const i = this.next[s];
    this.vals[s][i] = Math.max(0, ms);
    this.at[s][i] = now;
    this.next[s] = (i + 1) % RING;
  }

  clear(): void {
    for (const a of this.at) a.fill(-Infinity);
  }

  /** Each stage's median and 95th percentile over the last `windowMs`; null for a stage with no
   *  samples in it. Once a second, so the sort here is off the per-frame path. */
  summary(now: number, windowMs = STAGE_WINDOW_MS): StageSummary {
    const out = {} as StageSummary;
    STAGES.forEach((stage, s) => {
      let n = 0;
      const vals = this.vals[s];
      const at = this.at[s];
      for (let i = 0; i < RING; i++) {
        if (now - at[i] <= windowMs) this.scratch[n++] = vals[i];
      }
      if (n === 0) {
        out[stage] = null;
        return;
      }
      const sorted = this.scratch.subarray(0, n).sort();
      out[stage] = { p50: percentile(sorted, 0.5), p95: percentile(sorted, 0.95) };
    });
    return out;
  }
}

/** The engine's clock against this page's, from pings: `{"ping": sentAt}` answered with
 *  `{"pong": sentAt, "engine_us": e}`. The engine read its clock somewhere between the ping
 *  leaving and the pong arriving; the sample with the shortest round trip pins that best. */
export class ClockSync {
  private samples: { rtt: number; offset: number }[] = [];

  /** One pong; ignored when it carries no engine time (an older engine). */
  push(sentAt: unknown, engineUs: unknown, now: number): void {
    if (typeof sentAt !== "number" || typeof engineUs !== "number" || !Number.isFinite(sentAt) || !Number.isFinite(engineUs)) return;
    const rtt = now - sentAt;
    if (rtt < 0) return;
    this.samples.push({ rtt, offset: engineUs / 1000 - (sentAt + now) / 2 });
    if (this.samples.length > 8) this.samples.shift();
  }

  /** Engine ms minus page ms at the same instant, or null before the first timed pong. */
  get offsetMs(): number | null {
    let best: { rtt: number; offset: number } | null = null;
    for (const s of this.samples) if (!best || s.rtt < best.rtt) best = s;
    return best ? best.offset : null;
  }

  reset(): void {
    this.samples = [];
  }
}

/** The stage numbers for the stats line, whole ms (medians); "?" for a stage not measured. */
export function stageValues(s: StageSummary | null | undefined): Record<Stage, string> {
  const v = (t: StageTimes | null | undefined) => (t ? String(Math.round(t.p50)) : "?");
  return { net: v(s?.net), queue: v(s?.queue), decode: v(s?.decode), draw: v(s?.draw) };
}

/** One plain line for the logs: "net 6/9 · queue 0/1 · decode 4/7 · draw 2/8 ms (median/95th)". */
export function stagesLogText(s: StageSummary | null | undefined): string {
  if (!s || STAGES.every((k) => !s[k])) return "no frames timed yet";
  const part = (k: Stage) => {
    const t = s[k];
    return `${k} ${t ? `${Math.round(t.p50)}/${Math.round(t.p95)}` : "?"}`;
  };
  return `${STAGES.map(part).join(" · ")} ms (median/95th)`;
}

/** Press to picture, ms, from what can be measured:
 *   - the pad state from this page to the engine: half the page's round trip (pings);
 *   - the engine to the PS5 and back: the engine's Takion round trip (its messages out, the
 *     console's acknowledgements in). The press goes one way, the picture comes back the other;
 *   - the picture from the engine to the screen: the net, queue, decode and screen stages
 *     (medians).
 *  Not in it, because nothing here can see it: the controller's own delay, and the time the
 *  game and the PS5 take to react and encode the picture (often a frame or more). So the real
 *  figure is this plus that. Null until the round trips and the net and decode stages are known. */
export function inputEstimate(o: {
  pageRttMs: number | null | undefined;
  consoleRttMs: number | null | undefined;
  stages: StageSummary | null | undefined;
}): number | null {
  const { pageRttMs, consoleRttMs, stages } = o;
  const ok = (v: number | null | undefined): v is number => typeof v === "number" && Number.isFinite(v) && v >= 0;
  if (!ok(pageRttMs) || !ok(consoleRttMs) || !stages?.net || !stages.decode) return null;
  const p50 = (k: Stage) => stages[k]?.p50 ?? 0;
  return pageRttMs / 2 + consoleRttMs + p50("net") + p50("queue") + p50("decode") + p50("draw");
}
