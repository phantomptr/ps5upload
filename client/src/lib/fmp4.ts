// Fragmented MP4 for Media Source Extensions: the watch page's way to play the stream where
// WebCodecs is missing. Browsers expose VideoDecoder/AudioDecoder only on a secure page, and a
// phone opens the watch link as plain http://<computer>:19113, so there the picture has to go
// through a <video> element instead. These build the init segment (codec setup) and one media
// segment per access unit or Opus frame from what the engine sends.

/** The NAL units of an Annex-B stream, without their start codes. */
export function annexBNals(stream: Uint8Array): Uint8Array[] {
  const starts: number[] = [];
  for (let i = 0; i + 2 < stream.length; i++) {
    if (stream[i] === 0 && stream[i + 1] === 0 && stream[i + 2] === 1) {
      starts.push(i + 3);
      i += 2;
    }
  }
  return starts
    .map((s, k) => {
      let end = k + 1 < starts.length ? starts[k + 1] - 3 : stream.length;
      while (end > s && stream[end - 1] === 0) end--;
      return stream.subarray(s, end);
    })
    .filter((n) => n.length > 0);
}

const nalType = (n: Uint8Array, hevc: boolean) => (hevc ? (n[0] >> 1) & 0x3f : n[0] & 0x1f);

/** The emulation-prevention bytes removed (00 00 03 → 00 00). */
function rbsp(nal: Uint8Array): Uint8Array {
  const out = new Uint8Array(nal.length);
  let n = 0;
  let zeros = 0;
  for (const b of nal) {
    if (zeros >= 2 && b === 3) {
      zeros = 0;
      continue;
    }
    zeros = b === 0 ? zeros + 1 : 0;
    out[n++] = b;
  }
  return out.subarray(0, n);
}

function box(type: string, ...parts: Uint8Array[]): Uint8Array {
  const size = 8 + parts.reduce((a, p) => a + p.length, 0);
  const out = new Uint8Array(size);
  new DataView(out.buffer).setUint32(0, size);
  for (let i = 0; i < 4; i++) out[4 + i] = type.charCodeAt(i);
  let o = 8;
  for (const p of parts) {
    out.set(p, o);
    o += p.length;
  }
  return out;
}

/** Big-endian fields: [value, bytes]. */
function u(...fields: [number, number][]): Uint8Array {
  const out = new Uint8Array(fields.reduce((a, [, n]) => a + n, 0));
  let o = 0;
  for (const [v, n] of fields) {
    for (let i = n - 1; i >= 0; i--) out[o + n - 1 - i] = Math.floor(v / 2 ** (8 * i)) & 0xff;
    o += n;
  }
  return out;
}

const full = (type: string, version: number, flags: number, ...parts: Uint8Array[]) =>
  box(type, u([version, 1], [flags, 3]), ...parts);

const MATRIX = u([0x10000, 4], [0, 4], [0, 4], [0, 4], [0x10000, 4], [0, 4], [0, 4], [0, 4], [0x40000000, 4]);

/** What the init segment needs to know about a track. */
interface Track {
  handler: "vide" | "soun";
  timescale: number;
  sampleEntry: Uint8Array;
  width?: number;
  height?: number;
}

function initSegment(t: Track): Uint8Array {
  const video = t.handler === "vide";
  const ftyp = box("ftyp", new TextEncoder().encode("isom"), u([1, 4]), new TextEncoder().encode("isomiso6mp41"));
  const mvhd = full(
    "mvhd",
    0,
    0,
    u([0, 4], [0, 4], [t.timescale, 4], [0, 4], [0x10000, 4], [0x100, 2], [0, 2], [0, 4], [0, 4]),
    MATRIX,
    new Uint8Array(24),
    u([2, 4]),
  );
  const tkhd = full(
    "tkhd",
    0,
    3,
    u([0, 4], [0, 4], [1, 4], [0, 4], [0, 4], [0, 4], [0, 4], [0, 2], [0, 2], [video ? 0 : 0x100, 2], [0, 2]),
    MATRIX,
    u([(t.width ?? 0) * 0x10000, 4], [(t.height ?? 0) * 0x10000, 4]),
  );
  const mdhd = full("mdhd", 0, 0, u([0, 4], [0, 4], [t.timescale, 4], [0, 4], [0x55c4, 2], [0, 2]));
  const hdlr = full("hdlr", 0, 0, u([0, 4]), new TextEncoder().encode(t.handler), new Uint8Array(12), u([0, 1]));
  const head = video ? full("vmhd", 0, 1, new Uint8Array(8)) : full("smhd", 0, 0, u([0, 4]));
  const dinf = box("dinf", full("dref", 0, 0, u([1, 4]), full("url ", 0, 1)));
  const stbl = box(
    "stbl",
    full("stsd", 0, 0, u([1, 4]), t.sampleEntry),
    full("stts", 0, 0, u([0, 4])),
    full("stsc", 0, 0, u([0, 4])),
    full("stsz", 0, 0, u([0, 4], [0, 4])),
    full("stco", 0, 0, u([0, 4])),
  );
  const trak = box("trak", tkhd, box("mdia", mdhd, hdlr, box("minf", head, dinf, stbl)));
  const mvex = box("mvex", full("trex", 0, 0, u([1, 4], [1, 4], [0, 4], [0, 4], [0, 4])));
  const moov = box("moov", mvhd, trak, mvex);
  const out = new Uint8Array(ftyp.length + moov.length);
  out.set(ftyp, 0);
  out.set(moov, ftyp.length);
  return out;
}

