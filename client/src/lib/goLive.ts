// Go live (#635): the Remote Play stream straight to YouTube, Twitch, Kick, Facebook or a
// custom RTMP(S)/SRT server, run by the engine (engine rp_golive.rs), and the local feed for
// OBS (rp_live.rs). This file is the API and the pure logic the panel uses; the panel is
// screens/Play/GoLivePanel.tsx.

import { getEngineUrl } from "../state/engine";
import { ENGINE_LONG_TIMEOUT_MS, engineFetch, type EngineFetchInit } from "./engineFetch";

export type PresetId = "youtube" | "twitch" | "kick" | "facebook" | "custom";

/** A limit check of the session against a site (engine golive_dest.rs `Check`). */
export interface LiveCheck {
  code: "codec" | "hdr" | "bitrate" | "height" | "fps";
  blocking: boolean;
  have: number | string;
  limit: number | string | string[];
  message: string;
}

export interface LivePreset {
  id: PresetId;
  name: string;
  server: string | null;
  needs_server: boolean;
  codecs: string[];
  max_kbps: number;
  max_height: number;
  max_fps: number;
  key_page: string;
  bandwidth_test: boolean;
  checks: LiveCheck[];
}

export interface LiveDestination {
  id: string;
  preset: PresetId;
  label: string;
  server: string;
  bandwidth_test: boolean;
  enabled: boolean;
  /** "…abcd": the key's last four characters (the key itself never leaves the engine). */
  key_hint: string;
  /** The saved key can't be read any more (the key file changed): the site must be added again. */
  key_lost?: boolean;
  name: string;
  checks?: LiveCheck[];
}

export interface LiveSession {
  host: string;
  codec: "h264" | "h265" | "h265_hdr";
  width: number;
  height: number;
  fps: number;
  bitrate_kbps: number;
}

export interface LiveProblem {
  code: string;
  message: string;
}

/** `stopping`: Stop was pressed and ffmpeg is finishing (a few seconds at most). */
export type LivePhase = "starting" | "live" | "reconnecting" | "waiting_for_play" | "stopping" | "ended" | "failed";

export interface LiveDestStatus {
  id: string;
  label: string;
  preset: PresetId;
  name: string;
  state: "connecting" | "live" | "retrying" | "failed";
  error: LiveProblem | null;
  bandwidth_test: boolean;
}

export interface LiveRun {
  phase: LivePhase;
  error: LiveProblem | null;
  started_s: number;
  live_s: number | null;
  reconnects: number;
  retry_in_s: number | null;
  kbps: number;
  fps: number;
  speed: number | null;
  frames: number;
  dropped_frames: number;
  upload_skips: number;
  slow_upload: boolean;
  audio_kbps: number;
  destinations: LiveDestStatus[];
  session: LiveSession | null;
}

export interface FfmpegStatus {
  installed: boolean;
  version: string;
  supported: boolean;
  /** There is a download for this system's OS and CPU, but it can't run here (a Docker image):
   *  the engine needs its own ffmpeg (`PS5UPLOAD_FFMPEG`). */
  needs_own?: boolean;
  download_bytes: number | null;
  source: string | null;
  install: {
    running: boolean;
    step: "downloading" | "verifying" | "unpacking" | null;
    done_bytes: number;
    total_bytes: number;
    error: string | null;
    message: string | null;
  };
}

export interface StreamStatus {
  ffmpeg: FfmpegStatus;
  session: LiveSession | null;
  presets: LivePreset[];
  destinations: LiveDestination[];
  store_error: string | null;
  live: LiveRun | null;
}

/** An engine refusal: its stable code and its sentence. */
export class LiveApiError extends Error {
  constructor(
    public code: string,
    message: string,
    public body: Record<string, unknown> = {},
  ) {
    super(message);
  }
}

async function call<T>(path: string, init?: EngineFetchInit): Promise<T> {
  const r = await engineFetch(`${getEngineUrl()}${path}`, {
    ...init,
    headers: init?.body ? { "content-type": "application/json" } : undefined,
  });
  const text = await r.text();
  let body: Record<string, unknown>;
  try {
    body = text ? (JSON.parse(text) as Record<string, unknown>) : {};
  } catch {
    body = {};
  }
  if (!r.ok) {
    throw new LiveApiError(
      typeof body.error === "string" ? body.error : `http_${r.status}`,
      typeof body.message === "string" ? body.message : `HTTP ${r.status}`,
      body,
    );
  }
  return body as T;
}

export const goLiveApi = {
  status: () => call<StreamStatus>("/api/stream/status"),
  start: (ids: string[], audioKbps = 160) =>
    call<{ ok: boolean; live: LiveRun }>("/api/stream/start", {
      method: "POST",
      body: JSON.stringify({ destinations: ids.map((key_id) => ({ key_id })), audio_kbps: audioKbps }),
    }),
  stop: () => call<{ ok: boolean }>("/api/stream/stop", { method: "POST" }),
  add: (d: DestinationDraft) =>
    call<LiveDestination>("/api/stream/destinations", { method: "POST", body: JSON.stringify(draftBody(d)) }),
  update: (id: string, patch: Partial<DestinationDraft> & { enabled?: boolean }) =>
    call<LiveDestination>(`/api/stream/destinations/${encodeURIComponent(id)}`, {
      method: "POST",
      body: JSON.stringify(patch),
    }),
  remove: (id: string) =>
    call<{ ok: boolean }>(`/api/stream/destinations/${encodeURIComponent(id)}`, { method: "DELETE" }),
  installFfmpeg: () => call<{ ok: boolean; ffmpeg: FfmpegStatus }>("/api/stream/tools/ffmpeg/install", { timeoutMs: ENGINE_LONG_TIMEOUT_MS, method: "POST" }),
};

