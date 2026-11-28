#!/usr/bin/env python3
# PS5Upload Play - menu music: an original composition for this project,
# synthesised from scratch (no samples, no third-party material).
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Usage: make-music.py OUTDIR
#
# Writes OUTDIR/music.wav and OUTDIR/music.pcm (raw s16le, 48 kHz stereo):
# a calm 30 s loop, exactly 1,440,000 frames, peak -12 dBFS.
#
# 64 BPM, 8 bars, D major: Dmaj9 | Bm9 | Gmaj9 | A(add9), two bars each.
# Soft detuned pad, a quiet plucked arpeggio with a ping-pong delay, a sine
# bass and a long reverb. Everything is mixed into a circular buffer: note
# tails past the end wrap to the start, and the delay and reverb are
# circular, so the loop point is seamless. The only noise (reverb impulse)
# comes from a fixed seed, so the output is identical on every run.
import sys
import wave
from pathlib import Path

import numpy as np

RATE = 48_000
FRAMES = 1_440_000            # 30 s
BEAT = 45_000                 # 64 BPM
BAR = 4 * BEAT
EIGHTH = BEAT // 2
PEAK_DBFS = -12.0

# (pad voicing, bass, arpeggio notes) as MIDI numbers, two bars each
CHORDS = [
    ([50, 57, 61, 64, 66], 38, [62, 66, 69, 73, 76]),  # Dmaj9
    ([47, 54, 57, 61, 62], 35, [59, 62, 66, 69, 73]),  # Bm9
    ([43, 50, 54, 57, 59], 31, [55, 59, 62, 66, 69]),  # Gmaj9
    ([45, 52, 57, 59, 61], 33, [57, 64, 69, 71, 73]),  # A(add9)
]
# arpeggio over 16 eighths: index into the chord's arpeggio notes, None = rest
PATTERN = [0, None, 2, 4, None, 3, None, 2, 1, None, 2, 4, None, 3, None, None]


def hz(midi):
    return 440.0 * 2.0 ** ((midi - 69) / 12.0)


def envelope(n, attack, sustain, release):
    """Raised-cosine attack, hold until `sustain` samples, raised-cosine release."""
    env = np.ones(n)
    a = np.arange(attack)
    env[:attack] = 0.5 - 0.5 * np.cos(np.pi * a / attack)
    r = np.arange(n - sustain)
    env[sustain:] *= 0.5 + 0.5 * np.cos(np.pi * r / release)
    return env


def add_wrapped(buf, start, seg):
    """Add a (2, n) segment into the circular (2, FRAMES) buffer."""
    start %= FRAMES
    first = min(seg.shape[1], FRAMES - start)
    buf[:, start:start + first] += seg[:, :first]
    if first < seg.shape[1]:
        buf[:, :seg.shape[1] - first] += seg[:, first:]


def pad_note(freq, length, release):
    n = length + release
    t = np.arange(n) / RATE
    out = np.zeros((2, n))
    # three detuned voices: left, centre, right
    for cents, pan in ((-7, 0.85), (0, 0.5), (7, 0.15)):
        f = freq * 2 ** (cents / 1200)
        wave_ = sum(np.sin(2 * np.pi * f * k * t + 0.7 * k) / (k * k) for k in range(1, 7))
        out[0] += wave_ * np.sqrt(pan)
        out[1] += wave_ * np.sqrt(1 - pan)
    swell = 1.0 + 0.12 * np.sin(2 * np.pi * 0.11 * t + freq)  # slow breathing
    return out * swell * envelope(n, int(2.0 * RATE), length, release)


def pluck_note(freq, pan, velocity):
    n = int(2.5 * RATE)
    t = np.arange(n) / RATE
    tone = sum(0.55 ** (k - 1) * np.sin(2 * np.pi * freq * k * t) * np.exp(-t * (1.6 + 1.4 * k))
               for k in range(1, 6))
    tone *= np.minimum(1.0, t / 0.004) * velocity
    return np.stack([tone * np.sqrt(pan), tone * np.sqrt(1 - pan)])


