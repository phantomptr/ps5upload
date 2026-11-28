// PS5Upload Play - the guest: joining a friend's Play Together over the engine's guest socket
// (/api/join/ws). One thread talks to the engine; pictures and sound go to the sinks; the pad
// thread sends pad frames through send_pad().
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "../core/proto.hpp"
#include "../net/net.hpp"

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
    // Testing: let the socket go as a suspend does (the session then comes back with its token).
    void drop_socket() noexcept
    {
        ws_.close();
    }

  private:
    static void *entry(void *self) noexcept;
    void run() noexcept;
    bool connect_and_hello(bool resume) noexcept;
    void handle_text(const char *t, std::size_t n) noexcept;
    void handle_binary(const std::uint8_t *d, std::size_t n) noexcept;
    void set_phase(GuestView::Phase p, const char *why) noexcept;

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
};
} // namespace play
