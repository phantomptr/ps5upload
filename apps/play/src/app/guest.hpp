// PS5Upload Play - the guest: joining a friend's Play Together over the engine's guest socket
// (/api/join/ws). One thread talks to the engine; pictures and sound go to the sinks; the pad
// thread sends pad frames through send_pad().
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "../core/chat.hpp"
#include "../core/proto.hpp"
#include "../core/rooms.hpp"
#include "../net/net.hpp"
#include "../net/route.hpp"

#include <atomic>
#include <cstdint>
#include <pthread.h>

namespace play
{
// What the guest does with what arrives. Called on the guest thread.
class GuestSinks
{
  public:
    virtual ~GuestSinks() = default;
    // A new picture stream starts (joined, or back after a reconnect).
    virtual void begin(bool hevc) = 0;
    virtual void end() = 0;
    virtual void video_header(const std::uint8_t *d, std::size_t n) = 0;
    // engine_us: the engine's time the picture was whole (timed video), else 0.
    virtual void video(const std::uint8_t *d, std::size_t n, std::uint64_t engine_us) = 0;
    virtual void audio_header(const std::uint8_t *d, std::size_t n) = 0;
    virtual void audio(std::uint16_t index, const std::uint8_t *d, std::size_t n) = 0;
    virtual void rumble(std::uint8_t left, std::uint8_t right) = 0;
    virtual void light(std::uint8_t r, std::uint8_t g, std::uint8_t b) = 0;
    // The picture waits for a keyframe (the session asks the engine, once a second at most).
    virtual bool wants_keyframe() = 0;
    // The report for the host's panel: `{"frames":...}` members (without braces).
    virtual void stats(char *out, std::size_t cap) = 0;
    // Room voice (docs/ROOMS.md): one speaker's 20 ms Opus frame, already the person's to hear.
    virtual void voice(std::uint8_t speaker, std::uint16_t seq, const std::uint8_t *opus, std::size_t len)
    {
        (void)speaker, (void)seq, (void)opus, (void)len;
    }
};

struct GuestView
{
    enum Phase : std::uint8_t
    {
        idle,
        connecting,
        asking,   // the host is being asked
        waiting,  // the host is starting their game again
        joined,
        reconnecting,
        refused,  // why
        ended,    // why: the engine's bye, "left", "lost"
        failed,   // why: could not reach the engine ("unreachable", "busy", "old_engine")
        name_taken, // someone in the room has this name: `suggest` (join again with it or another)
        password,   // the room wants its password (the knock's challenge came; none given yet)
    } phase = idle;
    char why[24] = {};
    char host[48] = {};    // the host's name, from status
    char engine[64] = {};  // addr:port
    int player = 0;
    int rtt_ms = -1;
    bool hevc = false;
    bool block_system = false;
    bool stream_seat = false;
    bool move_first = false;
    bool took_host = false;
    char plays_as[48] = {};
    char host_user[48] = {};
    char codec_warning[12] = {};
    std::uint64_t phase_since_us = 0;
    std::uint64_t pictures = 0, sounds = 0, pads_sent = 0;
    int delivery_ms = -1; // engine -> app time of the last picture, when the engine times them
    char friend_pass[48] = {}; // the host's engine remembers this PS5 ("Play again"), from joined
    // The way to the engine (docs/ROUTES.md, guest_route.cpp): its kind (route::Kind, -1 before
    // one is chosen) and tier (route::Tier), the backups kept and the best one's tier, the
    // changes so far, and the last change (when, why: "upgrade", "rescue", "lost").
    std::int8_t route = -1;
    std::uint8_t tier = 0;
    bool bridged = false;
    std::uint8_t backups = 0, backup_tier = 0;
    int route_rtt_ms = -1, route_jitter_ms = -1, route_loss = -1;
    int route_switches = 0;
    std::uint64_t route_changed_us = 0;
    char route_why[12] = {};
    char bridge_at[64] = {}; // this home's PS5Upload (addr:port), when a join through it failed
    // Play Rooms (docs/ROOMS.md)
    char suggest[40] = {};   // name_taken: the name the host engine offers ("Sam (2)")
    int tries_left = -1;     // refused "password": tries left
    char lobby[24] = {};     // the host's lobby on the hub (joined), for Your friends' rooms
    char token[80] = {};     // the resume token (kept on disk: back after the host restarts)
    rooms::RoomState room;   // the room push
    std::uint32_t chat_rev = 0; // bumps on every chat line (Guest::chat_log() has them)
    int chat_unread = 0;
    char chat_note[12] = {};   // the engine's last chat note ("slow", "off", "muted")
    std::uint64_t chat_note_us = 0;
    unsigned voice_in = 0, voice_out = 0; // frames heard / sent
};

// How a join goes beyond the code (docs/EASY_JOIN.md): an invite link's key, a friend pass, asking
// to be remembered, this app's waiting room, and the guest socket's path when the host's engine is
// served under a prefix.
struct JoinExtras
{
    char invite[132] = {};
    char again[48] = {};
    bool remember = true;
    char inbox_room[24] = {};
    char inbox_hub[64] = {};
    char path[80] = {}; // "" = /api/join/ws
    // The invite key the hello carries on the bridge way only (docs/ROUTES.md §6): Play again
    // through this home's PS5Upload asks with the host's last internet invite, because an engine
    // takes no friend pass from an internet room. Unused when `invite` is set.
    char bridge_invite[132] = {};
    // Play Rooms (docs/ROOMS.md): this PS5's device id (base64url), how the join was found
    // ("rooms", "friends", "nearby", "again"; "" = an invite or a code), and the room's password
    // and key for a sealed knock (the challenge comes from the engine).
    char device[32] = {};
    char via[12] = {};
    char password[260] = {};
    char pk[100] = {};
    // Many controllers (integration design (d)): a seat without picture or sound in the group of
    // the player already in (its resume token).
    bool no_media = false;
    char group[80] = {};
    // Coming back to a game after this app was closed (a host restart, a crash): the resume token.
    char resume[80] = {};
};

class Guest
{
  public:
    Guest(GuestSinks &sinks, std::uint8_t *msg_buf, std::size_t msg_cap) noexcept;
    // Starts joining (code: 6 characters, or "" for a nearby join). False while one runs.
    bool start(const char *addr, std::uint16_t port, const char *code, const char *name,
               const char *version) noexcept;
    // For the next start(): see JoinExtras (kept until set again).
    void set_extras(const JoinExtras &x) noexcept
    {
        extras_ = x;
    }
    // Leaves (tells the host) and stops; returns at once.
    void leave() noexcept;
    // Waits for the thread to end (after leave(), or once it ended by itself).
    void join() noexcept;
    bool running() const noexcept
    {
        return running_.load();
    }
    GuestView view() noexcept;
    // From the pad thread while joined; false when it could not go (not joined, socket gone).
    bool send_pad(const proto::PadState &s) noexcept;
    // The console came back to the front after its sockets were let go: reconnect now.
    void nudge() noexcept
    {
        nudge_.store(true);
    }
    // The ways to the engine, best checked at connect and kept as backups during play
    // (docs/ROUTES.md). Set before start(); none: only start()'s address. Copied.
    void set_routes(const route::Candidate *c, int n) noexcept;
    // Testing: let the socket go as a suspend does (the session then comes back with its token).
    void drop_socket() noexcept
    {
        ws_.close();
    }

