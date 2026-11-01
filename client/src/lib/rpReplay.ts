// Instant replay: the engine keeps the last seconds of the stream as the console sent them
// (engine rp_replay.rs); this turns a snapshot of them into an MP4. The video is never
// re-encoded (the console's H.264/HEVC goes in as it came). The audio is turned into AAC where
// this device can encode it (Apple's Photos takes no Opus); elsewhere the Opus goes in as is.

import {
  AudioSample,
  AudioSampleSource,
  BufferTarget,
  EncodedAudioPacketSource,
  EncodedPacket,
  EncodedVideoPacketSource,
  Mp4OutputFormat,
  Output,
  canEncodeAudio,
} from "mediabunny";

import { getEngineUrl } from "../state/engine";
import { withDecoderStart } from "./videoDecoderGuard";

export interface ReplayFrame {
  kind: 1 | 4;
  tUs: number;
  key: boolean;
  data: Uint8Array;
}

export interface Replay {
  videoHeader: Uint8Array;
  hevc: boolean;
  audio: { channels: number; rate: number } | null;
  frames: ReplayFrame[];
}

const MAGIC = "PSRP1";

/** Whether an Annex-B header (the parameter sets) is HEVC: its first NAL is a VPS (32). */
export function isHevcHeader(h: Uint8Array): boolean {
  for (let i = 0; i + 3 < h.length; i++) {
    if (h[i] === 0 && h[i + 1] === 0 && h[i + 2] === 1) return ((h[i + 3] >> 1) & 0x3f) === 32;
  }
  return false;
}

/** Reads the engine's snapshot (rp_replay.rs `encode`). Throws on anything else. */
export function parseReplay(buf: ArrayBuffer): Replay {
  const b = new Uint8Array(buf);
  const dv = new DataView(buf);
  if (new TextDecoder().decode(b.subarray(0, 5)) !== MAGIC) throw new Error("not a replay");
  let i = 5;
  let videoHeader: Uint8Array | null = null;
  let audio: Replay["audio"] = null;
  const frames: ReplayFrame[] = [];
  while (i < b.length) {
    if (i + 13 > b.length) throw new Error("replay cut short");
    const kind = b[i];
    // t_us is a u64; a session's microseconds stay far below 2^53.
    const tUs = dv.getUint32(i + 1) * 2 ** 32 + dv.getUint32(i + 5);
    const len = dv.getUint32(i + 9);
    i += 13;
    let key = false;
    if (kind === 1) key = b[i++] === 1;
    if (i + len > b.length) throw new Error("replay cut short");
    const data = b.subarray(i, i + len);
    i += len;
    if (kind === 0) videoHeader = data;
    else if (kind === 3) audio = { channels: data[0], rate: new DataView(data.buffer, data.byteOffset).getUint32(2) };
    else if (kind === 1 || kind === 4) frames.push({ kind, tUs, key, data });
  }
  if (!videoHeader) throw new Error("replay has no video header");
  return { videoHeader, hevc: isHevcHeader(videoHeader), audio, frames };
}

/** Annex-B joined: the parameter sets ahead of the first keyframe (MP4 needs them there). */
function joined(a: Uint8Array, b: Uint8Array): Uint8Array {
  const out = new Uint8Array(a.length + b.length);
  out.set(a, 0);
  out.set(b, a.length);
  return out;
}

const hex2 = (n: number) => n.toString(16).padStart(2, "0");

/** The H.264 codec string from the parameter sets (`avc1.PPCCLL` from the SPS), or "". */
export function avcCodecFromHeader(h: Uint8Array): string {
  for (let i = 0; i + 6 < h.length; i++) {
    if (h[i] === 0 && h[i + 1] === 0 && h[i + 2] === 1 && (h[i + 3] & 0x1f) === 7) {
      return `avc1.${hex2(h[i + 4])}${hex2(h[i + 5])}${hex2(h[i + 6])}`;
    }
  }
  return "";
}

/** The picture's size, from decoding the first keyframe (exact whatever the stream's shape). */
async function probeSize(codec: string, keyframe: Uint8Array): Promise<{ width: number; height: number }> {
  // The Linux app: the GL display WebKitGTK needs before a decoder (videoDecoderGuard).
  return withDecoderStart(() => probeSizeNow(codec, keyframe));
}

async function probeSizeNow(codec: string, keyframe: Uint8Array): Promise<{ width: number; height: number }> {
  let size: { width: number; height: number } | null = null;
  let failed: unknown = null;
  const dec = new VideoDecoder({
    output: (f) => {
      size ??= { width: f.displayWidth, height: f.displayHeight };
      f.close();
    },
    error: (e) => {
      failed = e;
    },
  });
  dec.configure({ codec, optimizeForLatency: true });
  dec.decode(new EncodedVideoChunk({ type: "key", timestamp: 0, data: keyframe }));
  await dec.flush().catch((e) => {
    failed ??= e;
  });
  dec.close();
  if (!size) throw failed instanceof Error ? failed : new Error("could not read the replay's picture size");
  return size;
}

