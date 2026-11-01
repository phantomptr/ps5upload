// The picture for a local automation client (#615, engine rp_automation.rs).
//
// The engine never decodes video, so when a script or agent on the engine's computer asks
// `GET /api/ps5/rp/frame`, the engine sends the page that shows the session
// `{"snapshot": {"id", "format", "max_width"?, "after_seq"?, "wait_ms"?}}`. The page answers with
// the picture it shows now, read off its own canvas (the same one Screenshot saves), as binary
// `[9][u32 BE meta length][meta JSON][image]`. Nothing on screen changes and no second decoder
// runs. Only the page that runs the session is ever asked (never a watcher or a phone).

/** Page → engine binary kind of the answer. */
export const KIND_SNAPSHOT = 9;
/** The longest the page waits for a newer picture than `after_seq` (the engine waits 2 s). */
const MAX_WAIT_MS = 1500;

export interface SnapshotRequest {
  id: number;
  format: "png" | "jpeg";
  /** Scale down to at most this width (aspect kept); null: the picture's own size. */
  maxWidth: number | null;
  /** Answer with a picture newer than this one (frame_seq), waiting up to waitMs for it. */
  afterSeq: number | null;
  waitMs: number;
}

/** The engine's `snapshot` value, checked; null when it is not one. */
export function parseSnapshotRequest(v: unknown): SnapshotRequest | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  const int = (x: unknown) => (typeof x === "number" && Number.isInteger(x) && x >= 0 ? x : null);
  const id = int(o.id);
  if (id === null) return null;
  const format = o.format === "jpeg" ? "jpeg" : "png";
  const mw = int(o.max_width);
  const after = int(o.after_seq);
  const wait = int(o.wait_ms);
  return {
    id,
    format,
    maxWidth: mw && mw > 0 ? mw : null,
    afterSeq: after,
    waitMs: Math.min(wait ?? MAX_WAIT_MS, MAX_WAIT_MS),
  };
}

/** The size to encode at: the picture's own, or scaled down to maxWidth (never up). */
export function scaledSize(w: number, h: number, maxWidth: number | null): { w: number; h: number } {
  if (!maxWidth || w <= maxWidth || w <= 0) return { w, h };
  return { w: maxWidth, h: Math.max(1, Math.round((h * maxWidth) / w)) };
}

/** `[9][u32 BE meta length][meta JSON][image]`. */
export function encodeSnapshotReply(meta: Record<string, unknown>, image: Uint8Array = new Uint8Array(0)): Uint8Array<ArrayBuffer> {
  const m = new TextEncoder().encode(JSON.stringify(meta));
  const out = new Uint8Array(5 + m.length + image.length);
  out[0] = KIND_SNAPSHOT;
  new DataView(out.buffer).setUint32(1, m.length);
  out.set(m, 5);
  out.set(image, 5 + m.length);
  return out;
}

/** What a snapshot reads from the stream. */
export interface SnapshotSource {
  /** The canvas the picture is drawn on. */
  canvas: HTMLCanvasElement;
  /** Pictures drawn so far on this page (0: none yet). */
  frameSeq(): number;
  /** When the last picture was drawn (Date.now()). */
  frameAt(): number;
  /** Resolves true once a picture after `seq` is drawn, false after `ms`. */
  waitForFrame(seq: number, ms: number): Promise<boolean>;
}

export interface SnapshotDeps {
  /** A canvas to scale into (document.createElement("canvas") in the app). */
  makeCanvas?: () => HTMLCanvasElement;
  now?: () => number;
}

function toBlob(c: HTMLCanvasElement, type: string): Promise<Blob | null> {
  return new Promise((resolve) => {
    try {
      c.toBlob((b) => resolve(b), type, type === "image/jpeg" ? 0.9 : undefined);
    } catch {
      resolve(null);
    }
  });
}

/** The whole answer to `req`: the picture with its metadata, or `{id, error}`. */
export async function takeSnapshot(req: SnapshotRequest, src: SnapshotSource, deps: SnapshotDeps = {}): Promise<Uint8Array<ArrayBuffer>> {
  const now = deps.now ?? Date.now;
  const fail = (error: string) => encodeSnapshotReply({ id: req.id, error });
  if (src.frameSeq() === 0) return fail("no_picture");
  if (req.afterSeq !== null && src.frameSeq() <= req.afterSeq) {
    if (!(await src.waitForFrame(req.afterSeq, req.waitMs))) return fail("no_newer_frame");
  }
  const cv = src.canvas;
  const { w, h } = scaledSize(cv.width, cv.height, req.maxWidth);
  // Read now, so the metadata describes the picture read.
  const frameSeq = src.frameSeq();
  const frameAt = src.frameAt();
  let from = cv;
  if (w !== cv.width || h !== cv.height) {
    const make = deps.makeCanvas ?? (() => document.createElement("canvas"));
    const c = make();
    c.width = w;
    c.height = h;
    const ctx = c.getContext("2d");
    if (!ctx) return fail("encode_failed");
    ctx.drawImage(cv, 0, 0, w, h);
    from = c;
  }
  const capturedAt = now();
  const blob = await toBlob(from, req.format === "jpeg" ? "image/jpeg" : "image/png");
  if (!blob) return fail("encode_failed");
  const image = new Uint8Array(await blob.arrayBuffer());
  return encodeSnapshotReply(
    {
      id: req.id,
      format: req.format,
      width: w,
      height: h,
      captured_at_ms: capturedAt,
      frame_at_ms: frameAt,
      frame_seq: frameSeq,
    },
    image,
  );
}
