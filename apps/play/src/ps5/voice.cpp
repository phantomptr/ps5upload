// PS5Upload Play - room voice on the console (voice.hpp).
// SPDX-License-Identifier: GPL-3.0-or-later
#include "voice.hpp"

#include "../app/log.hpp"
#include "../core/jitter.hpp"
#include "../net/net.hpp"
#include "mic.hpp"
#include "sys.hpp"

#include <opus.h>

#include <atomic>
#include <cstdio>
#include <cstring>
#include <pthread.h>

namespace play::ps5::voice_io
{
namespace
{
constexpr int kSpeakers = 4;
constexpr int kOut = 1920;    // up to 40 ms at 48 kHz (each speaker decodes straight to 48 kHz)
constexpr unsigned kRing = 4096;
constexpr std::uint64_t kIdleUs = 500000;    // a speaker silent this long stops being mixed
constexpr std::uint64_t kSpeakingUs = 300000; // "talking" for the indicator

int user_id = -1;
std::atomic<Guest *> guest{nullptr};
pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
voice::Allow allow_now;      // under lock (the strings are static: "off"/"ptt"/"open" or the room's)
char room_mode[8] = {};
std::atomic<int> silent_state{-1};
std::atomic<bool> port_open{false}, sending{false}, headset{false};
std::atomic<unsigned> frames_sent{0};

// ---- playback: one slot per speaker ----
struct Speaker
{
    int id = -1;
    audio::Jitter jitter;
    alignas(16) std::uint8_t dec[20000];
    OpusDecoder *opus = nullptr;
    std::int16_t ring[kRing];
    unsigned head = 0, count = 0;
    std::uint64_t last_us = 0;
    int gain_q8 = 256;
    bool muted = false;
    int last_n = 960; // the last frame's length: silence and concealment follow it
};
Speaker speakers[kSpeakers];
voice::Duck duck_state;
// each person's volume and mute (this PS5 only), kept by speaker id across slots
int vol_by_id[256];
bool mute_by_id[256];
bool vol_ready = false;
std::atomic<int> duck_level{1};

Speaker *slot_for(int id, std::uint64_t now) noexcept
{
    Speaker *free_one = nullptr, *oldest = &speakers[0];
    for (auto &s : speakers)
    {
        if (s.id == id)
            return &s;
        if (s.id < 0 && !free_one)
            free_one = &s;
        if (s.last_us < oldest->last_us)
            oldest = &s;
    }
    Speaker *s = free_one ? free_one : (now - oldest->last_us > kIdleUs ? oldest : nullptr);
    if (!s)
        return nullptr;
    if (!vol_ready)
    {
        for (int i = 0; i < 256; ++i)
            vol_by_id[i] = 256, mute_by_id[i] = false;
        vol_ready = true;
    }
    s->id = id;
    s->jitter.set_depth(2, 5); // 40 ms before playing, at most 100 ms held
    s->jitter.reset();
    s->head = s->count = 0;
    s->gain_q8 = vol_by_id[id & 255];
    s->muted = mute_by_id[id & 255];
    const int size = opus_decoder_get_size(1);
    s->opus = size > 0 && static_cast<std::size_t>(size) <= sizeof s->dec ? reinterpret_cast<OpusDecoder *>(s->dec) : nullptr;
    if (s->opus && opus_decoder_init(s->opus, 48000, 1) != OPUS_OK)
        s->opus = nullptr;
    return s;
}

void decode_into(Speaker &s) noexcept
{
    alignas(16) static std::int16_t pcm[kOut];
    const std::uint8_t *data = nullptr;
    std::size_t len = 0;
    int n = 0;
    switch (s.jitter.next(&data, &len))
    {
    case audio::Jitter::Act::silence:
        std::memset(pcm, 0, sizeof pcm);
        n = s.last_n;
        break;
    case audio::Jitter::Act::play: n = opus_decode(s.opus, data, static_cast<opus_int32>(len), pcm, kOut, 0); break;
    case audio::Jitter::Act::fec: n = opus_decode(s.opus, data, static_cast<opus_int32>(len), pcm, s.last_n, 1); break;
    case audio::Jitter::Act::conceal: n = opus_decode(s.opus, nullptr, 0, pcm, s.last_n, 0); break;
    }
    if (n <= 0)
    {
        std::memset(pcm, 0, sizeof pcm);
        n = s.last_n;
    }
    else
        s.last_n = n;
    for (int i = 0; i < n && s.count < kRing; ++i)
    {
        s.ring[(s.head + s.count) % kRing] = pcm[i];
        ++s.count;
    }
}

// ---- capture ----
void *capture_main(void *) noexcept
{
    alignas(16) static std::uint8_t enc_state[40000];
    OpusEncoder *enc = nullptr;
    const int es = opus_encoder_get_size(1);
    if (es > 0 && static_cast<std::size_t>(es) <= sizeof enc_state)
    {
        enc = reinterpret_cast<OpusEncoder *>(enc_state);
        if (opus_encoder_init(enc, voice::kRate, 1, OPUS_APPLICATION_VOIP) != OPUS_OK)
            enc = nullptr;
    }
    if (enc)
    {
        // wideband speech, 24 kb/s, DTX and in-band FEC (the design's 7A.1b)
        opus_encoder_ctl(enc, OPUS_SET_BITRATE(24000));
        opus_encoder_ctl(enc, OPUS_SET_DTX(1));
        opus_encoder_ctl(enc, OPUS_SET_INBAND_FEC(1));
        opus_encoder_ctl(enc, OPUS_SET_PACKET_LOSS_PERC(5));
        opus_encoder_ctl(enc, OPUS_SET_COMPLEXITY(5));
        opus_encoder_ctl(enc, OPUS_SET_SIGNAL(OPUS_SIGNAL_VOICE));
        opus_encoder_ctl(enc, OPUS_SET_BANDWIDTH(OPUS_BANDWIDTH_WIDEBAND));
    }
    else
        log("voice: the Opus encoder didn't start; voice is listen-only");
    int handle = -1;
    alignas(16) static std::int16_t grain[256];
    std::int16_t frame[voice::kFrameMax];
    int bitrate = 24000;
    int have = 0, reads = 0;
    std::uint16_t seq = 0;
    voice::Gate gate;
    std::uint8_t out[voice::kMaxOpus];
    for (;;)
    {
        beat(Beat::voice, 0);
        pthread_mutex_lock(&lock);
        voice::Allow a = allow_now;
        char mode[8];
        std::memcpy(mode, room_mode, sizeof mode);
        pthread_mutex_unlock(&lock);
        a.room = mode;
        const bool want = a.mic_on && guest.load() && enc;
        if (want && handle < 0)
        {
            if (mic::init() < 0)
                log("voice: sceAudioInInit failed");
            mic::Format f;
            f.type = mic::kVoiceChat;
            f.rate = 16000;
            f.grain = 256;
            handle = mic::open(user_id, f);
            log("voice: mic port %#x", handle);
            if (handle < 0)
            {
                sceKernelUsleep(2000000); // try again later, not in a tight loop
                continue;
            }
            port_open = true;
            have = 0;
            gate.reset();
        }
        if (!want && handle >= 0)
        {
            mic::close(handle);
            handle = -1;
            port_open = false;
            sending = false;
            silent_state = -1;
            log("voice: mic port closed");
        }
        if (handle < 0)
        {
            sceKernelUsleep(50000);
            continue;
        }
        // one grain (16 ms, the call waits for it)
        if (mic::read(handle, grain) < 0)
        {
            sceKernelUsleep(16000);
            continue;
        }
        if (++reads % 30 == 1)
            silent_state = mic::silent(handle); // about twice a second
        a.silent = silent_state.load();
        // the engine's pace: 24 kb/s in 20 ms frames, or 12 kb/s in 40 ms (a Playable route)
        const int fsize = a.frame_ms >= 40 ? voice::kFrameMax : voice::kFrame;
        const int want_rate = a.kbps > 0 ? a.kbps * 1000 : 24000;
        if (want_rate != bitrate)
        {
            bitrate = want_rate;
            opus_encoder_ctl(enc, OPUS_SET_BITRATE(bitrate));
        }
        for (int i = 0; i < 256; ++i)
        {
            frame[have++] = grain[i];
            if (have < fsize)
                continue;
            have = 0;
            const std::uint8_t level = voice::level_of(frame, fsize);
            const bool loud = gate.feed(level, net::now_us());
            // push-to-talk sends while held; open mic only while the gate is open
            const bool open_mode = a.mode == voice::Mode::open && !std::strcmp(a.room, "open");
            const bool go = voice::may_send(a) && (open_mode ? loud : true);
            sending = go;
            if (!go)
                continue;
            const int n = opus_encode(enc, frame, fsize, out, sizeof out);
            Guest *g = guest.load();
            if (n > 0 && g && g->send_voice(seq++, level, out, static_cast<std::size_t>(n)))
                frames_sent.fetch_add(1);
        }
    }
    return nullptr;
}
} // namespace

bool init(int user) noexcept
{
    user_id = user;
    pthread_t t;
    return pthread_create(&t, nullptr, capture_main, nullptr) == 0;
}

void set_guest(Guest *g) noexcept
{
    guest.store(g);
}

void allow(const voice::Allow &a) noexcept
{
    pthread_mutex_lock(&lock);
    allow_now = a;
    std::snprintf(room_mode, sizeof room_mode, "%s", a.room ? a.room : "");
    allow_now.room = "";
    pthread_mutex_unlock(&lock);
}

platform::MicState state() noexcept
{
    platform::MicState m;
    m.open = port_open.load();
    m.silent = silent_state.load();
    m.sending = sending.load();
    m.sent = frames_sent.load();
    m.headset = headset.load();
    return m;
}

void set_headset(bool on) noexcept
{
    headset = on;
}

void packet(std::uint8_t speaker, std::uint16_t seq, const std::uint8_t *opus, std::size_t len) noexcept
{
    const std::uint64_t now = net::now_us();
    pthread_mutex_lock(&lock);
    if (Speaker *s = slot_for(speaker, now))
    {
        if (s->opus && !s->muted)
            s->jitter.insert(seq, opus, len);
        s->last_us = now;
    }
    pthread_mutex_unlock(&lock);
}

void mix(std::int16_t *stereo, unsigned frames) noexcept
{
    const std::uint64_t now = net::now_us();
    pthread_mutex_lock(&lock);
    bool talking = false;
    for (auto &s : speakers)
        talking = talking || (s.id >= 0 && now - s.last_us < kSpeakingUs && !s.muted);
    duck_state.set_level(duck_level.load());
    const int g = duck_state.step(talking, static_cast<int>(frames));
    // the game under the voices
    if (g < 32767)
        for (unsigned i = 0; i < frames * 2; ++i)
            stereo[i] = static_cast<std::int16_t>((static_cast<int>(stereo[i]) * g) >> 15);
    for (auto &s : speakers)
    {
        if (s.id < 0 || !s.opus)
            continue;
        if (now - s.last_us > kIdleUs && !s.count)
            continue; // nobody talking here: no concealment noise
        while (s.count < frames)
            decode_into(s);
        const int gain = s.muted ? 0 : s.gain_q8;
        for (unsigned i = 0; i < frames; ++i)
        {
            const int v = (static_cast<int>(s.ring[s.head]) * gain) >> 8;
            s.head = (s.head + 1) % kRing;
            for (int ch = 0; ch < 2; ++ch)
            {
                int x = stereo[i * 2 + ch] + v;
                stereo[i * 2 + ch] = static_cast<std::int16_t>(x > 32767 ? 32767 : x < -32768 ? -32768 : x);
            }
        }
        s.count -= frames;
    }
    pthread_mutex_unlock(&lock);
}

void stream_end() noexcept
{
    pthread_mutex_lock(&lock);
    for (auto &s : speakers)
        s.id = -1, s.count = 0;
    pthread_mutex_unlock(&lock);
}

void volume(int speaker, int percent, bool muted) noexcept
{
    pthread_mutex_lock(&lock);
    if (!vol_ready)
    {
        for (int i = 0; i < 256; ++i)
            vol_by_id[i] = 256, mute_by_id[i] = false;
        vol_ready = true;
    }
    vol_by_id[speaker & 255] = percent * 256 / 100;
    mute_by_id[speaker & 255] = muted;
    for (auto &s : speakers)
        if (s.id == speaker)
        {
            s.gain_q8 = vol_by_id[speaker & 255];
            s.muted = muted;
        }
    pthread_mutex_unlock(&lock);
}

void duck(int level) noexcept
{
    duck_level = level;
}

std::uint32_t speaking() noexcept
{
    const std::uint64_t now = net::now_us();
    std::uint32_t bits = 0;
    pthread_mutex_lock(&lock);
    for (const auto &s : speakers)
        if (s.id >= 0 && s.id < 32 && now - s.last_us < kSpeakingUs && !s.muted)
            bits |= 1u << s.id;
    pthread_mutex_unlock(&lock);
    return bits;
}
} // namespace play::ps5::voice_io