/** Opus frames decoded to samples (for the AAC encoder), in order. */
async function decodeOpus(frames: ReplayFrame[], channels: number, rate: number, t0: number): Promise<AudioData[]> {
  const out: AudioData[] = [];
  let failed: unknown = null;
  const dec = new AudioDecoder({
    output: (d) => out.push(d),
    error: (e) => {
      failed = e;
    },
  });
  dec.configure({ codec: "opus", sampleRate: rate, numberOfChannels: channels });
  for (const f of frames) {
    dec.decode(new EncodedAudioChunk({ type: "key", timestamp: f.tUs - t0, data: f.data }));
  }
  await dec.flush();
  dec.close();
  if (failed) {
    for (const d of out) d.close();
    throw failed;
  }
  return out;
}

/** What the stream knows about its video (its decoder's codec string), when it is playing. */
export interface ReplayHint {
  codec?: string;
}

/** The replay as an MP4 file. */
export async function replayToMp4(r: Replay, hint: ReplayHint = {}): Promise<Blob> {
  const made: { output: Output | null } = { output: null };
  try {
    return await writeMp4(r, hint, made);
  } catch (e) {
    // A failure part-way leaves nothing running behind it.
    await made.output?.cancel().catch(() => {});
    throw e;
  }
}

async function writeMp4(r: Replay, hint: ReplayHint, made: { output: Output | null }): Promise<Blob> {
  const video = r.frames.filter((f) => f.kind === 1);
  const first = video.findIndex((f) => f.key);
  if (first < 0) throw new Error("no keyframe in the replay yet");
  const t0 = video[first].tUs;
  const frames = video.slice(first);
  const audioFrames = r.frames.filter((f) => f.kind === 4 && f.tUs >= t0);

  const target = new BufferTarget();
  const output = new Output({ format: new Mp4OutputFormat({ fastStart: "in-memory" }), target });
  made.output = output;
  const vsrc = new EncodedVideoPacketSource(r.hevc ? "hevc" : "avc");
  output.addVideoTrack(vsrc, { frameRate: 60 });

  const a = r.audio;
  const aac = !!a && audioFrames.length > 0 && (await canEncodeAudio("aac", { numberOfChannels: a.channels, sampleRate: a.rate }));
  const aacSrc = aac ? new AudioSampleSource({ codec: "aac", bitrate: 160_000 }) : null;
  const opusSrc = !aac && a && audioFrames.length ? new EncodedAudioPacketSource("opus") : null;
  if (aacSrc) output.addAudioTrack(aacSrc);
  if (opusSrc) output.addAudioTrack(opusSrc);

  const codec = hint.codec || (r.hevc ? "" : avcCodecFromHeader(r.videoHeader));
  if (!codec) throw new Error("the replay's video codec is not known");
  const firstKey = joined(r.videoHeader, frames[0].data);
  const { width, height } = await probeSize(codec, firstKey);

  await output.start();
  for (let i = 0; i < frames.length; i++) {
    const f = frames[i];
    const next = frames[i + 1]?.tUs ?? f.tUs + 16_667;
    const data = i === 0 ? firstKey : f.data;
    await vsrc.add(
      new EncodedPacket(data, f.key ? "key" : "delta", (f.tUs - t0) / 1e6, Math.max(1, next - f.tUs) / 1e6),
      i === 0 ? { decoderConfig: { codec, codedWidth: width, codedHeight: height } } : undefined,
    );
  }
  if (aacSrc && a) {
    for (const d of await decodeOpus(audioFrames, a.channels, a.rate, t0)) {
      const s = new AudioSample(d);
      await aacSrc.add(s);
      s.close();
      d.close();
    }
  } else if (opusSrc && a) {
    for (let i = 0; i < audioFrames.length; i++) {
      const f = audioFrames[i];
      const next = audioFrames[i + 1]?.tUs ?? f.tUs + 10_000;
      await opusSrc.add(
        new EncodedPacket(f.data, "key", (f.tUs - t0) / 1e6, Math.max(1, next - f.tUs) / 1e6),
        i === 0 ? { decoderConfig: { codec: "opus", numberOfChannels: a.channels, sampleRate: a.rate } } : undefined,
      );
    }
  }
  await output.finalize();
  if (!target.buffer) throw new Error("the MP4 came out empty");
  return new Blob([target.buffer], { type: "video/mp4" });
}

/** The engine's replay of the running stream, as an MP4. */
export async function fetchReplayMp4(hint: ReplayHint = {}): Promise<Blob> {
  const res = await fetch(`${getEngineUrl()}/api/ps5/rp/replay`);
  if (!res.ok) {
    const body = (await res.json().catch(() => ({}))) as { error?: string };
    throw new Error(body.error || `HTTP ${res.status}`);
  }
  return replayToMp4(parseReplay(await res.arrayBuffer()), hint);
}
