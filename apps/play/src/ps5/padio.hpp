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
void rumble(std::uint8_t left, std::uint8_t right) noexcept;
void light(std::uint8_t r, std::uint8_t g, std::uint8_t b) noexcept;
void light_reset() noexcept;
unsigned sent() noexcept;
} // namespace play::ps5::padio
