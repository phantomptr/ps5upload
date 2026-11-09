// The devices that use a console and what they share through it (8.0.0, engine /api/mesh/*).
// An older helper answers {supported:false}: callers then behave as before 8.0.0.
// A plain fetch against the engine origin, so the desktop app and the web UI take the same path.

import { getEngineUrl } from "../state/engine";
import { consoleAddr } from "../lib/addr";

export interface MeshActivityItem {
  kind: string;
  title?: string;
  pct?: number | null;
  stage?: string;
  /** Docker only: how many browsers use that engine. */
  count?: number;
  /** Remote Play: where a phone joins this device's session as a controller (pad_link.rs). */
  pad?: string;
  /** Remote Play with Play Together open (#641): friends nearby may join (they are asked). */
  join?: MeshJoin;
}

/** A Play Together host as the other apps on the network see it. */
export interface MeshJoin {
  code: string;
  players: number;
  max: number;
  host_name: string;
  console_name: string;
  game_title?: string | null;
  /** The guest socket: ws://<host computer>:<port>/api/join/ws */
  ws: string;
}

export interface MeshDevice {
  id: string;
  name: string;
  /** The name the user gave it; "" when none. */
  nick: string;
  /** "desktop" | "android" | "ios" | "docker"; "" for an app from before 8.0.0. */
  kind: string;
  os: string;
  /** App version; "" for an app from before 8.0.0. */
  app: string;
  first: number;
  last: number;
  ip: string;
  online: boolean;
  blocked: boolean;
  activity: MeshActivityItem[];
  /** How the console let it in (12.2 helpers): "network" (on the home network, approval off),
   *  "tailscale" (over Tailscale, approval off), "approved" (it asked and someone allowed it), or
   *  "" when not known (paired with a code, launched by its app, or listed by an older helper). */
  via?: string;
}

/** A device asking to use the console while it asks before new devices connect (12.2 helpers).
 *  Any device already let in, or the console's own page, answers Allow or Decline. */
export interface MeshAsk {
  /** Its key (hex): what Allow and Decline name. */
  id: string;
  name: string;
  ip: string;
  /** When it first asked (the console's clock, ms). */
  at: number;
}

export interface MeshLock {
  name: string;
  label: string;
  by: string;
  until: number;
}

export interface MeshEvent {
  seq: number;
  at: number;
  kind: string;
  by: string;
  text: string;
}

export interface MeshSettings {
  require_approval: boolean;
  prune_days: number;
}

export interface MeshState {
  rev: number;
  now: number;
  you: string;
  helper: string;
  settings: MeshSettings;
  /** Devices asking to connect now; absent from an older helper. */
  asks?: MeshAsk[];
  devices: MeshDevice[];
  locks: MeshLock[];
  events: MeshEvent[];
  ev: number;
}

export type MeshStateAnswer =
  | { supported: false; engineOld?: boolean }
  | { supported: true; same: true; rev: number }
  | ({ supported: true; same?: false } & MeshState);

export interface MeshMe {
  id: string | null;
  name: string;
  kind: string;
  os: string;
  app: string;
  /** Docker only: false when /data is not kept by the host (every update makes a new device). */
  docker_volume: boolean | null;
}

/** The engine this app talks to predates 8.0.0 (a stale dev build, or a remote engine such as an
 *  older Docker container): it has no mesh routes at all. */
export class EngineTooOldError extends Error {
  constructor() {
    super("The engine behind this app is older than 8.0.0");
    this.name = "EngineTooOldError";
  }
}

async function engineJson(route: string, init?: RequestInit): Promise<Record<string, unknown>> {
  const res = await fetch(`${getEngineUrl()}${route}`, {
    ...init,
    signal: AbortSignal.timeout(20_000),
  });
  if (res.status === 404) throw new EngineTooOldError();
  const body = (await res.json().catch(() => ({}))) as Record<string, unknown>;
  if (!res.ok) {
    throw new Error(typeof body.error === "string" && body.error ? body.error : `request failed (${res.status})`);
  }
  return body;
}

