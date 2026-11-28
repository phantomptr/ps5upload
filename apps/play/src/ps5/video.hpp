// PS5Upload Play - the game's pictures: a queue of access units from the network thread, one
// hardware decoder (libSceVideodec2, depth 1), and the screen.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::ps5::video
{
// The decoder library and its compute queue: once, at start, before anything else.
bool init() noexcept;
void begin(bool hevc) noexcept;
void end() noexcept;
void header(const std::uint8_t *d, std::size_t n) noexcept;
void access_unit(const std::uint8_t *d, std::size_t n) noexcept;
bool wants_keyframe() noexcept;
struct Stats
{
    unsigned decoded = 0, shown = 0, skipped = 0, dropped = 0, errors = 0, key_waits = 0;
    float decode_ms = 0;
    bool hevc = false;
};
Stats stats() noexcept;
} // namespace play::ps5::video
