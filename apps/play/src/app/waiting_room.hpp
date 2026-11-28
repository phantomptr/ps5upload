// PS5Upload Play - the waiting room (docs/EASY_JOIN.md): while the home screen is up, the app hosts
// a room on the signalling hub (protocol v1, bound to a secret kept in room.txt) and shows its QR
// code. A phone that scanned it, or a host's PS5Upload ("Send invite"), joins the room and hands
// over an invite link; the app answers and the screen starts the join.
//
// Hub load is kept small (owner rule): the room is open only while armed (the home screen, at most
// kOpenFor at a stretch), the app sends nothing but `host`, its answers and `leave`, and the hub's
// pings keep it; the link closes when the screen goes or the time is up.
//
// This is the app's only connection to the hub (net::HubLink). Playing an internet invite never
// opens another: this home's PS5Upload talks to the hub for the bridge (docs/ROUTES.md §6).
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "../core/invite.hpp"
#include "../net/hub.hpp"
#include "../net/net.hpp"

#include <atomic>
#include <cstdint>
#include <pthread.h>

namespace play
{
class WaitingRoom
{
  public:
    static constexpr std::uint64_t kOpenForUs = 5ull * 60 * 1000000; // per stretch
    static constexpr const char *kHubUrl = "wss://hub.phantomptr.com/ws";
    static constexpr const char *kPage = "https://ps5upload.phantomptr.com/join/";

    struct View
    {
        enum State : std::uint8_t
        {
            off,        // not armed (or its time is up)
            connecting,
            ready,      // the QR works
            offline,    // the hub can't be reached (retrying)
        } state = off;
        char room[23] = {};
        char hub_host[64] = {};
        char qr[200] = {}; // the QR code's link
        bool expired = false; // the stretch ran out: arm again to show it
    };

    WaitingRoom() noexcept
    {
        pthread_mutex_init(&lock_, nullptr);
    }
    WaitingRoom(const WaitingRoom &) = delete;
    WaitingRoom &operator=(const WaitingRoom &) = delete;
    // secret_path: where the room's secret is kept (made on first use). hub_url: "" for kHubUrl.
    void init(const char *secret_path, const char *hub_url) noexcept;
    // Opens the room (now, for kOpenForUs) / closes it.
    void arm() noexcept;
    void disarm() noexcept;
    View view() noexcept;
    // An invite handed over (and its routes probed), oldest first; false when none.
    bool take(invite::Invite &out) noexcept;
    // While true, hand-offs are answered "busy" (playing, or a join already on its way).
    void set_busy(bool b) noexcept
    {
        busy_ = b;
    }
    // Whether this home has a PS5Upload the app can play an internet invite through (the bridge,
    // docs/ROUTES.md §6): such an invite is then answered yes even when nothing direct reaches it.
    void set_bridge(bool b) noexcept
    {
        bridge_ = b;
    }
    void stop() noexcept;
    // The room this app hosts and its hub's host: for the hello's `inbox`, so a host can send an
    // invite later. Empty until init().
    const char *room() const noexcept
    {
        return room_;
    }
    const char *hub_host() const noexcept
    {
        return hub_host_;
    }
    // Picks the direct route the app can reach (probes each), into inv.routes[0]; false: none.
    // Blocking (a second or two per route): the room's thread calls it.
    static bool pick_route(invite::Invite &inv) noexcept;

  private:
    static void *main(void *self) noexcept;
    void run() noexcept;
    void session() noexcept;
    void answer(net::HubLink &l, const char *gid, bool ok, const char *why) noexcept;

    pthread_t thread_{};
    pthread_mutex_t lock_;
    std::atomic<bool> run_{false}, busy_{false}, bridge_{false};
    std::atomic<std::uint64_t> armed_until_{0};
    View view_;
    char url_[128] = {};
    char secret_[48] = {};
    char room_[23] = {};
    char hub_host_[64] = {};
    char nonce_[24] = {};
    invite::Invite queue_[2];
    int queued_ = 0;
};
} // namespace play
