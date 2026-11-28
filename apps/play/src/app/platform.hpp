// PS5Upload Play - what the app needs from the console (src/ps5/) or a stand-in (tests).
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "guest.hpp"

#include <cstddef>
#include <cstdint>

namespace play::platform
{
// ---- controller ---------------------------------------------------------------------------
struct Input
{
    std::uint32_t held = 0;    // libScePad bits down now (sticks folded into the d-pad bits)
    std::uint32_t pressed = 0; // went down since the last call (none lost between frames)
    bool in_front = true;      // false while the system has the pad (home screen, control center)
};
Input input() noexcept;

// While joined, the pad thread sends this console's controller to the guest (null: stop).
void stream_pad(Guest *guest) noexcept;
struct Chords
{
    float ps = 0, leave = 0; // hold progress 0-1
    bool ps_sent = false;    // the host's PS button went (latched until read)
    bool leave_now = false;  // the leave chord completed (latched until read)
};
Chords chords() noexcept;

// ---- screen -------------------------------------------------------------------------------
// A picture from the game is on screen (shown within the last few hundred ms).
bool video_showing() noexcept;
// Shows a full 1920x1080 UI picture (when no game picture is showing).
void present(const std::uint32_t *ui) noexcept;
// Draws over the game picture: the rows [0, top) and [kH - bottom, kH) of `layer`
// (straight-alpha RGBA, 1920 wide) are blended over every picture until the next call.
void overlay(const std::uint32_t *layer, int top, int bottom) noexcept;

// ---- sound --------------------------------------------------------------------------------
enum class Blip
{
    move,
    choose,
    back,
    bad,
};
void blip(Blip b) noexcept;
void music(bool on, int volume_percent) noexcept;
void sounds(bool on) noexcept;

// ---- system -------------------------------------------------------------------------------
void power_tick() noexcept;
// The signed-in user's name, for the host's list ("" when unknown).
const char *user_name() noexcept;
// Reads a file (up to cap bytes); returns its length or -1.
long read_file(const char *path, void *buf, std::size_t cap) noexcept;
bool write_file(const char *path, const void *data, std::size_t len) noexcept;
const char *data_dir() noexcept; // where settings and the log live
// Back to the home screen (the only way an app ends).
[[noreturn]] void exit_app() noexcept;
// Where the guest's pictures and sound go.
GuestSinks &sinks() noexcept;
// One big buffer for the guest's messages, one for the companion's (both live for the app).
std::uint8_t *message_buffer(int which, std::size_t *cap) noexcept;
// The sinks' picture/sound counters for the status line (pictures shown, decode ms...).
struct MediaStats
{
    unsigned shown = 0, decoded = 0, dropped = 0, decode_errors = 0;
    float decode_ms = 0;
    unsigned snd_played = 0, snd_concealed = 0, snd_underruns = 0;
};
MediaStats media_stats() noexcept;
} // namespace play::platform
