// PS5Upload Play - what the app says to the PS5Upload engine and how it reads the answers.
// Guest socket: /api/join/ws (engine join.rs). App channel: /api/play/app (play_app.rs).
// Discovery: UDP 19261 (play_discovery.rs).
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::proto
{
// ---- the controller --------------------------------------------------------------------------
// libScePad's button bits (what scePadRead gives; the engine's `vpad::button`).
namespace pad
{
constexpr std::uint32_t kCreate = 0x1, kL3 = 0x2, kR3 = 0x4, kOptions = 0x8, kUp = 0x10, kRight = 0x20,
                        kDown = 0x40, kLeft = 0x80, kL2 = 0x100, kR2 = 0x200, kL1 = 0x400, kR1 = 0x800,
                        kTriangle = 0x1000, kCircle = 0x2000, kCross = 0x4000, kSquare = 0x8000,
                        kPs = 0x10000, kTouchpad = 0x100000;
// Set on every sample while the system has the pad (home screen, control center in front).
constexpr std::uint32_t kIntercepted = 0x80000000u;
} // namespace pad

// The apps' bits (Remote Play's `PadButton`), for an engine that does not take raw bits.
std::uint32_t raw_to_app(std::uint32_t raw) noexcept;

struct Touch
{
    std::uint8_t id;
    std::uint16_t x, y; // 0-1919, 0-1079
};

struct PadState
{
    std::uint32_t buttons = 0; // raw (libScePad) bits
    std::uint8_t lx = 128, ly = 128, rx = 128, ry = 128, l2 = 0, r2 = 0;
    std::uint8_t touches = 0;
    Touch touch[2] = {};
    bool has_motion = false;
    float gyro[3] = {}, accel[3] = {}; // rad/s, g
};

bool same_input(const PadState &a, const PadState &b) noexcept; // buttons, sticks, triggers, touch
// `[0x01][u32 seq][u32 buttons][lx ly rx ry l2 r2][n][n x (id, u16 x, u16 y)]` then optionally
// `[0x02][6 x f32]`, all little-endian (join.rs parse_pad). `raw`: send libScePad bits as they
// are (the engine was told raw_pad); else the apps' bits.
std::size_t pad_frame(std::uint8_t *out, std::size_t cap, std::uint32_t seq, const PadState &s,
                      bool raw) noexcept;
constexpr std::size_t kPadFrameMax = 16 + 2 * 5 + 25;

// ---- guest socket messages --------------------------------------------------------------------
struct ClientCaps
{
    const char *version; // "13.0.0"
    bool hevc, main10, raw_pad, timing;
};

// `{"hello": {...}}`: exactly one of code / resume / nothing (nearby). name may be empty.
std::size_t hello(char *out, std::size_t cap, const char *code, const char *resume, const char *name,
                  const ClientCaps &caps) noexcept;
// Easy join (docs/EASY_JOIN.md): an invite link's key, a friend pass ("Play again"), asking for a
// pass, and this app's waiting room on the hub (for a host's "Send invite"). Empty: not said.
struct HelloExtras
{
    const char *invite = nullptr;
    const char *again = nullptr;
    bool remember = false;
    const char *inbox_room = nullptr;
    const char *inbox_hub = nullptr;
    // Play Rooms (docs/ROOMS.md): this PS5's device id (base64url of 16 bytes: the host shows 4 hex
    // characters as its tag, keeps only a hash for blocks), the knock's path when the join came
    // from Rooms / Friends / Nearby / Play again, and the rooms protocol and features.
    const char *device = nullptr;
    const char *knock_via = nullptr;
    bool rooms = false; // say "proto": 2 and the features below
    // Many controllers on one PS5 (integration design (d)): a seat with no picture or sound, in the
    // group of the player already in (its resume token).
    bool no_media = false;
    const char *group = nullptr;
};
// The rooms features this app has (the design's 10.5).
constexpr const char *kRoomFeatures = "\"rooms\",\"knock\",\"sealed\",\"chat\",\"voice\",\"group\",\"ps5app\"";
std::size_t hello_ex(char *out, std::size_t cap, const char *code, const char *resume, const char *name,
                     const ClientCaps &caps, const HelloExtras &extras) noexcept;

enum class Kind : std::uint8_t
{
    video_header = 0,
    video = 1,
    audio_header = 3,
    audio = 4,
    haptics = 6,
    video_timed = 7,
};

// A guest-socket text message, read.
struct GuestEvent
{
    enum Type : std::uint8_t
    {
        none,
        asking,
        waiting,
        joined,
        refused,
        status,
        rumble_light,
        bye,
        pong,
        knock,      // {"knock": "challenge"|"wait"|"ok"|"no", ...} (a knock from Rooms, Friends, Nearby)
        name_taken, // {"name_taken": {"suggest": "Sam (2)"}}
    } type = none;
    char why[24] = {};      // refused / waiting / bye
    char token[80] = {};    // joined
    char friend_pass[48] = {}; // joined, when the hello asked to be remembered
    int player = 0;         // joined, status
    bool hevc = false;      // joined
    bool caps_known = false; // joined carried codec_warning: the engine read our caps
    char codec_warning[12] = {};
    // status
    int rtt_ms = -1;
    char host[48] = {};
    bool block_system = false, stream_seat = false, move_first = false, took_host = false;
    char plays_as[48] = {};
    char host_user[48] = {};
    char state[16] = {};
    // rumble / light (a message may carry both)
    bool has_rumble = false, has_light = false;
    std::uint8_t rumble_lr[2] = {};
    std::uint8_t light_rgb[3] = {};
    // joined: the host's lobby on the hub (Rooms › Your friends' rooms looks it up later)
    char lobby[24] = {};
    // knock
    char knock_state[12] = {}; // challenge, wait, ok, no
    char challenge[48] = {}; // base64url
    char pk[100] = {};       // the room's key (when the knock needs a secret)
    int tries_left = -1;     // a wrong password: tries left
    char suggest[40] = {};   // name_taken
    // pong
    double pong_t = 0;
    bool has_engine_us = false;
    double engine_us = 0;
};
bool parse_guest(const char *text, std::size_t len, GuestEvent &ev) noexcept;

// Plain words for the host's answers (i18n string ids live in ui/strings.hpp).
const char *refusal_key(const char *why) noexcept;

// ---- join codes -------------------------------------------------------------------------------
constexpr char kCodeAlphabet[] = "ABCDEFGHJKMNPQRSTUVWXYZ23456789";
constexpr int kCodeLen = 6;
// Upper case, dashes and spaces dropped; false when it is not 6 alphabet characters.
bool normalize_code(const char *in, char out[kCodeLen + 1]) noexcept;
// "ABC-DEF"
void code_display(const char *code, char out[kCodeLen + 2]) noexcept;

// ---- discovery --------------------------------------------------------------------------------
constexpr std::uint16_t kDiscoveryPort = 19261;
constexpr char kProbe[] = "PS5UPLOAD_PLAY_DISCOVER v1";
constexpr std::size_t kProbeSize = 512;
void probe(std::uint8_t out[kProbeSize]) noexcept;
struct Found
{
    char name[65] = {};
    char version[24] = {};
    char id[72] = {};
    std::uint16_t port = 0;
    int together = 0; // Play Togethers open there with the host playing
    // The engine's `you` block (integration design F2): this console is one of its own, it is
    // streaming this console now, and this app is paired with it.
    bool you_known = false, you_streaming = false, you_paired = false;
    char you_name[65] = {}; // this console's name there
};
bool parse_found(const char *text, std::size_t len, Found &f) noexcept;

// ---- small helpers ----------------------------------------------------------------------------
// One keyframe request a second at most.
class KeyframeAsk
{
  public:
    bool take(std::uint64_t now_us) noexcept
    {
        if (last_ && now_us - last_ < 1000000)
            return false;
        last_ = now_us ? now_us : 1;
        return true;
    }

  private:
    std::uint64_t last_ = 0;
};

// Is this Annex-B access unit a keyframe (IDR / IRAP)? Parameter sets alone are not.
bool is_keyframe(const std::uint8_t *au, std::size_t len, bool hevc) noexcept;

// Version words: "13.0.0" -> 13000000 (for comparisons), 0 when unreadable.
std::uint32_t version_number(const char *v) noexcept;
} // namespace play::proto
