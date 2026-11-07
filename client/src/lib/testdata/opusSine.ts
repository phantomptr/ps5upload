// Real Opus for tests (#720): a 1 kHz stereo sine, 48 kHz, 10 ms CELT frames as the PS5 sends
// them (TOC 0xf4), 21 packets, one per line in hex. Made with ffmpeg's libopus:
//   ffmpeg -f lavfi -i "sine=frequency=1000:sample_rate=48000:duration=0.2" -ac 2 -c:a libopus \
//     -b:a 64k -frame_duration 10 -application lowdelay -vbr off out.ogg
// and the packets taken out of the Ogg pages (the first two, OpusHead and OpusTags, left out).
import hex from "./opus_sine_1k_stereo_10ms.hex?raw";

export const OPUS_SINE_PACKETS: Uint8Array[] = hex
  .split("\n")
  .filter((l) => l.trim())
  .map((l) => Uint8Array.from(l.trim().match(/../g)!.map((b) => parseInt(b, 16))));
