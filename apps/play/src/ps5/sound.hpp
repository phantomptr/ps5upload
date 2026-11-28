// PS5Upload Play - sound: one 48 kHz stereo output that plays the game's sound while joined
// (Opus through the jitter buffer), and the menu music and button sounds otherwise.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::ps5::sound
{
bool init(const std::uint8_t *music_pcm, std::size_t music_bytes) noexcept;
void stream_begin() noexcept;
void stream_end() noexcept;
void stream_header(const std::uint8_t *d, std::size_t n) noexcept;
void stream_packet(std::uint16_t index, const std::uint8_t *d, std::size_t n) noexcept;
void music(bool on, int volume_percent) noexcept;
void sounds(bool on) noexcept;
enum class Blip
{
    move,
    choose,
    back,
    bad,
};
void blip(Blip b) noexcept;
struct Stats
{
    unsigned decoded = 0, concealed = 0, recovered = 0, underruns = 0, dropped = 0, late = 0, decode_errors = 0;
};
Stats stats() noexcept;
} // namespace play::ps5::sound
