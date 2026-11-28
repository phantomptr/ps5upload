// Play Rooms Details (design 6.4): the preview the host engine answers for a room before anyone
// joins (players, their quality, stream, approval), from the host engine on the home network or
// Tailscale (GET /api/join/preview) or once through the hub (GET /rooms/<lobby>/preview, cached
// 10 s). It never holds addresses, ways, guests' tags or chat.

import { readTier, type Tier } from "../../join/routeTier";
import { readChatMode, type ChatMode } from "./chat";
import { readRegion, type Region } from "./regions";

export interface PreviewPlayer {
  name: string;
  seat: number;
  kind: "host" | "ps5" | "web" | "app" | "other";
  tier: Tier | null;
  rttMs: number | null;
}

export interface RoomPreview {
  proto: number;
  caps: string[];
  host: { name: string; tag: string; ver: string; playingForMin: number | null };
  room: {
    name: string;
    visibility: Visibility;
    size: number;
    pw: boolean;
    approval: "ask" | "friends" | "password" | "anyone";
    chat: ChatMode | null;
    lang: string | null;
    region: Region | null;
    watchers: number;
  };
  game: { id: string | null; title: string; players: string | null; genre: string | null } | null;
  stream: { res: string; fps: number | null; codec: string; kbps: number | null; hdr: boolean } | null;
  hostLink: { rttMs: number | null; tier: Tier | null } | null;
  players: PreviewPlayer[];
  asking: number;
}

type Visibility = "private" | "friends" | "public";
const readVisibility = (v: unknown): Visibility | null => (v === "private" || v === "friends" || v === "public" ? v : null);
const num = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);
const str = (v: unknown): string | null => (typeof v === "string" && v ? v : null);
const obj = (v: unknown): Record<string, unknown> | null => (v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : null);

/** A preview document, checked; null when it isn't one. */
export function readPreview(v: unknown): RoomPreview | null {
  const o = obj(v);
  const h = obj(o?.host);
  const r = obj(o?.room);
  if (!o || !h || !r) return null;
  const g = obj(o.game);
  const s = obj(o.stream);
  const hl = obj(o.host_link);
  const ap = r.approval;
  return {
    proto: num(o.proto) ?? 2,
    caps: Array.isArray(o.caps) ? o.caps.filter((c): c is string => typeof c === "string") : [],
    host: {
      name: str(h.name) ?? "",
      tag: str(h.tag) ?? "",
      ver: str(h.ver) ?? "",
      playingForMin: num(h.playing_for_min),
    },
    room: {
      name: str(r.name) ?? "",
      visibility: readVisibility(r.visibility) ?? "public",
      size: num(r.size) ?? 4,
      pw: r.pw === true,
      approval: ap === "friends" || ap === "password" || ap === "anyone" ? ap : "ask",
      chat: readChatMode(r.chat),
      lang: str(r.lang),
      region: readRegion(r.region),
      watchers: num(r.watchers) ?? 0,
    },
    game: g && typeof g.title === "string" ? { id: str(g.id), title: g.title, players: str(g.players), genre: str(g.genre) } : null,
    stream: s
      ? { res: str(s.res) ?? "", fps: num(s.fps), codec: (str(s.codec) ?? "").toUpperCase(), kbps: num(s.kbps), hdr: s.hdr === true }
      : null,
    hostLink: hl ? { rttMs: num(hl.rtt_ms), tier: readTier(hl.tier) } : null,
    players: (Array.isArray(o.players) ? o.players : [])
      .map(obj)
      .filter((p): p is Record<string, unknown> => !!p)
      .map((p) => {
        const k = p.kind;
        return {
          name: str(p.name) ?? "",
          seat: num(p.seat) ?? 0,
          kind: (k === "host" || k === "ps5" || k === "web" || k === "app" ? k : "other") as PreviewPlayer["kind"],
          tier: readTier(p.tier),
          rttMs: num(p.rtt_ms),
        };
      })
      .sort((a, b) => a.seat - b.seat),
    asking: num(o.asking) ?? 0,
  };
}

/** Can this viewer's connection carry the stream? Measured download (kbit/s) vs the stream's
 *  bitrate with 20% to spare (the routes rule). Null when either is unknown. */
export function carriesStream(downKbps: number | null, streamKbps: number | null): boolean | null {
  if (downKbps == null || streamKbps == null || streamKbps <= 0) return null;
  return downKbps >= streamKbps * 1.2;
}

/** "1080p · 60 fps · HEVC · 15 Mbps · HDR off" in parts (the page joins them). */
export function streamParts(s: NonNullable<RoomPreview["stream"]>): string[] {
  const out: string[] = [];
  if (s.res) out.push(s.res);
  if (s.fps) out.push(`${s.fps} fps`);
  if (s.codec) out.push(s.codec);
  if (s.kbps) out.push(`${Math.round(s.kbps / 1000)} Mbps`);
  return out;
}
