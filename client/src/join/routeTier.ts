// How good a guest's connection is, in four tiers (apps/play/docs/ROUTES.md). The same lines as
// the engine's (join_route.rs LIMITS) and PS5Upload Play's (apps/play/src/net/route.cpp): the
// tests of all three pin the same table, so the page, the app and the host's panel say the same.

export type Tier = "excellent" | "good" | "playable" | "poor";

export interface Limits {
  tier: Tier;
  rttMs: number;
  jitterMs: number;
  /** Lost, per thousand. */
  lossPermille: number;
  /** What the way carries over what the stream needs, percent. */
  headroomPct: number;
}

/** Excellent, Good, Playable; past Playable's lines is Poor. */
export const LIMITS: readonly Limits[] = [
  { tier: "excellent", rttMs: 20, jitterMs: 5, lossPermille: 1, headroomPct: 150 },
  { tier: "good", rttMs: 40, jitterMs: 15, lossPermille: 5, headroomPct: 125 },
  { tier: "playable", rttMs: 80, jitterMs: 30, lossPermille: 10, headroomPct: 100 },
];

/** One reading; a part not known (null) never lowers the tier. */
export interface Reading {
  rttMs: number | null;
  jitterMs?: number | null;
  lossPermille?: number | null;
  headroomPct?: number | null;
  /** Times the guest fell behind lately: once caps it at Playable, three times at Poor. */
  behind?: number;
}

const within = (r: Reading, l: Limits) =>
  (r.rttMs == null || r.rttMs <= l.rttMs) &&
  (r.jitterMs == null || r.jitterMs <= l.jitterMs) &&
  (r.lossPermille == null || r.lossPermille <= l.lossPermille) &&
  (r.headroomPct == null || r.headroomPct >= l.headroomPct);

const ORDER: Tier[] = ["poor", "playable", "good", "excellent"];

/** The tier of a reading; null when nothing is known yet. */
export function classify(r: Reading): Tier | null {
  if (r.rttMs == null && r.jitterMs == null && r.lossPermille == null && r.headroomPct == null) return null;
  let t: Tier = LIMITS.find((l) => within(r, l))?.tier ?? "poor";
  const cap: Tier | null = (r.behind ?? 0) >= 3 ? "poor" : (r.behind ?? 0) >= 1 ? "playable" : null;
  if (cap && ORDER.indexOf(cap) < ORDER.indexOf(t)) t = cap;
  return t;
}

/** A tier word from the engine (anything else: null). */
export function readTier(v: unknown): Tier | null {
  return v === "excellent" || v === "good" || v === "playable" || v === "poor" ? v : null;
}

/** The round trips the page measures (its pings), smoothed as the app's meter does: RFC 6298's
 *  1/8 for the round trip, 1/4 for how much it moves between two. */
export class RttMeter {
  private srtt: number | null = null;
  private jit = 0;
  private last: number | null = null;
  private n = 0;

  add(ms: number): void {
    if (!Number.isFinite(ms) || ms < 0) return;
    if (this.srtt == null) this.srtt = ms;
    else {
      this.srtt += (ms - this.srtt) / 8;
      this.jit += (Math.abs(ms - (this.last ?? ms)) - this.jit) / 4;
    }
    this.last = ms;
    this.n++;
  }

  reading(): Reading {
    return {
      rttMs: this.srtt == null ? null : Math.round(this.srtt),
      jitterMs: this.n > 1 ? Math.round(this.jit) : null,
    };
  }
}

type Tr = (key: string, vars: undefined, fallback: string) => string;

/** The tier in words. */
export function tierLabel(tr: Tr, t: Tier): string {
  switch (t) {
    case "excellent":
      return tr("join_tier_excellent", undefined, "Excellent");
    case "good":
      return tr("join_tier_good", undefined, "Good");
    case "playable":
      return tr("join_tier_playable", undefined, "Playable");
    case "poor":
      return tr("join_tier_poor", undefined, "Poor");
  }
}

/** Bars for a tier (design 6.1): Excellent 4, Good 3, Playable 2, Poor 1, nothing known 0. */
export function barsOf(t: Tier | null): number {
  switch (t) {
    case "excellent":
      return 4;
    case "good":
      return 3;
    case "playable":
      return 2;
    case "poor":
      return 1;
    default:
      return 0;
  }
}

/** A round trip as shown everywhere: whole ms, "<1 ms" below one (null: nothing). */
export function msText(ms: number | null | undefined): string | null {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return null;
  return ms < 1 ? "<1 ms" : `${Math.round(ms)} ms`;
}

/** The tier a round trip alone gives (cards, estimates). */
export function tierOfMs(ms: number | null | undefined): Tier | null {
  return ms == null ? null : classify({ rttMs: ms });
}

/** A tier as one of three colours (the dot or text beside the ms): green, amber, red; "none"
 *  with no reading. Replaces the old 60/120 ms delay colours. */
export function toneOfTier(t: Tier | null): "good" | "fair" | "slow" | "none" {
  if (t === "excellent" || t === "good") return "good";
  if (t === "playable") return "fair";
  if (t === "poor") return "slow";
  return "none";
}

/** The "connection is slow" callout fires at Poor, or Playable with a fall-behind (design 6.1;
 *  it was a flat 120 ms before rooms). */
export function slowEnough(t: Tier | null, behind: boolean): boolean {
  return t === "poor" || (t === "playable" && behind);
}
