// PS5Upload Play - many controllers on one PS5 (the integration design's (d)): the first player
// joins as usual; anyone else signed in on this PS5 holds ✕ for 1 s to come along, on a seat of
// their own (a connection without picture or sound, in the first player's group, so the host
// asks once for the PS5 or lets the whole group in). Each controller's leave chord, rumble and
// light are its own; a controller that turns off keeps its place 60 s. The first player leaving
// while others play frees only their place (`release`).
// SPDX-License-Identifier: GPL-3.0-or-later
#include "app.hpp"

#include "log.hpp"
#include "platform.hpp"

#include <cstdio>
#include <cstring>
#include <new>

namespace play
{
namespace
{
using ui::Canvas;
using ui::Font;
using ui::S;
using ui::tr;
namespace col = ui::color;
constexpr int kLeft = 140;
} // namespace

void App::seat_toast(S what, const char *name) noexcept
{
    std::snprintf(seat_toast_, sizeof seat_toast_, tr(what), name);
    seat_toast_until_ = net::now_us() + 5000000;
    dirty_ = true;
}

void App::seats_frame(std::uint64_t now) noexcept
{
    const GuestView gv = guest_view();
    const bool primary_in = gv.phase == GuestView::joined || gv.phase == GuestView::reconnecting;
    if (fake_.on)
        return;
    if (!primary_in)
    {
        // the first player is not in (or left): every seat from this PS5 goes too
        for (int i = 0; i < platform::kLocalPads; ++i)
            if (seat_[i] && seat_user_[i] >= 0)
            {
                platform::attach_pad(seat_user_[i], nullptr);
                seat_[i]->leave();
                seat_user_[i] = -1;
            }
        if (gv.phase != GuestView::asking && gv.phase != GuestView::connecting)
            seats_.clear();
        return;
    }
    platform::LocalPad pads[platform::kLocalPads];
    platform::local_pads(pads);
    for (const auto &p : pads)
    {
        if (p.user < 0)
            continue;
        seats_.pad(p.user, p.name, p.connected, now);
        if (!seats_.hold(p.user, p.cross && p.in_front, now))
            continue;
        // ✕ held 1 s: a seat of its own, in the first player's group
        int place = -1;
        for (int i = 0; i < platform::kLocalPads && place < 0; ++i)
            if (seat_user_[i] < 0)
                place = i;
        if (place < 0 || !gv.token[0])
        {
            seats_.left(p.user);
            continue;
        }
        if (!seat_[place])
        {
            std::size_t cap = 0;
            std::uint8_t *buf = platform::seat_buffer(place, &cap);
            if (buf)
                seat_[place] = new (seat_mem_[place]) Guest(platform::seat_sinks(place, p.user), buf, cap);
        }
        else
            platform::seat_sinks(place, p.user);
        if (!seat_[place])
            continue;
        JoinExtras x;
        x.remember = false;
        x.no_media = true;
        std::snprintf(x.group, sizeof x.group, "%s", gv.token);
        std::snprintf(x.device, sizeof x.device, "%s", device_);
        std::snprintf(x.via, sizeof x.via, "%s", "group");
        std::snprintf(x.path, sizeof x.path, "%s", last_extras_.path);
        seat_[place]->set_extras(x);
        seat_[place]->start(addr_, port_, "", p.name[0] ? p.name : tr(S::host_friend), version_);
        seat_user_[place] = p.user;
        seat_phase_[place] = -1;
        seat_toast(S::toast_asking, p.name);
        log("seats: user %#x asks for a seat of its own", static_cast<unsigned>(p.user));
    }
    // each seat's progress
    for (int i = 0; i < platform::kLocalPads; ++i)
    {
        if (!seat_[i] || seat_user_[i] < 0)
            continue;
        const int user = seat_user_[i];
        const GuestView sv = seat_[i]->view();
        char name[40] = {};
        for (int k = 0; k < seats_.count(); ++k)
            if (seats_.at(k).user == user)
                std::snprintf(name, sizeof name, "%s", seats_.at(k).name);
        if (sv.phase != seat_phase_[i])
        {
            seat_phase_[i] = sv.phase;
            if (sv.phase == GuestView::joined)
            {
                seats_.joined(user, sv.player, sv.token);
                platform::attach_pad(user, seat_[i]);
                seat_toast(S::toast_in, name);
            }
            else if (sv.phase == GuestView::refused || sv.phase == GuestView::ended || sv.phase == GuestView::failed)
            {
                platform::attach_pad(user, nullptr);
                if (!std::strcmp(sv.why, "full"))
                {
                    std::snprintf(seat_toast_, sizeof seat_toast_, "%s", tr(S::seat_full));
                    seat_toast_until_ = now + 5000000;
                }
                seats_.left(user);
                seat_user_[i] = -1;
                continue;
            }
        }
        // its own leave chord, or a controller off for longer than its place is kept
        if (platform::pad_leave(user) || seats_.expired(user, now))
        {
            platform::attach_pad(user, nullptr);
            seat_[i]->leave();
        }
    }
    // the first player left earlier (released): once nobody from this PS5 plays, the game ends
    if (released_ && seats_.playing() == 0)
    {
        released_ = false;
        guest_->leave();
    }
}

int App::draw_seats_strip(Canvas &c, int y) noexcept
{
    if (!seats_.count())
        return 0;
    c.text(Font::small, kLeft, y, tr(S::seats_title), col::muted);
    y += 40;
    int x = kLeft;
    const int w = 400, h = 96;
    // the first player: this app's user, joining
    c.round_rect(x, y, w, h, 16, col::panel, 230);
    c.text(Font::body, x + 20, y + 12, platform::user_name(), col::text, w - 40);
    c.text(Font::small, x + 20, y + 56, tr(S::seat_joining), col::muted, w - 40);
    x += w + 16;
    for (int i = 0; i < seats_.count() && x + w < 1840; ++i)
    {
        const seats::Pad &p = seats_.at(i);
        if (p.primary)
            continue;
        char sub[64];
        switch (p.state)
        {
        case seats::State::idle:
            std::snprintf(sub, sizeof sub, "%s", tr(seats_.full() ? S::seat_full : S::seat_hold));
            break;
        case seats::State::asking: std::snprintf(sub, sizeof sub, "%s", tr(S::seat_asking)); break;
        case seats::State::playing: std::snprintf(sub, sizeof sub, tr(S::seat_playing), p.player); break;
        case seats::State::away: std::snprintf(sub, sizeof sub, "%s", tr(S::seat_away)); break;
        }
        c.round_rect(x, y, w, h, 16, col::panel, 230);
        const float prog = seats_.progress(p.user, net::now_us());
        if (prog > 0)
            c.ring(x + w - 40, y + h / 2, 22, 6, col::accent, 0, prog);
        c.text(Font::body, x + 20, y + 12, p.name, p.connected ? col::text : col::muted, w - 80);
        c.text(Font::small, x + 20, y + 56, sub, col::muted, w - 80);
        x += w + 16;
    }
    return 40 + h;
}
} // namespace play