const post = (route: string, body: object) =>
  engineJson(route, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });

const str = (v: unknown) => (typeof v === "string" ? v : "");
const num = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? v : 0);

/** A friend's Play Together, kept only when it can be joined (a socket and a code). */
function toJoin(v: unknown): MeshJoin | undefined {
  if (!v || typeof v !== "object") return undefined;
  const j = v as Record<string, unknown>;
  if (typeof j.ws !== "string" || !j.ws || typeof j.code !== "string" || !j.code) return undefined;
  return {
    code: j.code,
    players: num(j.players),
    max: num(j.max),
    host_name: str(j.host_name),
    console_name: str(j.console_name),
    game_title: typeof j.game_title === "string" ? j.game_title : null,
    ws: j.ws,
  };
}

function toItems(v: unknown): MeshActivityItem[] {
  if (!Array.isArray(v)) return [];
  return v
    .filter((x): x is Record<string, unknown> => !!x && typeof x === "object")
    .map((x) => ({
      kind: str(x.kind),
      title: typeof x.title === "string" ? x.title : undefined,
      pct: typeof x.pct === "number" ? x.pct : null,
      stage: typeof x.stage === "string" ? x.stage : undefined,
      count: typeof x.count === "number" ? x.count : undefined,
      pad: typeof x.pad === "string" ? x.pad : undefined,
      join: toJoin(x.join),
    }))
    .filter((x) => x.kind);
}

/** Exported for tests: the engine's answer, checked field by field. */
export function toMeshState(b: Record<string, unknown>): MeshStateAnswer {
  if (b.supported !== true) return { supported: false };
  if (b.same === true) return { supported: true, same: true, rev: num(b.rev) };
  const settings = (b.settings ?? {}) as Record<string, unknown>;
  const devices = Array.isArray(b.devices) ? b.devices : [];
  return {
    supported: true,
    rev: num(b.rev),
    now: num(b.now),
    you: str(b.you),
    helper: str(b.helper),
    settings: {
      require_approval: settings.require_approval === true,
      prune_days: num(settings.prune_days),
    },
    devices: devices
      .filter((d): d is Record<string, unknown> => !!d && typeof d === "object" && typeof (d as { id?: unknown }).id === "string")
      .map((d) => ({
        id: str(d.id),
        name: str(d.name),
        nick: str(d.nick),
        kind: str(d.kind),
        os: str(d.os),
        app: str(d.app),
        first: num(d.first),
        last: num(d.last),
        ip: str(d.ip),
        online: d.online === true,
        blocked: d.blocked === true,
        activity: toItems(d.activity),
        via: typeof d.via === "string" ? d.via : undefined,
      })),
    asks: (Array.isArray(b.asks) ? b.asks : [])
      .filter((a): a is Record<string, unknown> => !!a && typeof a === "object" && typeof (a as { id?: unknown }).id === "string")
      .map((a) => ({ id: str(a.id), name: str(a.name), ip: str(a.ip), at: num(a.at) })),
    locks: (Array.isArray(b.locks) ? b.locks : [])
      .filter((l): l is Record<string, unknown> => !!l && typeof l === "object")
      .map((l) => ({ name: str(l.name), label: str(l.label), by: str(l.by), until: num(l.until) })),
    events: (Array.isArray(b.events) ? b.events : [])
      .filter((e): e is Record<string, unknown> => !!e && typeof e === "object")
      .map((e) => ({ seq: num(e.seq), at: num(e.at), kind: str(e.kind), by: str(e.by), text: str(e.text) })),
    ev: num(b.ev),
  };
}

export async function fetchMeshState(host: string, since: number, ev: number, client: string): Promise<MeshStateAnswer> {
  const q = new URLSearchParams({
    addr: consoleAddr(host),
    since: String(since),
    ev: String(ev),
    client,
  });
  try {
    return toMeshState(await engineJson(`/api/mesh/state?${q}`));
  } catch (e) {
    if (e instanceof EngineTooOldError) return { supported: false, engineOld: true };
    throw e;
  }
}

