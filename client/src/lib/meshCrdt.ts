/**
 * The shared state every device keeps the same through the consoles (8.0.1): a last-writer-wins
 * map (a state-based CRDT) stamped with hybrid logical clocks.
 *
 * Each key holds one register: a value and the stamp of the change that set it. A removal is a
 * register too (value null, a "tombstone"), so a removal travels like any change and a stale copy
 * on another console cannot bring the value back. Merging two maps keeps, per key, the register
 * with the higher stamp. That merge is commutative, associative and idempotent, so every device
 * and every console's copy ends up the same whatever order the copies meet in, however often,
 * and through however many consoles.
 *
 * A stamp is (wall-clock ms, counter, device id). The clock never goes backwards on a device and
 * moves past every stamp it has seen (a hybrid logical clock), so a device's own edits stay in
 * order even when its clock is behind another's; the device id breaks exact ties the same way on
 * every device.
 */

export interface Hlc {
  /** Wall-clock milliseconds (or later, when a later stamp was seen). */
  t: number;
  /** Orders changes within one millisecond. */
  c: number;
  /** The device that made the change: breaks exact ties identically everywhere. */
  d: string;
}

export interface Reg {
  /** The value; null when the key was removed. */
  v: unknown;
  h: Hlc;
}

export type LwwMap = Record<string, Reg>;

/** -1, 0 or 1. */
export function cmpHlc(a: Hlc, b: Hlc): number {
  if (a.t !== b.t) return a.t < b.t ? -1 : 1;
  if (a.c !== b.c) return a.c < b.c ? -1 : 1;
  if (a.d !== b.d) return a.d < b.d ? -1 : 1;
  return 0;
}

export function isHlc(x: unknown): x is Hlc {
  const h = x as Partial<Hlc> | null;
  return (
    !!h &&
    typeof h.t === "number" &&
    Number.isFinite(h.t) &&
    typeof h.c === "number" &&
    Number.isFinite(h.c) &&
    typeof h.d === "string"
  );
}

/** A map read from elsewhere, checked: registers that are not well formed are dropped. */
export function asMap(raw: unknown): LwwMap {
  const out: LwwMap = {};
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return out;
  for (const [k, r] of Object.entries(raw as Record<string, unknown>)) {
    const reg = r as Partial<Reg> | null;
    if (!reg || typeof reg !== "object" || !("v" in reg) || !isHlc(reg.h)) continue;
    out[k] = { v: reg.v ?? null, h: { t: reg.h.t, c: reg.h.c, d: reg.h.d } };
  }
  return out;
}

/** Per key, the register with the higher stamp. */
export function merge(a: LwwMap, b: LwwMap): LwwMap {
  const out: LwwMap = { ...a };
  for (const [k, r] of Object.entries(b)) {
    const cur = out[k];
    if (!cur || cmpHlc(r.h, cur.h) > 0) out[k] = r;
  }
  return out;
}

/** Same registers, same stamps. */
export function sameMap(a: LwwMap, b: LwwMap): boolean {
  const ka = Object.keys(a);
  if (ka.length !== Object.keys(b).length) return false;
  for (const k of ka) {
    const x = a[k];
    const y = b[k];
    if (!y || cmpHlc(x.h, y.h) !== 0 || JSON.stringify(x.v) !== JSON.stringify(y.v)) return false;
  }
  return true;
}

/** The highest stamp in a map (the clock moves past it). */
export function maxHlc(m: LwwMap): Hlc | null {
  let best: Hlc | null = null;
  for (const r of Object.values(m)) if (!best || cmpHlc(r.h, best) > 0) best = r.h;
  return best;
}

/** A hybrid logical clock. `wall` is injectable for tests. */
export class Clock {
  constructor(
    public device: string,
    public last: { t: number; c: number } = { t: 0, c: 0 },
    private readonly wall: () => number = () => Date.now(),
  ) {}

  /** A stamp later than every stamp this clock made or saw. */
  now(): Hlc {
    const w = this.wall();
    if (w > this.last.t) this.last = { t: w, c: 0 };
    else this.last = { t: this.last.t, c: this.last.c + 1 };
    return { t: this.last.t, c: this.last.c, d: this.device };
  }

  /** Move past a stamp seen elsewhere. */
  observe(h: Hlc | null): void {
    if (!h) return;
    if (h.t > this.last.t || (h.t === this.last.t && h.c > this.last.c)) this.last = { t: h.t, c: h.c };
  }
}

/** Removals older than this are dropped from a map (a device away for longer could bring the
 *  value back; a year is far past any realistic absence). */
export const TOMBSTONE_TTL_MS = 365 * 86_400_000;

export function pruneTombstones(m: LwwMap, now: number): LwwMap {
  const out: LwwMap = {};
  for (const [k, r] of Object.entries(m)) {
    if (r.v === null && now - r.h.t > TOMBSTONE_TTL_MS) continue;
    out[k] = r;
  }
  return out;
}
