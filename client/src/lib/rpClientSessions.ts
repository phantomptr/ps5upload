/**
 * The page's last Remote Play sessions as this device saw them, for a bug report: which codec was
 * asked, which decoder and drawing path were chosen, every decoder failure with the config it was
 * given (codec string, coded size, hardware or software, the description's length; never frame
 * data), each fallback, and how the session ended. The engine keeps its own side (handshake
 * stages, the console's answers) and, from newer engines, these decoder events too.
 *
 * Kept in memory only, the last RP_CLIENT_SESSIONS sessions; small and bounded per session.
 */

/** Sessions kept. */
export const RP_CLIENT_SESSIONS = 8;
/** Decoder events kept (and logged and sent to the engine) per session. */
export const MAX_EVENTS = 32;
const MAX_FALLBACKS = 16;
const MAX_TEXT = 300;

/** One decoder decision or failure. Also what the page sends the engine as `{"decoder": …}`. */
export interface RpDecoderEvent {
  /** "configure", "configure_error", "decode_error", "no_config", "cannot_decode", "mse",
   *  "audio_decoder", "audio_error", … */
  event: string;
  /** "webcodecs-hw", "webcodecs-sw", "webcodecs-default", "mse", "worker", … */
  path?: string;
  codec?: string;
  coded_width?: number;
  coded_height?: number;
  /** The hardwareAcceleration asked for ("prefer-hardware", "prefer-software"), null when none. */
  hw?: string | null;
  /** The decoder description's length in bytes, null when there was none. */
  desc_len?: number | null;
  error?: string;
}

export interface RpClientSession {
  started_at: number;
  ended_at: number | null;
  /** What the page asked the console for: "h264", "h265", "h265_hdr" (or the preference before
   *  that was settled). */
  codec_asked: string | null;
  /** The decoder in use last: "webcodecs-hw", "webcodecs-sw", "webcodecs-default", "mse". */
  video_path: string | null;
  /** How the picture is drawn: "gl", "2d", "hdr". */
  draw_path: string | null;
  hdr: boolean | null;
  /** The sound's path: "worker", "page", "scheduled" (no worklet), "mse", "wasm" (the WebAssembly decoder), "none". */
  audio_path: string | null;
  frames_decoded: number | null;
  events: (RpDecoderEvent & { t: number })[];
  fallbacks: string[];
  end_reason: string | null;
}

export interface RpClientSessionHandle {
  set(fields: Partial<Pick<RpClientSession, "codec_asked" | "video_path" | "draw_path" | "hdr" | "audio_path" | "frames_decoded">>): void;
  /** False once the session holds MAX_EVENTS: the event is dropped, and the caller neither logs
   *  nor sends it (a decoder failing on every frame would otherwise flood both). */
  event(ev: RpDecoderEvent): boolean;
  fallback(text: string): void;
  /** The first reason given wins; later calls only stamp nothing. */
  end(reason: string): void;
}

const ring: RpClientSession[] = [];

const cut = (s: string): string => (s.length > MAX_TEXT ? `${s.slice(0, MAX_TEXT)}…` : s);

export function beginRpClientSession(codecAsked: string | null, now = Date.now()): RpClientSessionHandle {
  const s: RpClientSession = {
    started_at: now,
    ended_at: null,
    codec_asked: codecAsked,
    video_path: null,
    draw_path: null,
    hdr: null,
    audio_path: null,
    frames_decoded: null,
    events: [],
    fallbacks: [],
    end_reason: null,
  };
  ring.push(s);
  while (ring.length > RP_CLIENT_SESSIONS) ring.shift();
  return {
    set(fields) {
      Object.assign(s, fields);
    },
    event(ev) {
      if (s.events.length >= MAX_EVENTS) return false;
      const e = { ...ev, t: Date.now() };
      if (e.error !== undefined) e.error = cut(e.error);
      s.events.push(e);
      return true;
    },
    fallback(text) {
      if (s.fallbacks.length < MAX_FALLBACKS) s.fallbacks.push(cut(text));
    },
    end(reason) {
      if (s.ended_at !== null) return;
      s.ended_at = Date.now();
      s.end_reason = cut(reason);
    },
  };
}

/** The kept sessions, oldest first (copies). */
export function rpClientSessions(): RpClientSession[] {
  return ring.map((s) => ({ ...s, events: s.events.map((e) => ({ ...e })), fallbacks: [...s.fallbacks] }));
}

/** For tests. */
export function resetRpClientSessions(): void {
  ring.length = 0;
}

/** The facts of a video decoder config worth keeping: never its description's bytes. */
export function videoConfigFacts(
  cfg: { codec: string; codedWidth?: number; codedHeight?: number; hardwareAcceleration?: string; description?: unknown },
  size?: { width: number; height: number },
): Pick<RpDecoderEvent, "codec" | "coded_width" | "coded_height" | "hw" | "desc_len"> {
  const d = cfg.description as { byteLength?: number } | undefined;
  return {
    codec: cfg.codec,
    coded_width: cfg.codedWidth ?? size?.width,
    coded_height: cfg.codedHeight ?? size?.height,
    hw: cfg.hardwareAcceleration ?? null,
    desc_len: d && typeof d.byteLength === "number" ? d.byteLength : null,
  };
}

/** "webcodecs-hw" / "-sw" / "-default" for the hardwareAcceleration a config ended up with. */
export function webcodecsPath(hw: string | null | undefined): string {
  return hw === "prefer-hardware" ? "webcodecs-hw" : hw === "prefer-software" ? "webcodecs-sw" : "webcodecs-default";
}
