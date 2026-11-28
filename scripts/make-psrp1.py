#!/usr/bin/env python3
"""Make a PSRP1 recording (instant replay's form, engine rp_replay.rs) from any clip, for the
replayed Play Together host a test engine serves (PS5UPLOAD_DEV_REPLAY_HOST, rp_dev_replay.rs).

  make-psrp1.py --video clip.hevc [--fps 60] [--opus sound.ogg] --out clip.psrp1

--video: an Annex-B H.264 or HEVC elementary stream with an access unit delimiter before every
picture and no B-frames, as Remote Play sends (x265: -x265-params aud=1:bframes=0; x264:
-x264-params aud=1 -bf 0). --opus: an Ogg Opus file of 10 ms frames at 48 kHz stereo
(-c:a libopus -frame_duration 10 -ac 2 -ar 48000). Example, a 20 s high-motion 1080p60 HEVC clip:

  ffmpeg -f lavfi -i "testsrc2=s=1920x1080:r=60,scroll=h=0.004:v=0.002,noise=alls=12:allf=t" \\
    -f lavfi -i "sine=f=440:r=48000:d=20" -t 20 -map 0:v -c:v libx265 -preset fast -b:v 25M \\
    -x265-params keyint=60:min-keyint=60:bframes=0:aud=1:repeat-headers=1 -pix_fmt yuv420p \\
    -f hevc clip.hevc -map 1:a -c:a libopus -frame_duration 10 -ac 2 -b:a 128k sound.ogg
"""
import argparse
import struct
import sys

KIND_VIDEO_HEADER, KIND_VIDEO, KIND_AUDIO_HEADER, KIND_AUDIO = 0, 1, 3, 4


def nal_units(b):
    """(start of the start code, start of the NAL) for each NAL unit in an Annex-B stream."""
    out = []
    i = 0
    n = len(b)
    while i + 3 <= n:
        if b[i] == 0 and b[i + 1] == 0 and b[i + 2] == 1:
            s = i - 1 if i > 0 and b[i - 1] == 0 else i
            out.append((s, i + 3))
            i += 3
        else:
            i += 1
    return out


def nal_type(b, at, hevc):
    return (b[at] >> 1) & 0x3F if hevc else b[at] & 0x1F


def access_units(b):
    """Splits at access unit delimiters: [(au bytes, keyframe, parameter sets)], hevc."""
    nals = nal_units(b)
    if not nals:
        sys.exit("no NAL units in the video file")
    first = b[nals[0][1]]
    hevc = ((first >> 1) & 0x3F) in (32, 33, 34, 35)
    aud = 35 if hevc else 9
    params = (32, 33, 34) if hevc else (7, 8)
    keys = range(16, 22) if hevc else (5,)
    aus = []
    cur = None
    for k, (s, at) in enumerate(nals):
        end = nals[k + 1][0] if k + 1 < len(nals) else len(b)
        t = nal_type(b, at, hevc)
        if t == aud or cur is None:
            if cur is not None:
                aus.append(cur)
            cur = {"start": s, "end": end, "key": False, "params": []}
        cur["end"] = end
        if t in keys:
            cur["key"] = True
        if t in params:
            cur["params"].append(bytes(b[at:end]))
    aus.append(cur)
    out = [(bytes(b[a["start"]:a["end"]]), a["key"], a["params"]) for a in aus]
    if sum(1 for a in out if a[1]) == 0:
        sys.exit("no keyframe found (is there an AUD before every picture?)")
    return out, hevc


def ogg_packets(b):
    """The packets of the first logical stream of an Ogg file."""
    pkts, cur, i = [], b"", 0
    while i + 27 <= len(b):
        if b[i:i + 4] != b"OggS":
            sys.exit("not an Ogg file")
        nseg = b[i + 26]
        lace = b[i + 27:i + 27 + nseg]
        i += 27 + nseg
        for v in lace:
            cur += b[i:i + v]
            i += v
            if v < 255:
                pkts.append(cur)
                cur = b""
    return pkts


def record(kind, t_us, data, key=None):
    r = struct.pack(">BQI", kind, t_us, len(data))
    if key is not None:
        r += bytes([1 if key else 0])
    return r + data


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--video", required=True)
    ap.add_argument("--fps", type=float, default=60.0)
    ap.add_argument("--opus")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    with open(a.video, "rb") as f:
        aus, hevc = access_units(f.read())
    header = b"".join(b"\x00\x00\x00\x01" + p for p in next(au for au in aus if au[1])[2])
    if not header:
        sys.exit("the first keyframe carries no parameter sets (x265: repeat-headers=1)")
    frames = [(round(i * 1e6 / a.fps), KIND_VIDEO, au, key) for i, (au, key, _) in enumerate(aus)]
    audio_header = None
    if a.opus:
        with open(a.opus, "rb") as f:
            pkts = ogg_packets(f.read())
        if len(pkts) < 3 or not pkts[0].startswith(b"OpusHead"):
            sys.exit("not an Ogg Opus file")
        # channels, bits, rate, samples per frame, as the console's stream info says them
        audio_header = struct.pack(">BBIII", 2, 16, 48000, 480, 0)
        for k, p in enumerate(pkts[2:]):
            frames.append((k * 10000, KIND_AUDIO, struct.pack(">H", k & 0xFFFF) + p, None))
    frames.sort(key=lambda f: (f[0], f[1]))
    with open(a.out, "wb") as f:
        f.write(b"PSRP1")
        f.write(record(KIND_VIDEO_HEADER, 0, header))
        if audio_header:
            f.write(record(KIND_AUDIO_HEADER, 0, audio_header))
        for t, kind, data, key in frames:
            f.write(record(kind, t, data, key if kind == KIND_VIDEO else None))
    keys = sum(1 for au in aus if au[1])
    print(f"{a.out}: {'HEVC' if hevc else 'H.264'}, {len(aus)} pictures ({keys} keyframes), "
          f"{sum(1 for f in frames if f[1] == KIND_AUDIO)} sound frames")


if __name__ == "__main__":
    main()
