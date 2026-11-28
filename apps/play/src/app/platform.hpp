// PS5Upload Play - what the app needs from the console (src/ps5/) or a stand-in (tests).
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "../core/voice.hpp"
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
    bool room_menu = false;  // OPTIONS + TOUCHPAD held, then △: the Room menu (latched until read)
    bool talk = false;       // Create held while it is push-to-talk
};
Chords chords() noexcept;
// The same without taking the latched presses (for drawing the hold rings).
Chords chords_peek() noexcept;
// While the Room menu is open in a game the controller is the menu's: the game gets "nothing
// pressed" (and the menu reads input() as on any screen).
void pad_to_menu(bool on) noexcept;
// Create is push-to-talk and never reaches the game (the Play Rooms design's 7A.3).
void ptt_on_create(bool on) noexcept;

// ---- room voice (docs/ROOMS.md; src/ps5/voice.cpp) --------------------------------------------
// What may be sent (the app fills in the room's parts each frame; the capture thread adds the
// console's silent state, the app in front and Create held).
void voice_allow(const voice::Allow &a) noexcept;
struct MicState
{
    bool open = false;   // the capture port is open (the mic switch is on)
    int silent = -1;     // sceAudioInGetSilentState (0 live), -1 not open
    bool sending = false; // frames are going out now
    unsigned sent = 0;
    bool headset = false; // a headset is plugged in (open mic needs one: no echo cancelling here)
};
MicState mic_state() noexcept;
// Others' voices: volume per speaker (0-200 %) and mute, and how much the game's sound drops
// while someone talks (0 off, 1 a little, 2 more).
void voice_volume(int speaker, int percent, bool muted) noexcept;
void voice_duck(int level) noexcept;
// Who is talking right now (by the engine's speaker id; bit i = speaker i), from the voices heard.
std::uint32_t voice_speaking() noexcept;

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
// A file of the app's own (assets/<name>), read once and kept; null when missing.
const std::uint8_t *asset(const char *name, std::size_t *len) noexcept;
// The console's system language id (SCE_SYSTEM_SERVICE_PARAM_ID_LANG), or -1.
int system_language() noexcept;
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