/** What the Add form holds. */
export interface DestinationDraft {
  preset: PresetId;
  key: string;
  /** Kick: the server URL; Custom: the full URL. */
  server: string;
  label: string;
  bandwidth_test: boolean;
}

export function emptyDraft(preset: PresetId): DestinationDraft {
  return { preset, key: "", server: "", label: "", bandwidth_test: preset === "twitch" };
}

function draftBody(d: DestinationDraft): Record<string, unknown> {
  const b: Record<string, unknown> = { preset: d.preset, key: d.key.trim() };
  if (d.label.trim()) b.label = d.label.trim();
  if (d.preset === "custom") b.url = d.server.trim();
  else if (d.preset === "kick") b.server = d.server.trim();
  if (d.preset === "twitch") b.bandwidth_test = d.bandwidth_test;
  return b;
}

/** Why the Add form can't be saved yet (same rules as the engine's), or null. */
export type DraftProblem = "key_missing" | "key_invalid" | "server_missing" | "url_invalid";

const BAD_CHARS = /[\s|[\]\\'"]/;

export function draftProblem(d: DestinationDraft): DraftProblem | null {
  const key = d.key.trim();
  const server = d.server.trim();
  if (d.preset === "custom") {
    if (!/^(rtmps?|srt):\/\/[^/?\s]+/i.test(server) || BAD_CHARS.test(server)) return "url_invalid";
    if (key && (key.length < 4 || BAD_CHARS.test(key) || key.includes("://"))) return "key_invalid";
    return null;
  }
  if (d.preset === "kick") {
    if (!server) return "server_missing";
    if (!/^rtmps:\/\/[^/?\s]+/i.test(server) || BAD_CHARS.test(server)) return "url_invalid";
  }
  if (key.length < 4) return "key_missing";
  if (BAD_CHARS.test(key) || key.includes("://") || key.length > 512) return "key_invalid";
  return null;
}

/** h:mm:ss (or m:ss under an hour) for the LIVE chip. */
export function formatElapsed(s: number): string {
  const t = Math.max(0, Math.floor(s));
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const sec = String(t % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}

/** "6.1 Mbps" / "850 kbps". */
export function formatKbps(kbps: number): string {
  if (!Number.isFinite(kbps) || kbps <= 0) return "0 kbps";
  return kbps >= 1000 ? `${(kbps / 1000).toFixed(1)} Mbps` : `${Math.round(kbps)} kbps`;
}

/** "6.1 Mbps · 60 fps" (units as the streaming sites write them). */
export function formatRate(kbps: number, fps: number): string {
  return `${formatKbps(kbps)} · ${Math.round(fps)} fps`;
}

/** "1080p 60 fps". */
export function formatMode(height: number, fps: number): string {
  return `${height}p ${fps} fps`;
}

/** A run that still goes (or tries to). */
export function isLive(run: LiveRun | null | undefined): boolean {
  return !!run && run.phase !== "ended" && run.phase !== "failed";
}

/** What Play would have to restart with so every chosen site takes the stream: H.264 when
 *  one refuses HEVC/HDR, and the lowest bitrate limit among them when the session is above it.
 *  Null when nothing needs changing. */
export function playFix(
  session: LiveSession | null,
  chosen: Array<{ checks?: LiveCheck[] }>,
): { codec: "h264" | null; bitrateKbps: number | null } | null {
  if (!session) return null;
  let codec: "h264" | null = null;
  let bitrate: number | null = null;
  for (const d of chosen) {
    for (const c of d.checks ?? []) {
      if (c.code === "codec" || c.code === "hdr") codec = "h264";
      if (c.code === "bitrate" && typeof c.limit === "number") {
        bitrate = bitrate === null ? c.limit : Math.min(bitrate, c.limit);
      }
    }
  }
  if (!codec && bitrate === null) return null;
  return { codec, bitrateKbps: bitrate };
}

/** Roughly how much upload Go live needs: the video's bitrate, the sound, plus a margin. */
export function uploadNeededKbps(session: LiveSession | null, destinations: number, audioKbps = 160): number {
  if (!session) return 0;
  return Math.round((session.bitrate_kbps + audioKbps) * 1.2 * Math.max(1, destinations));
}

/** Where OBS on this computer reads the stream: the engine's live feed. */
export function liveFeedUrl(engineUrl: string): string {
  return `${engineUrl.replace(/\/+$/, "")}/api/ps5/rp/live.ts`;
}

/** `GET /api/ps5/rp/live/status` (engine rp_live.rs). */
export interface LiveFeedStatus {
  running: boolean;
  consumers: number;
  codec?: string;
  width?: number | null;
  height?: number | null;
  fps?: number | null;
}
