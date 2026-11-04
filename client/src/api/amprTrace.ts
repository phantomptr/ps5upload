// Measuring how a game reads its files, for the LZ4 asset packs' profile (engine
// `ampr_trace`): the debug ampr_emu goes into the game's fakelib2, sessions are collected
// after play, and the profile Convert uses is generated from them.

import { consoleAddr } from "../lib/addr";
import { getEngineUrl } from "../state/engine";
import { engineFetch } from "../lib/engineFetch";

export interface TraceState {
  tracing: boolean;
  host: string | null;
  game_path: string | null;
  started_at: number | null;
}

export interface TraceSession {
  id: string;
  collected_at: number;
  journal_bytes: number;
}

export interface TraceProfile {
  generated_at: number;
  sessions: number;
  summary: {
    traces?: number;
    reads?: number;
    files_observed?: number;
    files_packed?: number;
    decoded_cache_bytes?: number;
    physical_cache_bytes?: number;
    runtime_workers?: number;
    warnings?: string[];
  } | null;
}

export interface TraceStatus {
  title_id: string;
  state: TraceState;
  sessions: TraceSession[];
  profile: TraceProfile | null;
  /** The debug emulator build the engine places. */
  runtime: string;
  /** Set by collect. */
  collected?: { session: string; journal_bytes: number };
  /** Set by stop: whether a fakelib2 of ours was removed. */
  removed?: boolean;
}

async function call<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await engineFetch(`${getEngineUrl()}${path}`, {
    headers: { "content-type": "application/json" },
    ...init,
  });
  const body = (await res.json().catch(() => ({}))) as T & { error?: string };
  if (!res.ok) throw new Error(body.error || `${path} failed (${res.status})`);
  return body;
}

const post = (path: string, body: unknown) =>
  call<TraceStatus>(path, { method: "POST", body: JSON.stringify(body) });

export const amprTrace = {
  status: (titleId: string) =>
    call<TraceStatus>(`/api/ampr/trace?title_id=${encodeURIComponent(titleId)}`),
  start: (host: string, gamePath: string, titleId: string) =>
    post("/api/ampr/trace/start", { addr: consoleAddr(host), game_path: gamePath, title_id: titleId }),
  stop: (host: string, gamePath: string, titleId: string) =>
    post("/api/ampr/trace/stop", { addr: consoleAddr(host), game_path: gamePath, title_id: titleId }),
  collect: (host: string, gamePath: string, titleId: string) =>
    post("/api/ampr/trace/collect", { addr: consoleAddr(host), game_path: gamePath, title_id: titleId }),
  generate: (titleId: string) => post("/api/ampr/trace/generate", { title_id: titleId }),
  /** The measured profile's text, or null when there is none. */
  profile: async (titleId: string): Promise<{ toml: string; profile: TraceProfile } | null> => {
    const r = await call<{ exists: boolean; toml: string | null; profile: TraceProfile | null }>(
      `/api/ampr/profile?title_id=${encodeURIComponent(titleId)}`,
    );
    return r.exists && r.toml && r.profile ? { toml: r.toml, profile: r.profile } : null;
  },
  forget: (titleId: string) =>
    call<TraceStatus>(`/api/ampr/profile?title_id=${encodeURIComponent(titleId)}`, { method: "DELETE" }),
};

/** A PS5/PS4 title id out of a content id (`UP9000-PPSA03671_00-X` → `PPSA03671`). */
export function titleIdOf(contentId: string | null | undefined): string | null {
  const m = /([A-Z]{4}\d{5})/.exec(contentId ?? "");
  return m ? m[1] : null;
}
