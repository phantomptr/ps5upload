// The stream's codec helpers, kept free of the app's state so the phone's watch page
// (pad/watchPlayer.ts, built into the engine's pad page) can use them too.

/** True when an Annex-B access unit starts a picture the decoder can begin at. */
export function isKeyframe(au: Uint8Array, hevc: boolean): boolean {
  for (let i = 0; i + 3 < au.length; i++) {
    if (au[i] !== 0 || au[i + 1] !== 0 || au[i + 2] !== 1) continue;
    if (hevc) {
      const t = (au[i + 3] >> 1) & 0x3f;
      if (t >= 16 && t <= 21) return true; // IRAP: BLA, IDR, CRA
      if (t < 16) return false;
    } else {
      const t = au[i + 3] & 0x1f;
      if (t === 5) return true;
      if (t === 1) return false;
    }
  }
  return false;
}

/** `avc1.PPCCLL` from the SPS in an Annex-B header. */
export function avcCodecString(header: Uint8Array): string {
  for (let i = 0; i + 6 < header.length; i++) {
    if (header[i] === 0 && header[i + 1] === 0 && header[i + 2] === 1 && (header[i + 3] & 0x1f) === 7) {
      const h = (b: number) => b.toString(16).padStart(2, "0");
      return `avc1.${h(header[i + 4])}${h(header[i + 5])}${h(header[i + 6])}`;
    }
  }
  return "avc1.4d4028";
}

const HEVC_CANDIDATES = ["hev1.1.6.L153.B0", "hvc1.1.6.L153.B0", "hev1.2.4.L153.B0", "hvc1.2.4.L153.B0", "hev1.1.6.L150.90"];

/** The first HEVC codec string this device decodes, or null. */
export async function supportedHevc(width: number, height: number): Promise<string | null> {
  if (typeof VideoDecoder === "undefined") return null;
  for (const codec of HEVC_CANDIDATES) {
    try {
      if ((await VideoDecoder.isConfigSupported({ codec, codedWidth: width, codedHeight: height })).supported) {
        return codec;
      }
    } catch {
      // Not this one.
    }
  }
  return null;
}
