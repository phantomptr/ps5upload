// PS5Upload Play - the controller thread.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "../app/platform.hpp"

#include <cstdint>

namespace play::ps5::padio
{
bool open(int user) noexcept;
platform::Input input() noexcept;
void stream_to(Guest *g) noexcept;
platform::Chords chords() noexcept;
platform::Chords chords_peek() noexcept;
// The Room menu has the controller: the game gets "nothing pressed".
void to_menu(bool on) noexcept;
// Create is push-to-talk (never sent to the game).
void ptt_on_create(bool on) noexcept;
void rumble(std::uint8_t left, std::uint8_t right) noexcept;
void light(std::uint8_t r, std::uint8_t g, std::uint8_t b) noexcept;
void light_reset() noexcept;
unsigned sent() noexcept;
// Rumble changes applied to the controller (scePadSetVibration calls), and the last one's result.
struct Rumbles
{
    unsigned calls = 0, nonzero = 0;
    int last_rc = 0;
};
Rumbles rumbles() noexcept;
} // namespace play::ps5::padio
