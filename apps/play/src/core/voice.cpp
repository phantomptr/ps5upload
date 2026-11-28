// PS5Upload Play - room voice: frames, the gate, push-to-talk, the mixer's settings.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "voice.hpp"

#include <cstdio>
#include <cstring>

namespace play::voice
{
std::size_t up_frame(std::uint8_t *out, std::size_t cap, std::uint16_t seq, std::uint8_t level,
                     const std::uint8_t *opus, std::size_t len) noexcept
{
    if (!opus || !len || len > static_cast<std::size_t>(kMaxOpus) || cap < 4 + len)
        return 0;
    out[0] = kUp;
    out[1] = static_cast<std::uint8_t>(seq >> 8);
    out[2] = static_cast<std::uint8_t>(seq);
    out[3] = level;
    std::memcpy(out + 4, opus, len);
    return 4 + len;
}

int parse_down(const std::uint8_t *d, std::size_t n, Item *out, int cap) noexcept
{
    if (n < 2 || d[0] != kDown)
        return -1;
    const int count = d[1];
    std::size_t at = 2;
    int k = 0;
    for (int i = 0; i < count; ++i)
    {
        if (at + 4 > n)
            return -1;
        Item it;
        it.speaker = d[at];
        it.seq = static_cast<std::uint16_t>(d[at + 1] << 8 | d[at + 2]);
        it.len = d[at + 3];
        at += 4;
        if (!it.len || at + it.len > n)
            return -1;
        it.opus = d + at;
        at += it.len;
        if (k < cap)
            out[k++] = it;
    }
    return at == n ? k : -1;
}

std::size_t mic_message(char *out, std::size_t cap, const char *state) noexcept
{
    const int n = std::snprintf(out, cap, "{\"mic\":\"%s\"}", state);
    return n > 0 && static_cast<std::size_t>(n) < cap ? static_cast<std::size_t>(n) : 0;
}

std::uint8_t level_of(const std::int16_t *pcm, int n) noexcept
{
    if (!pcm || n <= 0)
        return 0;
    std::uint64_t sum = 0;
    for (int i = 0; i < n; ++i)
        sum += static_cast<std::uint64_t>(static_cast<std::int64_t>(pcm[i]) * pcm[i]);
    const std::uint64_t ms = sum / static_cast<std::uint64_t>(n); // up to 2^30
    if (!ms)
        return 0;
    // log2 with a linear fraction from the next bits, then dB = 3.0103 * (log2(ms) - 30)
    int bit = 63;
    while (!(ms >> bit))
        --bit;
    const double frac = bit >= 8 ? static_cast<double>((ms >> (bit - 8)) & 0xff) / 256.0
                                 : static_cast<double>((ms << (8 - bit)) & 0xff) / 256.0;
    const double db = 3.0103 * (static_cast<double>(bit) + frac - 30.0);
    const double v = (db + 60.0) * 255.0 / 60.0;
    return static_cast<std::uint8_t>(v < 0 ? 0 : v > 255 ? 255 : v);
}

std::uint8_t Gate::threshold() const noexcept
{
    // a little above the room's floor, never below -45 dBFS (63)
    const int t = noise_ + 26;
    return static_cast<std::uint8_t>(t < 63 ? 63 : t > 200 ? 200 : t);
}

bool Gate::feed(std::uint8_t level, std::uint64_t now) noexcept
{
    // the floor follows quiet frames quickly, loud ones very slowly
    if (level < noise_)
        noise_ = (noise_ * 7 + level) / 8;
    else
        noise_ = (noise_ * 255 + level) / 256;
    if (level >= threshold())
        open_until_ = now + kHangoverUs;
    return open_until_ && now < open_until_;
}

const char *why_not(const Allow &a) noexcept
{
    if (!a.mic_on)
        return "off";
    if (!a.room || !*a.room || !std::strcmp(a.room, "off"))
        return "room_off";
    if (a.host_muted)
        return "muted";
    if (a.silent < 0 || (a.silent & 0x1))
        return "no_mic";
    if (a.silent)
        return "mic_muted";
    if (!a.in_front)
        return "behind";
    if (a.paused)
        return "paused";
    // push-to-talk in a PTT-only room or by choice; open mic only where the room allows it
    const bool open = a.mode == Mode::open && !std::strcmp(a.room, "open");
    if (!open && !a.talk_held)
        return "hold";
    return "";
}

bool may_send(const Allow &a) noexcept
{
    return !*why_not(a);
}

Mixer::P *Mixer::slot(int id) noexcept
{
    for (auto &p : p_)
        if (p.id == id)
            return &p;
    for (auto &p : p_)
        if (p.id < 0)
        {
            p = P{};
            p.id = id;
            return &p;
        }
    return nullptr;
}

const Mixer::P *Mixer::find(int id) const noexcept
{
    for (const auto &p : p_)
        if (p.id == id)
            return &p;
    return nullptr;
}

void Mixer::set_volume(int id, int percent) noexcept
{
    if (P *p = slot(id))
        p->vol = percent < 0 ? 0 : percent > 200 ? 200 : percent;
}

int Mixer::volume(int id) const noexcept
{
    const P *p = find(id);
    return p ? p->vol : 100;
}

void Mixer::set_muted(int id, bool m) noexcept
{
    if (P *p = slot(id))
        p->muted = m;
}

bool Mixer::muted(int id) const noexcept
{
    const P *p = find(id);
    return p && p->muted;
}

int Mixer::gain_q8(int id) const noexcept
{
    const P *p = find(id);
    if (!p)
        return 256;
    return p->muted ? 0 : p->vol * 256 / 100;
}

void Mixer::clear() noexcept
{
    for (auto &p : p_)
        p = P{};
}

int Duck::step(bool talking, int frames) noexcept
{
    const float target = !talking || level_ == 0 ? 1.0f : level_ == 1 ? 0.5f : 0.25f;
    // attack 50 ms down, release 400 ms up (one-pole, per block)
    const float tau = target < gain_ ? 0.05f : 0.4f;
    float k = static_cast<float>(frames) / (tau * 48000.0f);
    if (k > 1)
        k = 1;
    gain_ += (target - gain_) * k;
    return static_cast<int>(gain_ * 32767.0f);
}
} // namespace play::voice