/** One sample as a media segment. */
function mediaSegment(seq: number, time: number, duration: number, data: Uint8Array, key: boolean): Uint8Array {
  const flags = key ? 0x02000000 : 0x01010000;
  const trun = (offset: number) => full("trun", 0, 0x000701, u([1, 4], [offset, 4], [duration, 4], [data.length, 4], [flags, 4]));
  const traf = (offset: number) =>
    box("traf", full("tfhd", 0, 0x020000, u([1, 4])), full("tfdt", 1, 0, u([time, 8])), trun(offset));
  const mfhd = full("mfhd", 0, 0, u([seq, 4]));
  const moofLen = box("moof", mfhd, traf(0)).length;
  const moof = box("moof", mfhd, traf(moofLen + 8));
  const mdat = box("mdat", data);
  const out = new Uint8Array(moof.length + mdat.length);
  out.set(moof, 0);
  out.set(mdat, moof.length);
  return out;
}

/** Annex-B access unit → 4-byte length-prefixed NAL units (access unit delimiters dropped, and
 *  the parameter sets too with `dropParams`: an avcC description carries them). */
export function lengthPrefixed(au: Uint8Array, hevc: boolean, dropParams = false): Uint8Array {
  const skip = hevc ? [35, ...(dropParams ? [32, 33, 34] : [])] : [9, ...(dropParams ? [7, 8] : [])];
  const nals = annexBNals(au).filter((n) => !skip.includes(nalType(n, hevc)));
  const out = new Uint8Array(nals.reduce((a, n) => a + 4 + n.length, 0));
  const dv = new DataView(out.buffer);
  let o = 0;
  for (const n of nals) {
    dv.setUint32(o, n.length);
    out.set(n, o + 4);
    o += 4 + n.length;
  }
  return out;
}

const hex = (b: number) => b.toString(16).padStart(2, "0");

const sized = (n: Uint8Array) => {
  const out = new Uint8Array(2 + n.length);
  out[0] = n.length >> 8;
  out[1] = n.length & 0xff;
  out.set(n, 2);
  return out;
};

const cat = (parts: Uint8Array[]) => {
  const out = new Uint8Array(parts.reduce((a, p) => a + p.length, 0));
  let o = 0;
  for (const p of parts) {
    out.set(p, o);
    o += p.length;
  }
  return out;
};

/** The AVCDecoderConfigurationRecord (avcC's contents) for the SPS and PPS in an Annex-B
 *  header, or null without them. Also WebCodecs' `description` for H.264 in AVC form. */
export function avcDescription(header: Uint8Array): Uint8Array | null {
  const nals = annexBNals(header);
  const sps = nals.filter((n) => nalType(n, false) === 7);
  const pps = nals.filter((n) => nalType(n, false) === 8);
  if (!sps.length || !pps.length || sps[0].length < 4) return null;
  return cat([
    u([1, 1], [sps[0][1], 1], [sps[0][2], 1], [sps[0][3], 1], [0xff, 1], [0xe0 | sps.length, 1]),
    ...sps.map(sized),
    u([pps.length, 1]),
    ...pps.map(sized),
  ]);
}

/** The video track's init segment and MSE type, from the stream's parameter sets; null when
 *  they are missing. `size` is the visible picture (rpSps.spsVisibleRect). */
