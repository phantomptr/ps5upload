// PS5Upload Play - microphone capture (libSceAudioIn): whatever the system routes to the user's
// voice input (the controller's own mic, a headset on its jack, a USB headset).
// Measured on the console in docs/MIC.md.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstdint>

namespace play::ps5::mic
{
// Port types (SceAudioInType values as public PS4/PS5 homebrew sources give them).
enum Type : int
{
    kVoiceChat = 0,
    kGeneral = 1,
    kVoiceRecognition = 5,
};
// Bits of sceAudioInGetSilentState: why the port gives zeros right now (0 = live).
enum Silent : unsigned
{
    kNoDevice = 0x1,
    kPriorityLow = 0x2,
    kUserSetting = 0x4, // muted by the user
    kUnableFormat = 0x8,
};

struct Format
{
    int type = kVoiceChat;
    unsigned rate = 48000; // 16000 or 48000
    unsigned grain = 256;  // samples per read
    bool stereo = false;
    bool hq = false; // sceAudioInHqOpen instead of sceAudioInOpen
};

// sceAudioInInit; "already initialized" counts as success. Returns its result.
int init() noexcept;
// A port for the user (the id from sceUserService), or a negative error code.
int open(int user, const Format &f) noexcept;
// Blocks until one grain is captured and copies it to out (grain samples, times 2 if stereo).
// Returns what sceAudioInInput returns (the samples read), or a negative error code.
int read(int handle, std::int16_t *out) noexcept;
// Silent bits (see Silent), or a negative error code.
int silent(int handle) noexcept;
int close(int handle) noexcept;
} // namespace play::ps5::mic
