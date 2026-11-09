// The running game's sampled frame rate (#774, engine /api/ps5/hw/fps). The engine reads the
// console's display frame count once a second while someone looks or a capture runs; the rate is
// frames over each second, not single-frame times. A plain fetch against the engine origin, so the
// desktop app, the phone app and the web UI take the same path.

import { consoleAddr } from "../lib/addr";
import { getEngineUrl } from "../state/engine";

/** ok: a game is counted. starting: no rate yet. no_game: nothing to count. unsupported: an older
 *  helper. offline: no answer. no_device / failed: the console gave no count. */
export type FpsState = "ok" | "starting" | "no_game" | "unsupported" | "offline" | "no_device" | "failed";

export interface FpsStats {
  samples: number;
  seconds: number;
  min_fps: number;
  max_fps: number;
  /** All frames over all the time sampled. */
  avg_fps: number;
  /** The average of the slowest 1% of one-second samples. */
  low_1pct_fps: number;
}

export interface FpsCaptureInfo {
  active: boolean;
  started_ms?: number;
  stopped_ms?: number | null;
  /** "stopped" or "limit" (six hours). */
  stop_reason?: string | null;
  samples: number;
  stats?: FpsStats;
}

export interface FpsLive {
  state: FpsState;
  fps: number | null;
  frames?: number;
  interval_ms?: number;
  at_ms?: number;
  error?: string;
  capture: FpsCaptureInfo;
}

export interface FpsSample {
  t_ms: number;
  elapsed_ms: number;
  fps: number;
  frames: number;
  interval_ms: number;
  cpu_temp_c?: number;
  soc_temp_c?: number;
}

export interface FpsCapture {
  active: boolean;
  started_ms: number;
  stopped_ms: number | null;
  stop_reason: string | null;
  stats: FpsStats;
  total: number;
  since: number;
  samples: FpsSample[];
}

const STATES: FpsState[] = ["ok", "starting", "no_game", "unsupported", "offline", "no_device", "failed"];

/** Reads a live reply, however odd: an unknown state is "failed", a missing rate is none. */
export function parseFpsLive(body: unknown): FpsLive {
  const b = (body ?? {}) as Record<string, unknown>;
  const state = STATES.includes(b.state as FpsState) ? (b.state as FpsState) : "failed";
  const cap = (b.capture ?? {}) as Record<string, unknown>;
  return {
    state,
    fps: typeof b.fps === "number" && Number.isFinite(b.fps) ? b.fps : null,
    frames: typeof b.frames === "number" ? b.frames : undefined,
    interval_ms: typeof b.interval_ms === "number" ? b.interval_ms : undefined,
    at_ms: typeof b.at_ms === "number" ? b.at_ms : undefined,
    error: typeof b.error === "string" ? b.error : undefined,
    capture: {
      active: cap.active === true,
      started_ms: typeof cap.started_ms === "number" ? cap.started_ms : undefined,
      stopped_ms: typeof cap.stopped_ms === "number" ? cap.stopped_ms : null,
      stop_reason: typeof cap.stop_reason === "string" ? cap.stop_reason : null,
      samples: typeof cap.samples === "number" ? cap.samples : 0,
      stats: (cap.stats as FpsStats | undefined) ?? undefined,
    },
  };
}

export class FpsError extends Error {
  readonly status: number;
  constructor(message: string, status: number) {
    super(message);
    this.name = "FpsError";
    this.status = status;
  }
}

async function reply(res: Response): Promise<unknown> {
  const body = (await res.json().catch(() => ({}))) as Record<string, unknown>;
  if (!res.ok) {
    throw new FpsError(
      typeof body.error === "string" && body.error ? body.error : `request failed (${res.status})`,
      res.status,
    );
  }
  return body;
}

function q(host: string): string {
  return `addr=${encodeURIComponent(consoleAddr(host))}`;
}

/** The latest sample. Keeps the engine sampling for 15 s, so poll it while the readout shows. */
export async function fetchFpsLive(host: string): Promise<FpsLive> {
  const res = await fetch(`${getEngineUrl()}/api/ps5/hw/fps?${q(host)}`, {
    signal: AbortSignal.timeout(8_000),
  });
  return parseFpsLive(await reply(res));
}

async function post(path: string, host: string): Promise<FpsLive> {
  const res = await fetch(`${getEngineUrl()}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ addr: consoleAddr(host) }),
    signal: AbortSignal.timeout(8_000),
  });
  return parseFpsLive(await reply(res));
}

export const startFpsCapture = (host: string) => post("/api/ps5/hw/fps/capture/start", host);
export const stopFpsCapture = (host: string) => post("/api/ps5/hw/fps/capture/stop", host);

/** The capture's rows from `since` on, or null when the console has none. */
export async function fetchFpsCapture(host: string, since = 0): Promise<FpsCapture | null> {
  const res = await fetch(`${getEngineUrl()}/api/ps5/hw/fps/capture?${q(host)}&since=${since}`, {
    signal: AbortSignal.timeout(8_000),
  });
  if (res.status === 404) return null;
  const b = (await reply(res)) as FpsCapture;
  return { ...b, samples: Array.isArray(b.samples) ? b.samples : [] };
}

/** The capture as CSV text (the engine writes it). */
export async function fetchFpsCaptureCsv(host: string): Promise<string> {
  const res = await fetch(`${getEngineUrl()}/api/ps5/hw/fps/capture/csv?${q(host)}`, {
    signal: AbortSignal.timeout(20_000),
  });
  if (!res.ok) await reply(res);
  return res.text();
}