    // Play Rooms (any thread): chat, the mic, voice frames, the ready answer. False when not joined.
    bool send_quick(int id) noexcept;
    bool send_text(const char *text) noexcept; // cleaned here (chat.hpp); false when rate-limited
    bool send_mic(const char *state) noexcept;  // "on", "off", "ptt_down", "ptt_up"
    bool send_voice(std::uint16_t seq, std::uint8_t level, const std::uint8_t *opus, std::size_t len) noexcept;
    // The password for a room that asked for it (phase password): sealed and sent.
    bool send_password(const char *pw) noexcept;
    // {"notice": n} once the responsibility notice is accepted; {"flag": sid}: "Tell the host".
    bool send_notice(int version) noexcept;
    // {"release": 1}: this player's place is freed, the picture and sound stay for the others
    // from this PS5 (integration design (d), item 3). The pad stops going.
    bool release() noexcept;
    bool send_flag(int sid) noexcept;
    bool send_ready() noexcept; // {"ready": true} in the host's ready check
    // A copy of the chat (the guest thread adds to it).
    void chat_copy(chat::Log &out) noexcept;
    void chat_read() noexcept;

  private:
    static void *entry(void *self) noexcept;
    void run() noexcept;
    bool connect_and_hello(bool resume) noexcept;
    std::size_t make_hello(char *out, std::size_t cap, bool resume, int way) noexcept;
    void handle_text(const char *t, std::size_t n) noexcept;
    void handle_binary(const std::uint8_t *d, std::size_t n) noexcept;
    void set_phase(GuestView::Phase p, const char *why) noexcept;