export function videoInit(
  header: Uint8Array,
  hevc: boolean,
  size: { w: number; h: number },
): { init: Uint8Array; mime: string } | null {
  const nals = annexBNals(header);
  const of = (t: number) => nals.filter((n) => nalType(n, hevc) === t);
  const visual = (fourcc: string, config: Uint8Array) =>
    box(
      fourcc,
      new Uint8Array(6),
      u([1, 2], [0, 16], [size.w, 2], [size.h, 2], [0x480000, 4], [0x480000, 4], [0, 4], [1, 2]),
      new Uint8Array(32),
      u([0x18, 2], [0xffff, 2]),
      config,
    );

  if (!hevc) {
    const sps = of(7);
    const record = avcDescription(header);
    if (!record) return null;
    const avcC = box("avcC", record);
    return {
      init: initSegment({ handler: "vide", timescale: 90000, sampleEntry: visual("avc1", avcC), width: size.w, height: size.h }),
      mime: `video/mp4; codecs="avc1.${hex(sps[0][1])}${hex(sps[0][2])}${hex(sps[0][3])}"`,
    };
  }

  const vps = of(32);
  const sps = of(33);
  const pps = of(34);
  if (!vps.length || !sps.length || !pps.length) return null;
  // profile_tier_level's general part: the 12 bytes after the NAL header and the
  // vps-id/sub-layers byte.
  const r = rbsp(sps[0]);
  if (r.length < 15) return null;
  const ptl = r.subarray(3, 15);
  const profileIdc = ptl[0] & 0x1f;
  const depth = profileIdc === 2 ? 2 : 0; // Main 10
  const array = (type: number, list: Uint8Array[]) => cat([u([0x80 | type, 1], [list.length, 2]), ...list.map(sized)]);
  const hvcC = box(
    "hvcC",
    u([1, 1]),
    ptl,
    u([0xf000, 2], [0xfc, 1], [0xfd, 1], [0xf8 | depth, 1], [0xf8 | depth, 1], [0, 2], [0x0f, 1], [3, 1]),
    array(32, vps),
    array(33, sps),
    array(34, pps),
  );
  // hvc1.<space><profile>.<compat, bit-reversed>.<tier><level>.<constraint bytes>
  const space = ["", "A", "B", "C"][ptl[0] >> 6];
  let compat = 0;
  const c32 = ((ptl[1] << 24) | (ptl[2] << 16) | (ptl[3] << 8) | ptl[4]) >>> 0;
  for (let i = 0; i < 32; i++) if (c32 & (1 << i)) compat |= 1 << (31 - i);
  const constraint = Array.from(ptl.subarray(5, 11));
  while (constraint.length && constraint[constraint.length - 1] === 0) constraint.pop();
  const codec = [
    `hvc1.${space}${profileIdc}`,
    (compat >>> 0).toString(16),
    `${ptl[0] & 0x20 ? "H" : "L"}${ptl[11]}`,
    ...constraint.map((b) => b.toString(16)),
  ].join(".");
  return {
    init: initSegment({ handler: "vide", timescale: 90000, sampleEntry: visual("hvc1", hvcC), width: size.w, height: size.h }),
    mime: `video/mp4; codecs="${codec}"`,
  };
}

/** One access unit as a media segment (90 kHz times). */
export function videoSegment(seq: number, time: number, duration: number, au: Uint8Array, hevc: boolean, key: boolean): Uint8Array {
  return mediaSegment(seq, time, duration, lengthPrefixed(au, hevc), key);
}

/** The Opus track's init segment (48 kHz times, as Opus always is). */
export function opusInit(channels: number, rate: number): Uint8Array {
  const dOps = box("dOps", u([0, 1], [channels, 1], [312, 2], [rate, 4], [0, 2], [0, 1]));
  const entry = box("Opus", new Uint8Array(6), u([1, 2], [0, 8], [channels, 2], [16, 2], [0, 4], [48000 * 0x10000, 4]), dOps);
  return initSegment({ handler: "soun", timescale: 48000, sampleEntry: entry });
}

/** How long an Opus packet plays, in 48 kHz samples (from its TOC byte, RFC 6716 3.1). */
export function opusSamples(packet: Uint8Array): number {
  if (!packet.length) return 960;
  const config = packet[0] >> 3;
  const per =
    config < 12 ? [480, 960, 1920, 2880][config % 4] : config < 16 ? [480, 960][config % 2] : [120, 240, 480, 960][config % 4];
  const c = packet[0] & 3;
  const frames = c === 0 ? 1 : c === 3 ? (packet[1] ?? 1) & 0x3f : 2;
  return per * Math.max(1, frames);
}

/** One Opus packet as a media segment. */
export function opusSegment(seq: number, time: number, packet: Uint8Array): Uint8Array {
  return mediaSegment(seq, time, opusSamples(packet), packet, true);
}
