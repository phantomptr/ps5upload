/**
 * What this device's web view can decode and show, for a bug report: WebCodecs' answers for the
 * codecs Remote Play uses (H.264 with and without a description, HEVC Main and Main10, each asked
 * for in hardware and in software), Opus, what the system's media player (MSE and Safari's
 * ManagedMediaSource) takes, WebGL 2 and its renderer, and whether the page is a secure context
 * (WebCodecs needs one). Each probe has a time limit and none throws: an answer that could not be
 * had is null.
 */

/** Per probe. A web view that never answers must not hold the report up. */
const PROBE_MS = 1500;

export const AVC_CODECS = ["avc1.4d4028", "avc1.640028", "avc1.42e01f"] as const;
export const HEVC_CODECS = ["hvc1.1.6.L120.90", "hev1.1.6.L120.90", "hvc1.2.4.L120.90"] as const;
export const MSE_TYPES = [
  'video/mp4; codecs="avc1.640028"',
  'video/mp4; codecs="hvc1.1.6.L120.90"',
  'video/mp4; codecs="hev1.1.6.L120.90"',
  'audio/mp4; codecs="opus"',
] as const;

/** One video codec: supported with prefer-hardware / prefer-software (null: no answer). */
export interface VideoProbe {
  codec: string;
  /** Asked with an avcC description (H.264 only). */
  desc: boolean;
  hw: boolean | null;
  sw: boolean | null;
}

export interface MediaProbe {
  secure_context: boolean | null;
  has: {
    VideoDecoder: boolean;
    AudioDecoder: boolean;
    MediaSource: boolean;
    ManagedMediaSource: boolean;
    AudioWorklet: boolean;
  };
  video: VideoProbe[];
  opus: boolean | null;
  /** Type -> isTypeSupported; null when that API is missing. */
  mse: Record<string, boolean> | null;
  managed_mse: Record<string, boolean> | null;
  webgl2: { ok: boolean; vendor: string | null; renderer: string | null } | null;
  ms: number;
}

/** A minimal avcC for a 1920x1080 High (or the profile in `codec`) stream: SPS and PPS of the
 *  right shape. isConfigSupported only parses it. */
export function sampleAvcC(codec: string): Uint8Array {
  const hex = codec.split(".")[1] ?? "640028";
  const profile = parseInt(hex.slice(0, 2), 16) || 0x64;
  const compat = parseInt(hex.slice(2, 4), 16) || 0;
  const level = parseInt(hex.slice(4, 6), 16) || 0x28;
  const sps = [0x67, profile, compat, level, 0xac, 0xd9, 0x40, 0x78, 0x02, 0x27, 0xe5, 0x84, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0xf0, 0x3c, 0x60, 0xc6, 0x58];
  const pps = [0x68, 0xeb, 0xe3, 0xcb, 0x22, 0xc0];
  return new Uint8Array([
    1, profile, compat, level, 0xff, 0xe1,
    (sps.length >> 8) & 0xff, sps.length & 0xff, ...sps,
    1, (pps.length >> 8) & 0xff, pps.length & 0xff, ...pps,
  ]);
}

function bounded<T>(p: Promise<T>, ms = PROBE_MS): Promise<T | null> {
  return new Promise((resolve) => {
    const t = setTimeout(() => resolve(null), ms);
    p.then(
      (v) => {
        clearTimeout(t);
        resolve(v);
      },
      () => {
        clearTimeout(t);
        resolve(null);
      },
    );
  });
}

type G = typeof globalThis & {
  VideoDecoder?: { isConfigSupported(c: unknown): Promise<{ supported?: boolean }> };
  AudioDecoder?: { isConfigSupported(c: unknown): Promise<{ supported?: boolean }> };
  MediaSource?: { isTypeSupported(t: string): boolean };
  ManagedMediaSource?: { isTypeSupported(t: string): boolean };
  AudioWorkletNode?: unknown;
  isSecureContext?: boolean;
};

