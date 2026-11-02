// Types for rpAudioWorklet.js (plain JavaScript: the worklet file is served as it is).

export const MAX_SPEED_CHANGE: number;
export const ENTER_MS: number;
export const EXIT_MS: number;

export interface RpRingStats {
  ms: number;
  under: number;
  avgMs: number;
  minMs: number;
  corrMs: number;
  trimmedMs: number;
}

export class RpRing {
  constructor(cap: number, rate?: number);
  readonly n: number;
  readonly speed: number;
  readonly primed: boolean;
  setTarget(samples: number): void;
  reset(): void;
  push(planes: Float32Array[], scale: number): void;
  pull(out: Float32Array[], len: number): void;
  stats(): RpRingStats;
}
