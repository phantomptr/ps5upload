// PS5Upload Play - room voice (the Play Rooms design's 7A): the frames on the guest socket, the
// voice-activity gate, push-to-talk, when this PS5 may send at all, and the volume per person.
// Pure logic: the microphone, Opus and the speakers are src/ps5/voice.cpp and sound.cpp.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::voice
{
// The engine's wire (API.md "Play Rooms chat and voice"):
// guest -> engine: [0x0A][seq u16 BE][level u8][Opus 1-255 B] (20 ms, 40 ms at 12 kb/s, mono);
// engine -> guest: [0x0B][n][(sid u8, seq u16 BE, len u8, Opus)...], one bundle per tick.
// (The design's 6 and 7 were taken: 7 is the timed video access unit on the same socket.)
constexpr std::uint8_t kUp = 0x0A;
constexpr std::uint8_t kDown = 0x0B;
constexpr int kRate = 16000;        // capture and encode (wideband speech)
constexpr int kFrame = 320;         // 20 ms at 16 kHz (40 ms: 640, when the engine says frame_ms 40)
constexpr int kFrameMax = 640;
constexpr int kMaxOpus = 255;       // a bundle's length byte
constexpr int kMaxSpeakers = 3;     // the engine forwards at most 3 to a listener
constexpr std::uint64_t kHangoverUs = 300000;

std::size_t up_frame(std::uint8_t *out, std::size_t cap, std::uint16_t seq, std::uint8_t level,
                     const std::uint8_t *opus, std::size_t len) noexcept;

struct Item
{
    std::uint8_t speaker = 0;
    std::uint16_t seq = 0;
    const std::uint8_t *opus = nullptr;
    std::uint8_t len = 0;
};
// A bundle (with its kind byte); -1 when it is not well formed.
int parse_down(const std::uint8_t *d, std::size_t n, Item *out, int cap) noexcept;

// {"mic": "on"|"off"|"ptt_down"|"ptt_up"}
std::size_t mic_message(char *out, std::size_t cap, const char *state) noexcept;

// The level byte (0-255) of a 20 ms frame: its loudness on a log scale (-60 dBFS..0 -> 0..255).
std::uint8_t level_of(const std::int16_t *pcm, int n) noexcept;

// Voice-activity gate: open above a threshold, kept open 300 ms after the last loud frame (so
// words aren't cut), never open on silence. Frames while closed are not sent at all.
class Gate
{
  public:
    // A frame's level; true: send this frame.
    bool feed(std::uint8_t level, std::uint64_t now_us) noexcept;
    void reset() noexcept
    {
        open_until_ = 0;
        noise_ = 40;
    }
    std::uint8_t threshold() const noexcept;

  private:
    std::uint64_t open_until_ = 0;
    int noise_ = 40; // a slow floor follow: a fan or a hum doesn't keep it open
};

// What may go out right now (the design's 7A.2/7A.3 and docs/MIC.md): the person's mic switch
// (off on every join), how they talk, the room's voice mode, the host's mute, the console's
// silent state (0 = a live mic), and the app in front.
enum class Mode : std::uint8_t
{
    ptt,  // hold Create (or the overlay's Talk)
    open, // open mic (only where the room allows it and with a headset)
};
struct Allow
{
    bool mic_on = false;
    Mode mode = Mode::ptt;
    bool talk_held = false;   // Create held (push-to-talk)
    const char *room = "";    // "off" / "ptt" / "open" ("" = no voice there)
    bool host_muted = false;
    int silent = 1;           // sceAudioInGetSilentState (0 = live), <0 not open
    bool in_front = true;
    bool paused = false;      // the engine paused voice (Poor connection)
    int kbps = 24;            // what the engine says to encode at (24 or 12; 0 = paused)
    int frame_ms = 20;        // 20 or 40
};
bool may_send(const Allow &a) noexcept;
// Why not (for the overlay): "off", "room_off", "muted", "no_mic", "mic_muted", "behind",
// "paused", "hold" (push-to-talk not held), "" = sending.
const char *why_not(const Allow &a) noexcept;

// Push-to-talk edges: {"mic": "ptt_down"} when Create goes down, "ptt_up" when it comes up.
class Ptt
{
  public:
    // Returns "ptt_down", "ptt_up" or null (no change).
    const char *update(bool held) noexcept
    {
        if (held == held_)
            return nullptr;
        held_ = held;
        return held ? "ptt_down" : "ptt_up";
    }
    bool held() const noexcept
    {
        return held_;
    }

  private:
    bool held_ = false;
};

// Volume per person (0-200 %, 100 by default) and mute, by the engine's speaker id; local only.
class Mixer
{
  public:
    static constexpr int kPeople = 8;
    void set_volume(int id, int percent) noexcept;
    int volume(int id) const noexcept;
    void set_muted(int id, bool m) noexcept;
    bool muted(int id) const noexcept;
    // The gain (Q8: 256 = 100 %) for a speaker.
    int gain_q8(int id) const noexcept;
    void clear() noexcept;

  private:
    struct P
    {
        int id = -1;
        int vol = 100;
        bool muted = false;
    } p_[kPeople];
    P *slot(int id) noexcept;
    const P *find(int id) const noexcept;
};

// The game's sound while someone talks (the design's 7A.1): Off / A little (-6 dB) / More (-12 dB),
// 50 ms attack, 400 ms release. step() per block of `frames` at 48 kHz; returns the Q15 gain.
class Duck
{
  public:
    void set_level(int level) noexcept // 0 off, 1 a little, 2 more
    {
        level_ = level < 0 ? 0 : level > 2 ? 2 : level;
    }
    int step(bool talking, int frames) noexcept;

  private:
    int level_ = 1;
    float gain_ = 1.0f;
};
} // namespace play::voice