export async function postMeshActivity(host: string, client: string, items: MeshActivityItem[]): Promise<boolean> {
  const b = await post("/api/mesh/activity", { addr: consoleAddr(host), client, items });
  return b.supported === true;
}

export type LockAnswer =
  | { supported: false }
  | { supported: true; got: true; until: number }
  | { supported: true; got: false; by: string; byName: string; label: string; until: number };

export async function meshLock(
  host: string,
  op: "take" | "release",
  name: string,
  opts: { ttl?: number; force?: boolean; label?: string; owner?: string } = {},
): Promise<LockAnswer> {
  const b = await post("/api/mesh/lock", {
    addr: consoleAddr(host),
    op,
    name,
    ttl: opts.ttl ?? 60,
    force: !!opts.force,
    label: opts.label ?? "",
    owner: opts.owner ?? "",
  });
  if (b.supported !== true) return { supported: false };
  if (b.got === true) return { supported: true, got: true, until: num(b.until) };
  return { supported: true, got: false, by: str(b.by), byName: str(b.by_name), label: str(b.label), until: num(b.until) };
}

export async function meshDevice(
  host: string,
  op: "rename" | "remove" | "block" | "unblock" | "allow" | "decline",
  id: string,
  name?: string,
): Promise<boolean> {
  const b = await post("/api/mesh/device", { addr: consoleAddr(host), op, id, name });
  return b.supported === true;
}

export async function meshSettings(host: string, change: Partial<MeshSettings>): Promise<MeshSettings | null> {
  const b = await post("/api/mesh/settings", { addr: consoleAddr(host), ...change });
  if (b.supported !== true) return null;
  const s = (b.settings ?? {}) as Record<string, unknown>;
  return { require_approval: s.require_approval === true, prune_days: num(s.prune_days) };
}

export async function meshAnnounce(host: string, kind: string, text = ""): Promise<boolean> {
  const b = await post("/api/mesh/announce", { addr: consoleAddr(host), kind, text });
  return b.supported === true;
}

export type DocAnswer = { supported: false } | { supported: true; rev: number; doc: Record<string, unknown> | null };

export async function meshDocGet(host: string, name: string): Promise<DocAnswer> {
  const q = new URLSearchParams({ addr: consoleAddr(host), name });
  const b = await engineJson(`/api/mesh/doc?${q}`);
  if (b.supported !== true) return { supported: false };
  const doc = b.doc && typeof b.doc === "object" && !Array.isArray(b.doc) ? (b.doc as Record<string, unknown>) : null;
  return { supported: true, rev: num(b.rev), doc };
}

export type DocPutAnswer =
  | { supported: false }
  | { supported: true; saved: true; rev: number }
  | { supported: true; saved: false; rev: number; doc: Record<string, unknown> | null };

export async function meshDocPut(host: string, name: string, base: number, doc: Record<string, unknown>): Promise<DocPutAnswer> {
  const b = await post("/api/mesh/doc", { addr: consoleAddr(host), name, base, doc });
  if (b.supported !== true) return { supported: false };
  if (b.saved === true) return { supported: true, saved: true, rev: num(b.rev) };
  const cur = b.doc && typeof b.doc === "object" && !Array.isArray(b.doc) ? (b.doc as Record<string, unknown>) : null;
  return { supported: true, saved: false, rev: num(b.rev), doc: cur };
}

export async function fetchMeshMe(): Promise<MeshMe> {
  const b = await engineJson("/api/mesh/me");
  return {
    id: typeof b.id === "string" ? b.id : null,
    name: str(b.name),
    kind: str(b.kind),
    os: str(b.os),
    app: str(b.app),
    docker_volume: typeof b.docker_volume === "boolean" ? b.docker_volume : null,
  };
}

export async function setMeshName(name: string): Promise<MeshMe> {
  await post("/api/mesh/me", { name });
  return fetchMeshMe();
}
