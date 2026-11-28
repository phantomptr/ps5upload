// PS5Upload Play - room voice on the console (docs/ROOMS.md, docs/MIC.md): the microphone on a
// thread of its own (voice chat port, 16 kHz mono, grain 256), Opus 20 ms frames to the host
// engine only while allowed (mic on, push-to-talk held or open mic with a headset, a live mic,
// the app in front, not muted by the host), and the others' voices decoded and mixed into the
// game's sound, which dips a little while someone talks.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "../app/guest.hpp"
#include "../app/platform.hpp"
#include "../core/voice.hpp"

#include <cstddef>
#include <cstdint>

namespace play::ps5::voice_io
{
// Starts the capture thread for this user (the port opens only when the mic is switched on).
bool init(int user) noexcept;
void set_guest(Guest *g) noexcept;
void allow(const voice::Allow &a) noexcept;
platform::MicState state() noexcept;
// A speaker's frame from the engine (the guest thread).
void packet(std::uint8_t speaker, std::uint16_t seq, const std::uint8_t *opus, std::size_t len) noexcept;
// The sound thread: adds the voices into one block of the game's 48 kHz stereo sound (and dips
// the game under them).
void mix(std::int16_t *stereo, unsigned frames) noexcept;
void stream_end() noexcept;
void volume(int speaker, int percent, bool muted) noexcept;
void duck(int level) noexcept;
std::uint32_t speaking() noexcept;
void set_headset(bool on) noexcept;
} // namespace play::ps5::voice_io
