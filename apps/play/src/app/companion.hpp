// PS5Upload Play - the host's side: the app channel to PS5Upload (/api/play/app, play_app.rs).
// The app dials out, pairs once with a code typed into PS5Upload, then shows the live status
// (Play Together, asks, guests, switches, Remote Play) and sends what the person chooses.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "../net/net.hpp"

#include <atomic>
#include <cstdint>
#include <pthread.h>

namespace play
{
struct CompanionDoc
{
    std::uint32_t rev = 0; // bumps on every doc received (for redraws)
    bool paired = false;
    char engine_name[65] = {};
    char engine_version[24] = {};
    char console_name[65] = {};
    char console_addr[48] = {};
    // session
    bool playing = false;
    char codec[12] = {};
    int bitrate_kbps = 0;
    // Play Together
    bool together_open = false, together_playing = false;
    bool has_invite = false;
    char code[8] = {}, code_display[9] = {};
    char url[256] = {};
    char works[12] = {}; // "anywhere" or "wifi"
    int invite_left_s = 0;
    struct Ask
    {
        long long id = 0;
        char name[48] = {};
        char client_kind[12] = {};
        char codec_warning[12] = {};
        // Play Rooms: where the ask came from (invite, again, nearby, friends, rooms, group), the
        // password's check (-1 none, 0 wrong, 1 right), the asking device's tag, a round trip hint,
        // and how many players a PS5's group asks for (many controllers)
        char via[12] = {};
        int password_ok = -1;
        char tag[8] = {};
        int rtt_hint = -1;
        int group_size = 0;
        bool own = false; // the host's own device: "It's me" (`decide` allow "me") gives Player 1
    } asks[4];
    int n_asks = 0;
    struct Guest
    {
        long long id = 0;
        int player = 0;
        char name[48] = {};
        char state[16] = {};
        int rtt_ms = -1;
        char via[12] = {};  // "lan" or "internet": how they connect
        char came[12] = {}; // how they found the room (invite, again, nearby, friends, rooms, group)
        char client_kind[12] = {};
        // the way they play over ("lan", "tailscale", "internet") and its tier word (docs/ROUTES.md)
        char route[12] = {};
        char tier[12] = {};
        // Play Rooms
        char tag[8] = {};
        bool from_rooms = false;
        bool muted_voice = false, muted_chat = false;
        long long group = 0; // the guest id of the first player from the same PS5 (0 none)
    } guests[4];
    // Play Rooms (docs/ROOMS.md): the room block (GET /api/join `room`)
    struct Room
    {
        bool known = false;
        char name[40] = {};
        char vis[10] = {};       // private / friends / public
        int size = 0;
        bool password = false;
        bool approve_friends = true, approve_password = false;
        bool locked = false;
        char chat[8] = {};       // off / quick / on
        char voice[8] = {};      // off / ptt / open
        bool start_muted = false;
        char listed[12] = {};    // off / listing / listed / refused / old_hub
        char listed_why[16] = {};
        char lang[8] = {}, region[16] = {};
        bool nearby = true;
        char game[72] = {};
        int link_rtt = -1;
        char link_tier[12] = {};
        struct Block
        {
            char name[40] = {};
            char tag[8] = {};
        } blocked[8];
        int n_blocked = 0;
    } room;
    // Who may end or loosen the room from this TV (scenario audit): the stream's PS5 user and the
    // console's owner, when the engine says them ("" = not said: anyone here).
    char stream_user[48] = {};
    char console_owner[48] = {};
    int n_guests = 0;
    // switches
    bool block_system_buttons = true;
    int max_players = 3;
    bool stream_seat = false;
    char invites[16] = {}; // "anywhere" / "wifi"
    long long settings_rev = 0;
    int max_guests = 3;
    // around
    bool rp_paired = false;
    int helper_present = -1; // -1 unknown, 0 no, 1 yes
    char keep_awake[16] = {};
};

struct CompanionView
{
    enum Phase : std::uint8_t
    {
        idle,
        connecting,
        unpaired,  // connected; the engine wants a pairing
        pairing,   // a code is on screen
        paired,    // full status in doc
        retrying,  // the socket dropped: coming back
        failed,    // why
    } phase = idle;
    char why[24] = {};
    char pair_code[8] = {}, pair_display[9] = {};
    int pair_left_s = 0;
    std::uint64_t pair_until_us = 0;
    char auth[16] = {}; // "invalid", "other_console", "revoked" (the token stopped working)
    char last_error[32] = {}; // the last intent's error
    CompanionDoc doc;
};

class Companion
{
  public:
    // on_token: a new pairing token for addr:port ("" when it stopped working): keep it.
    using TokenFn = void (*)(const char *addr, std::uint16_t port, const char *token, void *ctx);
    Companion(std::uint8_t *msg_buf, std::size_t msg_cap) noexcept;
    bool start(const char *addr, std::uint16_t port, const char *token, const char *version, TokenFn fn,
               void *ctx) noexcept;
    void stop() noexcept;
    bool running() const noexcept
    {
        return running_.load();
    }
    CompanionView view() noexcept;
    // For the next start(): a one-time grant from the helper (pairs without a code), and this
    // app's install id (the engine binds the pairing to it: an address change is no re-pair).
    void set_hello(const char *grant, const char *app_id) noexcept;

    // Intents (any thread). Each returns false when the socket is not up.
    bool pair() noexcept;
    bool decide(long long ask, bool allow) noexcept;
    bool kick(int player) noexcept;
    bool invite(bool fresh) noexcept;
    bool close_together() noexcept;
    bool set_switch(const char *key, const char *json_value) noexcept;
    // Play Rooms host controls (the design's 5 / 10.1), token-scoped to this console like decide.
    bool room_set(const char *key, const char *json_value) noexcept; // {"room": {key: value}}
    bool kick_block(int player, const char *block) noexcept;         // block: "room", "always" or null
    bool unblock(const char *tag) noexcept;
    bool mute(int player, bool voice, bool chat) noexcept;
    bool mute_all(bool on) noexcept;
    bool allow_group(long long ask) noexcept;                        // "Let all from that PS5 in"
    bool its_me(long long ask) noexcept;                             // the host's own device: Player 1
    bool ready_check() noexcept;                                     // "Ready?" for everyone in
    bool chat_quick(int id, int notice = 0) noexcept;
    bool keep_awake(const char *mode) noexcept;
    bool app_state(bool foreground) noexcept;

  private:
    static void *entry(void *self) noexcept;
    void run() noexcept;
    void handle(const char *t, std::size_t n) noexcept;
    bool intent(const char *body) noexcept;

    net::WsClient ws_;
    pthread_t thread_{};
    pthread_mutex_t lock_;
    std::atomic<bool> running_{false}, stop_{false};
    CompanionView view_;
    char addr_[64] = {};
    std::uint16_t port_ = 0;
    char token_[80] = {};
    char grant_[72] = {};
    char app_id_[40] = {};
    char version_[24] = {};
    TokenFn token_fn_ = nullptr;
    void *ctx_ = nullptr;
    std::atomic<std::uint32_t> req_{1};
};

bool parse_companion_doc(const char *text, std::size_t len, CompanionDoc &doc) noexcept;
} // namespace play
