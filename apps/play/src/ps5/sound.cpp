// PS5Upload Play - sound.
// Phase 0: sceAudioOutOpen(main, 256 frames, 48 kHz, S16 stereo) consumes blocks at exactly real
// time; the first three calls return at once (about 16 ms queued), then each blocks for its block.
// The engine sends 10 ms Opus frames (480 samples); they are decoded into a ring and taken out
// 256 frames at a time.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "sound.hpp"

#include "../app/log.hpp"
#include "../core/jitter.hpp"
#include "sys.hpp"

#include <opus.h>

#include <atomic>
#include <cstring>
#include <pthread.h>

namespace play::ps5::sound
{
namespace
{
constexpr unsigned kBlock = 256, kOpusFrame = 480;
int port = -1;
pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
audio::Jitter jitter;
audio::PcmRing ring;
alignas(16) std::uint8_t opus_state[32768];
OpusDecoder *opus = nullptr;
bool streaming = false;
Stats st;
const std::int16_t *music_pcm = nullptr;
std::size_t music_frames = 0, music_pos = 0;
std::atomic<bool> music_on{false}, sounds_on{true};
std::atomic<int> music_vol{25};
float music_gain = 0; // follows the target slowly: no clicks
// one button sound at a time
std::atomic<int> blip_kind{-1};
std::atomic<unsigned> blip_serial{0};

bool opus_reset() noexcept
{
    const int size = opus_decoder_get_size(2);
    if (size <= 0 || static_cast<std::size_t>(size) > sizeof opus_state)
        return false;
    opus = reinterpret_cast<OpusDecoder *>(opus_state);
    return opus_decoder_init(opus, 48000, 2) == OPUS_OK;
}

// Fills the ring with the next 10 ms of the game's sound.
void decode_next() noexcept
{
    alignas(16) static std::int16_t pcm[kOpusFrame * 2];
    const std::uint8_t *data = nullptr;
    std::size_t len = 0;
    const auto act = jitter.next(&data, &len);
    int n = 0;
    switch (act)
    {
    case audio::Jitter::Act::silence:
        std::memset(pcm, 0, sizeof pcm);
        n = kOpusFrame;
        break;
    case audio::Jitter::Act::play:
        n = opus_decode(opus, data, static_cast<opus_int32>(len), pcm, kOpusFrame, 0);
        break;
    case audio::Jitter::Act::fec:
        n = opus_decode(opus, data, static_cast<opus_int32>(len), pcm, kOpusFrame, 1);
        break;
    case audio::Jitter::Act::conceal:
        n = opus_decode(opus, nullptr, 0, pcm, kOpusFrame, 0);
        break;
    }
    if (n < 0)
    {
        if (st.decode_errors++ < 5)
            log("sound: opus_decode %d", n);
        std::memset(pcm, 0, sizeof pcm);
        n = kOpusFrame;
    }
    else if (act != audio::Jitter::Act::silence)
        ++st.decoded;
    ring.push(pcm, static_cast<std::size_t>(n));
}

// A short tone shaped by a falling envelope (a triangle wave: no maths library needed).
struct Tone
{
    int kind = -1;
    unsigned pos = 0, len = 0;
    float phase = 0;
};
Tone tone;

float tone_sample(Tone &t) noexcept
{
    if (t.kind < 0 || t.pos >= t.len)
        return 0;
    const float u = static_cast<float>(t.pos) / static_cast<float>(t.len);
    float f;
    switch (t.kind)
    {
    case 0: f = 1320; break;                    // move: a tick
    case 1: f = u < 0.45f ? 660.0f : 990.0f; break; // choose: up
    case 2: f = u < 0.45f ? 660.0f : 440.0f; break; // back: down
    default: f = 196; break;                     // bad: low
    }
    t.phase += f / 48000.0f;
    if (t.phase >= 1)
        t.phase -= 1;
    const float tri = t.phase < 0.5f ? t.phase * 4 - 1 : 3 - t.phase * 4;
    const float env = (1 - u) * (1 - u) * (u < 0.02f ? u / 0.02f : 1);
    ++t.pos;
    return tri * env * 0.09f;
}

void *thread_main(void *) noexcept
{
    alignas(16) static std::int16_t block[kBlock * 2];
    unsigned seen_blip = 0;
    for (;;)
    {
        pthread_mutex_lock(&lock);
        const bool stream = streaming;
        if (stream)
        {
            while (ring.frames() < kBlock)
                decode_next();
            ring.pop(block, kBlock);
        }
        pthread_mutex_unlock(&lock);
        if (!stream)
        {
            const unsigned serial = blip_serial.load();
            if (serial != seen_blip)
            {
                seen_blip = serial;
                tone = Tone{};
                tone.kind = blip_kind.load();
                tone.len = tone.kind == 0 ? 1200 : tone.kind == 3 ? 4800 : 3600;
            }
            const int v = music_vol.load();
            const float target = music_on.load() && music_pcm ? static_cast<float>(v * v) / 10000.0f : 0.0f;
            for (unsigned i = 0; i < kBlock; ++i)
            {
                music_gain += (target - music_gain) * 0.0005f;
                float l = 0, r = 0;
                if (music_pcm && music_gain > 0.0001f)
                {
                    l = static_cast<float>(music_pcm[music_pos * 2]) * music_gain;
                    r = static_cast<float>(music_pcm[music_pos * 2 + 1]) * music_gain;
                    music_pos = (music_pos + 1) % music_frames;
                }
                const float b = sounds_on.load() ? tone_sample(tone) * 32767.0f : 0.0f;
                l += b;
                r += b;
                block[i * 2] = static_cast<std::int16_t>(l > 32767 ? 32767 : l < -32768 ? -32768 : l);
                block[i * 2 + 1] = static_cast<std::int16_t>(r > 32767 ? 32767 : r < -32768 ? -32768 : r);
            }
        }
        const int rc = sceAudioOutOutput(port, block);
        if (rc < 0)
        {
            static int errors;
            if (errors++ < 5)
                log("sound: sceAudioOutOutput %#x", rc);
            sceKernelUsleep(5000);
        }
    }
    return nullptr;
}
} // namespace

bool init(const std::uint8_t *pcm, std::size_t bytes) noexcept
{
    log("sound: sceAudioOutInit %#x", sceAudioOutInit());
    port = sceAudioOutOpen(0xff, 0, 0, kBlock, 48000, 1 /* S16 stereo */);
    log("sound: sceAudioOutOpen %#x", port);
    if (port < 0)
        return false;
    if (!opus_reset())
    {
        log("sound: opus decoder setup failed");
        return false;
    }
    jitter.set_depth(4, 14); // 40 ms before playing, at most 140 ms held
    if (pcm && bytes >= 4)
    {
        music_pcm = reinterpret_cast<const std::int16_t *>(pcm);
        music_frames = bytes / 4;
    }
    pthread_t t;
    return pthread_create(&t, nullptr, thread_main, nullptr) == 0;
}

void stream_begin() noexcept
{
    pthread_mutex_lock(&lock);
    jitter.reset();
    ring.clear();
    opus_reset();
    streaming = true;
    pthread_mutex_unlock(&lock);
}

void stream_end() noexcept
{
    pthread_mutex_lock(&lock);
    streaming = false;
    ring.clear();
    pthread_mutex_unlock(&lock);
}

void stream_header(const std::uint8_t *d, std::size_t n) noexcept
{
    // 2 channels, 16 bits, 48000 Hz, 480-sample frames is what the console sends; say so if not.
    if (n >= 10)
    {
        const unsigned rate = static_cast<unsigned>(d[2]) << 24 | d[3] << 16 | d[4] << 8 | d[5];
        const unsigned frame = static_cast<unsigned>(d[6]) << 24 | d[7] << 16 | d[8] << 8 | d[9];
        if (d[0] != 2 || rate != 48000 || frame != kOpusFrame)
            log("sound: header says %u channels, %u Hz, %u-sample frames", d[0], rate, frame);
    }
}

void stream_packet(std::uint16_t index, const std::uint8_t *d, std::size_t n) noexcept
{
    pthread_mutex_lock(&lock);
    if (streaming)
        jitter.insert(index, d, n);
    pthread_mutex_unlock(&lock);
}

void music(bool on, int volume) noexcept
{
    music_on = on;
    music_vol = volume < 0 ? 0 : volume > 100 ? 100 : volume;
}

void sounds(bool on) noexcept
{
    sounds_on = on;
}

void blip(Blip b) noexcept
{
    blip_kind = static_cast<int>(b);
    blip_serial.fetch_add(1);
}

Stats stats() noexcept
{
    pthread_mutex_lock(&lock);
    Stats s = st;
    const auto &j = jitter.stats();
    s.concealed = static_cast<unsigned>(j.concealed);
    s.recovered = static_cast<unsigned>(j.recovered);
    s.underruns = static_cast<unsigned>(j.underruns);
    s.dropped = static_cast<unsigned>(j.dropped);
    s.late = static_cast<unsigned>(j.late);
    pthread_mutex_unlock(&lock);
    return s;
}
} // namespace play::ps5::sound