def bass_note(freq, length, release):
    n = length + release
    t = np.arange(n) / RATE
    tone = np.sin(2 * np.pi * freq * t) + 0.15 * np.sin(4 * np.pi * freq * t)
    return np.stack([tone, tone]) * envelope(n, int(0.6 * RATE), length, release)


def spectrum_filter(x, gain_fn):
    """Circular (loop-safe) linear filter applied in the frequency domain."""
    f = np.fft.rfftfreq(FRAMES, 1 / RATE)
    return np.fft.irfft(np.fft.rfft(x, axis=1) * gain_fn(f), n=FRAMES, axis=1)


def ping_pong(x, delay, feedback):
    """Circular cross-feedback delay: y_L = x_L + g*y_R(t-d), and vice versa."""
    y = x.copy()
    for _ in range(24):  # feedback**24 is negligible
        y = x + feedback * np.roll(y[::-1], delay, axis=1)
    return y - x


def reverb(x, seconds=3.2, predelay=0.025):
    """Circular convolution with a fixed-seed decaying-noise impulse response."""
    rng = np.random.default_rng(20261127)
    n = int(6 * RATE)
    t = np.arange(n) / RATE
    decay = np.exp(-6.9 * t / seconds) * np.minimum(1.0, t / 0.08)
    ir = np.zeros((2, FRAMES))
    start = int(predelay * RATE)
    ir[:, start:start + n] = rng.standard_normal((2, n)) * decay
    ir /= np.sqrt(np.sum(ir ** 2, axis=1, keepdims=True))
    wet = np.fft.irfft(np.fft.rfft(x, axis=1) * np.fft.rfft(ir, axis=1), n=FRAMES, axis=1)
    return spectrum_filter(wet, lambda f: 1 / np.sqrt(1 + (f / 3500) ** 2))  # darker tail


def main():
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    chord_len = 2 * BAR
    pad = np.zeros((2, FRAMES))
    pluck = np.zeros((2, FRAMES))
    bass = np.zeros((2, FRAMES))
    for i, (voicing, root, arp) in enumerate(CHORDS):
        start = i * chord_len
        for m in voicing:
            add_wrapped(pad, start, pad_note(hz(m), chord_len, int(3.0 * RATE)))
        add_wrapped(bass, start, bass_note(hz(root), chord_len, int(1.5 * RATE)))
        for step, idx in enumerate(PATTERN):
            if idx is None:
                continue
            pan = 0.35 + 0.3 * (step % 2)
            velocity = 0.9 if step % 4 == 0 else 0.7
            add_wrapped(pluck, start + step * EIGHTH, pluck_note(hz(arp[idx]), pan, velocity))

    def norm(x):
        return x / np.sqrt(np.mean(x ** 2))

    pad, pluck, bass = norm(pad), norm(pluck), norm(bass)
    pluck = pluck + 0.45 * ping_pong(pluck, 3 * EIGHTH // 2, 0.42)
    dry = pad + 0.32 * pluck + 0.30 * bass
    mix = dry + 0.55 * reverb(pad + 0.6 * pluck)
    # gentle master EQ: no DC/sub rumble, soft top
    mix = spectrum_filter(mix, lambda f: (f / np.sqrt(f * f + 35.0 ** 2)) / np.sqrt(1 + (f / 7000) ** 4))
    mix *= 10 ** (PEAK_DBFS / 20) / np.max(np.abs(mix))
    pcm = np.clip(np.round(mix.T * 32767), -32768, 32767).astype("<i2")
    assert pcm.shape == (FRAMES, 2)

    (out / "music.pcm").write_bytes(pcm.tobytes())
    with wave.open(str(out / "music.wav"), "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(pcm.tobytes())


if __name__ == "__main__":
    main()
