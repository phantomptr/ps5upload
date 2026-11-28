// PS5Upload Play - many controllers on one PS5 (the integration design's (d)): each signed-in
// controller is a person who may come along. The first player joins as usual; any other holds ✕
// for 1 s (so a stray press doesn't count) to ask for a seat of their own, on a connection of its
// own without picture or sound (`"media": false` + `"group"`, the first player's token). A
// controller that goes off keeps its place 60 s (the engine's resume window), then leaves. Pure
// logic: padio reads the pads, the app opens the extra connections.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>

namespace play::seats
{
constexpr std::uint64_t kHoldUs = 1000000;
constexpr std::uint64_t kAwayUs = 60000000; // the engine keeps a seat this long

enum class State : std::uint8_t
{
    idle,    // signed in, not playing: "Hold ✕ to come along"
    asking,  // its connection is asking the host
    playing,
    away,    // the controller went off while playing: the place is kept for a while
};

struct Pad
{
    int user = -1;
    char name[40] = {};
    bool connected = false;
    State state = State::idle;
    std::uint64_t hold_since = 0, away_since = 0;
    int player = 0;
    char token[80] = {}; // its resume token (from "joined")
    bool primary = false;
};

class Seats
{
  public:
    static constexpr int kMax = 4;

    // A signed-in user's controller as padio sees it (connected or not).
    void pad(int user, const char *name, bool connected, std::uint64_t now) noexcept
    {
        Pad *p = find(user);
        if (!p)
        {
            if (n_ >= kMax)
                return;
            p = &pads_[n_++];
            *p = Pad{};
            p->user = user;
        }
        std::snprintf(p->name, sizeof p->name, "%s", name ? name : "");
        if (!connected && p->connected && p->state == State::playing)
        {
            p->state = State::away;
            p->away_since = now ? now : 1;
        }
        if (connected && p->state == State::away)
            p->state = State::playing; // back in time: the same seat (the app resumes with the token)
        if (!connected)
            p->hold_since = 0;
        p->connected = connected;
    }
    void set_primary(int user) noexcept
    {
        for (int i = 0; i < n_; ++i)
        {
            pads_[i].primary = pads_[i].user == user;
            if (pads_[i].primary)
                pads_[i].state = State::playing;
        }
    }
    // The game took every seat it has: holding ✕ asks nothing.
    void set_full(bool full) noexcept
    {
        full_ = full;
    }
    bool full() const noexcept
    {
        return full_;
    }
    // ✕ held or not on an idle controller; true once it was held 1 s (then: open its connection).
    bool hold(int user, bool down, std::uint64_t now) noexcept
    {
        Pad *p = find(user);
        if (!p || p->state != State::idle || !p->connected)
            return false;
        if (!down)
        {
            p->hold_since = 0;
            return false;
        }
        if (!p->hold_since)
            p->hold_since = now ? now : 1;
        if (now - p->hold_since < kHoldUs || full_)
            return false;
        p->hold_since = 0;
        p->state = State::asking;
        return true;
    }
    // Hold progress 0-1 for the waiting room's ring.
    float progress(int user, std::uint64_t now) const noexcept
    {
        for (int i = 0; i < n_; ++i)
            if (pads_[i].user == user && pads_[i].hold_since)
            {
                const std::uint64_t h = now - pads_[i].hold_since;
                return h >= kHoldUs ? 1.0f : static_cast<float>(h) / kHoldUs;
            }
        return 0;
    }
    void joined(int user, int player, const char *token) noexcept
    {
        if (Pad *p = find(user))
        {
            p->state = State::playing;
            p->player = player;
            std::snprintf(p->token, sizeof p->token, "%s", token ? token : "");
        }
    }
    // Its connection ended (left, refused, removed): back to idle.
    void left(int user) noexcept
    {
        if (Pad *p = find(user))
        {
            p->state = State::idle;
            p->player = 0;
            p->token[0] = 0;
            p->hold_since = 0;
        }
    }
    // An away controller whose place ran out: the app says leave on its connection.
    bool expired(int user, std::uint64_t now) const noexcept
    {
        for (int i = 0; i < n_; ++i)
            if (pads_[i].user == user)
                return pads_[i].state == State::away && now - pads_[i].away_since > kAwayUs;
        return false;
    }
    int count() const noexcept
    {
        return n_;
    }
    const Pad &at(int i) const noexcept
    {
        return pads_[i];
    }
    int playing() const noexcept
    {
        int k = 0;
        for (int i = 0; i < n_; ++i)
            k += pads_[i].state == State::playing || pads_[i].state == State::away ? 1 : 0;
        return k;
    }
    void remove(int user) noexcept
    {
        for (int i = 0; i < n_; ++i)
            if (pads_[i].user == user)
            {
                for (int k = i + 1; k < n_; ++k)
                    pads_[k - 1] = pads_[k];
                --n_;
                return;
            }
    }
    void clear() noexcept
    {
        n_ = 0;
        full_ = false;
    }

  private:
    Pad *find(int user) noexcept
    {
        for (int i = 0; i < n_; ++i)
            if (pads_[i].user == user)
                return &pads_[i];
        return nullptr;
    }
    Pad pads_[kMax];
    int n_ = 0;
    bool full_ = false;
};
} // namespace play::seats