    // guest_route.cpp: the ways (docs/ROUTES.md)
    bool route_open(net::WsClient &ws, int way, int *status) noexcept;
    bool route_is_bridge(int way) noexcept; // -1: the one in use
    void route_pick() noexcept;
    void route_tick(std::uint64_t now) noexcept;
    bool route_switch(int to, const char *why) noexcept;
    void route_sent(std::uint32_t id) noexcept;
    void route_answer(std::uint32_t id) noexcept;
    void route_picture(std::uint64_t engine_us) noexcept;
    void route_heard() noexcept;
    void route_status(const char *t, std::size_t n) noexcept;
    void route_lost(const char *why) noexcept;
    void route_stats(char *out, std::size_t cap) noexcept;
    void route_view() noexcept;
    void route_stop() noexcept;
    static void *route_entry(void *self) noexcept;
    void route_main() noexcept;

    GuestSinks &sinks_;
    net::WsClient ws_;
    pthread_t thread_{};
    pthread_mutex_t lock_;
    std::atomic<bool> running_{false}, stop_{false}, leave_{false}, nudge_{false};
    GuestView view_;
    char addr_[64] = {};
    std::uint16_t port_ = 0;
    char code_[8] = {};
    char name_[48] = {};
    char version_[24] = {};
    char token_[80] = {};
    JoinExtras extras_;
    std::atomic<bool> joined_{false}, raw_pad_{false};
    std::atomic<std::uint32_t> pad_seq_{1};
    proto::KeyframeAsk key_ask_;
    std::uint64_t dropped_at_ = 0;
    // engine clock vs ours (timed video): offset = engine_us - our µs at the pong's middle
    bool have_offset_ = false;
    std::int64_t offset_us_ = 0;
    std::uint64_t ping_sent_us_ = 0;
    // the ways (guest_route.cpp); sel_ under route_lock_
    route::Selector sel_;
    pthread_mutex_t route_lock_;
    int n_routes_ = 0;
    std::atomic<int> switch_to_{-1};
    std::atomic<bool> route_run_{false};
    pthread_t route_thread_{};
    std::int64_t last_skipped_ = -1;
    int lost_tries_ = 0;
    // why the last try through this home's PS5Upload failed ("" none): the bridge's own reason,
    // "old" (no /api/join/bridge there: older than 13.0.0) or "unreachable"
    char bridge_why_[16] = {};
    char bridge_at_[64] = {};
    // Play Rooms
    chat::Log chat_;
    chat::Rate chat_rate_;
    char challenge_[48] = {};
    char room_pk_[100] = {};
    bool host_restarting_ = false; // the engine said "waiting": its session restarts (a longer window)
    std::atomic<bool> released_{false};
    void handle_room_text(const char *t, std::size_t n) noexcept;
    bool seal_and_send(const char *pw) noexcept;
};
} // namespace play
