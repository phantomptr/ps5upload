// PS5Upload Play - the Join list's network side (docs/ROOMS.md): the hub's room list, a lookup of
// friends' rooms, and a room's Details (the host engine's preview, with a real round trip when
// this PS5 reaches the engine directly). One thread, short GETs over the hub's TLS client; the
// waiting room stays the app's only long-lived hub socket. A 429 is waited out silently (its
// Retry-After, with jitter); nothing is asked while the Join screen is not up.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "../core/rooms.hpp"
#include "../net/hub.hpp"

#include <atomic>
#include <cstdint>
#include <pthread.h>

namespace play
{
class RoomsNet
{
  public:
    enum class State : std::uint8_t
    {
        idle,
        loading,
        ok,
        unreachable, // "Can't reach the rooms list. Check your internet."
        off,         // the hub has rooms off, or a self-hosted hub without them
    };
    enum class Detail : std::uint8_t
    {
        none,
        loading,
        ok,
        old_host,   // no preview capability: "More details need the host to update PS5Upload"
        no_direct,  // the hub's previews are off (cut-back): "Details need a direct connection"
        no_answer,  // the host didn't answer
        gone,       // the room closed
        failed,
    };
    struct View
    {
        State state = State::idle;
        std::uint32_t rev = 0;
        rooms::List list;
        rooms::Card friends[8];
        int n_friends = 0;
        bool friends_known = false;
        // Details for one card
        Detail detail = Detail::none;
        char detail_key[72] = {}; // the room id, or addr:port for a nearby game
        rooms::Preview preview;
        int measured_ms = -1; // a real round trip (a direct way), -1 none
    };

    void init(const char *hub_ws_url) noexcept;
    void start() noexcept; // while the Join list is up
    void stop() noexcept;
    bool running() const noexcept
    {
        return run_.load();
    }
    // The first page again with this sort and filter (the hub sorts and filters too).
    void refresh(rooms::Sort s, const rooms::Filter &f) noexcept;
    void more() noexcept;
    // The friends' lobbies to look up (Play again passes that carry one); asked at the hub's pace.
    void set_friends(const char *const *lobbies, const char *const *names, int n) noexcept;
    // Details for a card (the preview, and a round trip on a direct way: a nearby game).
    void details(const rooms::Card &c) noexcept;
    void clear_details() noexcept;
    View view() noexcept;
    std::uint32_t rev() const noexcept
    {
        return rev_.load();
    }
    const char *base() const noexcept
    {
        return base_;
    }

  private:
    static void *entry(void *self) noexcept;
    void main() noexcept;
    bool wait_turn(std::uint64_t &next_ok) noexcept;
    void fetch_list(bool more) noexcept;
    void fetch_friends() noexcept;
    void fetch_details() noexcept;
    int hub_get(const char *path, std::uint64_t &next_ok, int *status) noexcept;

    net::HubLink http_;
    char base_[128] = {};
    char body_[48 * 1024];
    pthread_t thread_{};
    pthread_mutex_t lock_ = PTHREAD_MUTEX_INITIALIZER;
    std::atomic<bool> run_{false};
    std::atomic<std::uint32_t> rev_{0};
    View v_;
    // asks (under lock_)
    bool want_list_ = false, want_more_ = false, want_detail_ = false;
    rooms::Sort sort_ = rooms::Sort::best;
    rooms::Filter filter_;
    rooms::Card detail_card_;
    char lobbies_[8][24] = {};
    char names_[8][40] = {};
    int n_lobbies_ = 0;
    std::uint64_t next_friends_ = 0;
    int every_s_ = 60;
    // backoff per kind of request (a 429's Retry-After, jittered)
    std::uint64_t next_list_ = 0, next_status_ = 0, next_preview_ = 0;
    int fails_ = 0;
};
} // namespace play
