// The picture's visible size, read from the stream's own sequence parameter set (SPS).
//
// Why: Microsoft Edge's hardware HEVC decoder on Windows (so the Windows app's WebView2) hands
// out frames whose visibleRect and display size are smaller than the picture (1920×1080 coming
// out as e.g. 1280×720), while the coded size is right. drawImage draws only the visibleRect,
// so the top-left part of the picture was shown enlarged and cut off, in every picture size.
// Chrome reports the same stream correctly. The SPS says what the visible picture really is, so
// the frame can be rebuilt with it (see visibleFix).

/** The visible part of the coded picture, in luma samples. */
export interface VisibleRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** The NAL payload without its emulation-prevention bytes (00 00 03 → 00 00). */
function unescape(nal: Uint8Array): Uint8Array {
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

/** The NAL units of an Annex-B stream. */
function nals(stream: Uint8Array): Uint8Array[] {
  const starts: number[] = [];
  for (let i = 0; i + 2 < stream.length; i++) {
    if (stream[i] === 0 && stream[i + 1] === 0 && stream[i + 2] === 1) {
      starts.push(i + 3);
      i += 2;
    }
  }
  return starts.map((s, k) => {
    let end = k + 1 < starts.length ? starts[k + 1] - 3 : stream.length;
    // A four-byte start code leaves a zero at the end of the NAL before it.
    while (end > s && stream[end - 1] === 0) end--;
    return stream.subarray(s, end);
  });
}

class Bits {
  private pos = 0;
  constructor(private readonly b: Uint8Array) {}
  u(n: number): number {
    let v = 0;
    for (let i = 0; i < n; i++) {
      const byte = this.b[this.pos >> 3];
      if (byte === undefined) throw new Error("SPS ended early");
      v = v * 2 + ((byte >> (7 - (this.pos & 7))) & 1);
      this.pos++;
    }
    return v;
  }
  skip(n: number): void {
    this.pos += n;
    if (this.pos > this.b.length * 8) throw new Error("SPS ended early");
  }
  ue(): number {
    let zeros = 0;
    while (this.u(1) === 0) {
      if (++zeros > 31) throw new Error("bad Exp-Golomb code");
    }
    return 2 ** zeros - 1 + this.u(zeros);
  }
  se(): number {
    const k = this.ue();
    return k % 2 ? (k + 1) / 2 : -k / 2;
  }
}

function hevcSps(rbsp: Uint8Array): VisibleRect {
  const r = new Bits(rbsp);
  r.skip(16); // NAL header
  r.skip(4); // sps_video_parameter_set_id
  const maxSubLayersMinus1 = r.u(3);
  r.skip(1); // sps_temporal_id_nesting_flag
  // profile_tier_level(1, maxSubLayersMinus1)
  r.skip(96); // general profile (88 bits) + general_level_idc
  const profile: boolean[] = [];
  const level: boolean[] = [];
  for (let i = 0; i < maxSubLayersMinus1; i++) {
    profile.push(r.u(1) === 1);
    level.push(r.u(1) === 1);
  }
  if (maxSubLayersMinus1 > 0) for (let i = maxSubLayersMinus1; i < 8; i++) r.skip(2);
  for (let i = 0; i < maxSubLayersMinus1; i++) {
    if (profile[i]) r.skip(88);
    if (level[i]) r.skip(8);
  }
  r.ue(); // sps_seq_parameter_set_id
  const chroma = r.ue();
  const separate = chroma === 3 ? r.u(1) === 1 : false;
  const w = r.ue();
  const h = r.ue();
  const subW = !separate && (chroma === 1 || chroma === 2) ? 2 : 1;
  const subH = !separate && chroma === 1 ? 2 : 1;
  if (r.u(1) === 0) return { x: 0, y: 0, w, h };
  const left = r.ue();
  const right = r.ue();
  const top = r.ue();
  const bottom = r.ue();
  return { x: subW * left, y: subH * top, w: w - subW * (left + right), h: h - subH * (top + bottom) };
}

const AVC_HIGH_PROFILES = new Set([100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135]);

function avcSps(rbsp: Uint8Array): VisibleRect {
  const r = new Bits(rbsp);
  r.skip(8); // NAL header
  const profileIdc = r.u(8);
  r.skip(16); // constraint flags, level_idc
  r.ue(); // seq_parameter_set_id
  let chroma = 1;
  let separate = false;
  if (AVC_HIGH_PROFILES.has(profileIdc)) {
    chroma = r.ue();
    if (chroma === 3) separate = r.u(1) === 1;
    r.ue(); // bit_depth_luma_minus8
    r.ue(); // bit_depth_chroma_minus8
    r.skip(1); // qpprime_y_zero_transform_bypass_flag
    if (r.u(1) === 1) {
      for (let i = 0; i < (chroma !== 3 ? 8 : 12); i++) {
        if (r.u(1) === 0) continue;
        let last = 8;
        let next = 8;
        for (let j = 0; j < (i < 6 ? 16 : 64); j++) {
          if (next !== 0) next = (last + r.se() + 256) % 256;
          last = next === 0 ? last : next;
        }
      }
    }
  }
  r.ue(); // log2_max_frame_num_minus4
  const pocType = r.ue();
  if (pocType === 0) r.ue();
  else if (pocType === 1) {
    r.skip(1);
    r.se();
    r.se();
    const n = r.ue();
    for (let i = 0; i < n; i++) r.se();
  }
  r.ue(); // max_num_ref_frames
  r.skip(1); // gaps_in_frame_num_value_allowed_flag
  const mbsW = r.ue() + 1;
  const mapH = r.ue() + 1;
  const frameMbsOnly = r.u(1);
  if (!frameMbsOnly) r.skip(1);
  r.skip(1); // direct_8x8_inference_flag
  const w = mbsW * 16;
  const h = (2 - frameMbsOnly) * mapH * 16;
  if (r.u(1) === 0) return { x: 0, y: 0, w, h };
  const arrayType = separate ? 0 : chroma;
  const cropX = arrayType === 0 ? 1 : arrayType === 3 ? 1 : 2;
  const cropY = (arrayType === 0 ? 1 : arrayType === 1 ? 2 : 1) * (2 - frameMbsOnly);
  const left = r.ue();
  const right = r.ue();
  const top = r.ue();
  const bottom = r.ue();
  return { x: cropX * left, y: cropY * top, w: w - cropX * (left + right), h: h - cropY * (top + bottom) };
}

/** The visible picture an SPS in this video header (Annex-B parameter sets) describes, or null
 *  when there is none or it cannot be read. */
export function spsVisibleRect(header: Uint8Array, hevc: boolean): VisibleRect | null {
  for (const nal of nals(header)) {
    if (nal.length < 2) continue;
    const isSps = hevc ? ((nal[0] >> 1) & 0x3f) === 33 : (nal[0] & 0x1f) === 7;
    if (!isSps) continue;
    try {
      const v = (hevc ? hevcSps : avcSps)(unescape(nal));
      return v.w > 0 && v.h > 0 && v.x >= 0 && v.y >= 0 ? v : null;
    } catch {
      return null;
    }
  }
  return null;
}

/** The visibleRect a frame should have instead of its own, or null to leave it as it is: only
 *  when its own is clearly smaller than the stream's (by more than a 16-pixel macroblock) and
 *  the stream's fits in the frame's coded size. A correctly reported frame is never touched. */
export function visibleFix(
  frame: { codedWidth: number; codedHeight: number; visibleRect: { width: number; height: number } | null },
  sps: VisibleRect | null,
): VisibleRect | null {
  const v = frame.visibleRect;
  if (!sps || !v) return null;
  if (v.width >= sps.w - 16 && v.height >= sps.h - 16) return null;
  if (sps.x + sps.w > frame.codedWidth || sps.y + sps.h > frame.codedHeight) return null;
  return sps;
}