async function videoSupported(
  vd: NonNullable<G["VideoDecoder"]>,
  codec: string,
  description: Uint8Array | undefined,
  hw: "prefer-hardware" | "prefer-software",
): Promise<boolean | null> {
  const cfg: Record<string, unknown> = { codec, codedWidth: 1920, codedHeight: 1080, hardwareAcceleration: hw };
  if (description) cfg.description = description;
  const r = await bounded(Promise.resolve().then(() => vd.isConfigSupported(cfg)));
  return r ? !!r.supported : null;
}

function typeTable(api: { isTypeSupported(t: string): boolean } | undefined): Record<string, boolean> | null {
  if (!api || typeof api.isTypeSupported !== "function") return null;
  const out: Record<string, boolean> = {};
  for (const t of MSE_TYPES) {
    try {
      out[t] = !!api.isTypeSupported(t);
    } catch {
      out[t] = false;
    }
  }
  return out;
}

function probeWebgl2(): MediaProbe["webgl2"] {
  if (typeof document === "undefined") return null;
  try {
    const c = document.createElement("canvas");
    const gl = c.getContext("webgl2") as WebGL2RenderingContext | null;
    if (!gl) return { ok: false, vendor: null, renderer: null };
    const ext = gl.getExtension("WEBGL_debug_renderer_info") as { UNMASKED_VENDOR_WEBGL: number; UNMASKED_RENDERER_WEBGL: number } | null;
    const str = (v: unknown) => (typeof v === "string" && v ? v : null);
    const out = {
      ok: true,
      vendor: str(ext ? gl.getParameter(ext.UNMASKED_VENDOR_WEBGL) : gl.getParameter(gl.VENDOR)),
      renderer: str(ext ? gl.getParameter(ext.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER)),
    };
    gl.getExtension("WEBGL_lose_context")?.loseContext();
    return out;
  } catch {
    return { ok: false, vendor: null, renderer: null };
  }
}

export async function probeMedia(): Promise<MediaProbe> {
  const t0 = Date.now();
  const g = globalThis as G;
  const vd = typeof g.VideoDecoder === "function" || typeof g.VideoDecoder === "object" ? g.VideoDecoder : undefined;
  const ad = typeof g.AudioDecoder === "function" || typeof g.AudioDecoder === "object" ? g.AudioDecoder : undefined;
  const cases: { codec: string; desc: boolean }[] = [
    ...AVC_CODECS.flatMap((codec) => [
      { codec, desc: false },
      { codec, desc: true },
    ]),
    ...HEVC_CODECS.map((codec) => ({ codec, desc: false })),
  ];
  const video: VideoProbe[] = await Promise.all(
    cases.map(async ({ codec, desc }) => {
      if (!vd) return { codec, desc, hw: null, sw: null };
      const d = desc ? sampleAvcC(codec) : undefined;
      const [hw, sw] = await Promise.all([
        videoSupported(vd, codec, d, "prefer-hardware"),
        videoSupported(vd, codec, d, "prefer-software"),
      ]);
      return { codec, desc, hw, sw };
    }),
  );
  let opus: boolean | null = null;
  if (ad) {
    const r = await bounded(
      Promise.resolve().then(() => ad.isConfigSupported({ codec: "opus", sampleRate: 48000, numberOfChannels: 2 })),
    );
    opus = r ? !!r.supported : null;
  }
  return {
    secure_context: typeof g.isSecureContext === "boolean" ? g.isSecureContext : null,
    has: {
      VideoDecoder: !!vd,
      AudioDecoder: !!ad,
      MediaSource: !!g.MediaSource,
      ManagedMediaSource: !!g.ManagedMediaSource,
      AudioWorklet: typeof g.AudioWorkletNode !== "undefined",
    },
    video,
    opus,
    mse: typeTable(g.MediaSource),
    managed_mse: typeTable(g.ManagedMediaSource),
    webgl2: probeWebgl2(),
    ms: Date.now() - t0,
  };
}
