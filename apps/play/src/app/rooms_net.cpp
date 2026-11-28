// PS5Upload Play - the Join list's network side (rooms_net.hpp).
// SPDX-License-Identifier: GPL-3.0-or-later
#include "rooms_net.hpp"

#include "../net/net.hpp"
#include "../net/route_probe.hpp"
#include "log.hpp"

#include <cstdio>
#include <cstring>

namespace play
{
namespace
{
constexpr unsigned kTimeoutMs = 6000;

std::uint64_t jitter_us(std::uint64_t base_us) noexcept
{
    std::uint8_t r[2];
    net::random_bytes(r, 2);
    // up to a quarter more, so many apps don't come back at the same moment
    return base_us + base_us / 4 * (static_cast<std::uint64_t>(r[0] << 8 | r[1]) % 1000) / 1000;
}
} // namespace

void RoomsNet::init(const char *hub_ws_url) noexcept
{
    if (!net::http_base_of(hub_ws_url, base_, sizeof base_))
        std::snprintf(base_, sizeof base_, "%s", "https://hub.phantomptr.com");
}

void RoomsNet::start() noexcept
{
    if (run_.exchange(true))
        return;
    if (thread_)
        pthread_join(thread_, nullptr);
    if (pthread_create(&thread_, nullptr, entry, this) != 0)
    {
        thread_ = {};
        run_ = false;
    }
}

void RoomsNet::stop() noexcept
{
    if (!run_.exchange(false))
        return;
    if (thread_)
    {
        pthread_join(thread_, nullptr);
        thread_ = {};
    }
}

void RoomsNet::refresh(rooms::Sort s, const rooms::Filter &f) noexcept
{
    pthread_mutex_lock(&lock_);
    sort_ = s;
    filter_ = f;
    want_list_ = true;
    want_more_ = false;
    pthread_mutex_unlock(&lock_);
}

void RoomsNet::more() noexcept
{
    pthread_mutex_lock(&lock_);
    if (v_.list.next > 0 && !want_list_)
        want_more_ = true;
    pthread_mutex_unlock(&lock_);
}

void RoomsNet::set_friends(const char *const *lobbies, const char *const *names, int n) noexcept
{
    pthread_mutex_lock(&lock_);
    bool same = n == n_lobbies_;
    for (int i = 0; i < n && same; ++i)
        same = !std::strcmp(lobbies_[i], lobbies[i]);
    n_lobbies_ = n < 8 ? n : 8;
    for (int i = 0; i < n_lobbies_; ++i)
    {
        std::snprintf(lobbies_[i], sizeof lobbies_[i], "%s", lobbies[i]);
        std::snprintf(names_[i], sizeof names_[i], "%s", names && names[i] ? names[i] : "");
    }
    if (!same)
        next_friends_ = 0; // ask now (still at the hub's pace: wait_turn)
    pthread_mutex_unlock(&lock_);
}

void RoomsNet::details(const rooms::Card &c) noexcept
{
    char key[72];
    if (c.room[0])
        std::snprintf(key, sizeof key, "%s", c.room);
    else
        std::snprintf(key, sizeof key, "%s:%u", c.addr, static_cast<unsigned>(c.port));
    pthread_mutex_lock(&lock_);
    if (std::strcmp(key, v_.detail_key) != 0 || v_.detail == Detail::none)
    {
        detail_card_ = c;
        std::snprintf(v_.detail_key, sizeof v_.detail_key, "%s", key);
        v_.detail = Detail::loading;
        v_.preview = rooms::Preview{};
        v_.measured_ms = -1;
        want_detail_ = true;
        rev_.fetch_add(1);
    }
    pthread_mutex_unlock(&lock_);
}

void RoomsNet::clear_details() noexcept
{
    pthread_mutex_lock(&lock_);
    v_.detail = Detail::none;
    v_.detail_key[0] = 0;
    want_detail_ = false;
    pthread_mutex_unlock(&lock_);
}

RoomsNet::View RoomsNet::view() noexcept
{
    pthread_mutex_lock(&lock_);
    View v = v_;
    pthread_mutex_unlock(&lock_);
    return v;
}

void *RoomsNet::entry(void *self) noexcept
{
    static_cast<RoomsNet *>(self)->main();
    return nullptr;
}

bool RoomsNet::wait_turn(std::uint64_t &next_ok) noexcept
{
    return net::now_us() >= next_ok;
}

// GET base+path: the status, the body in body_. 429: the next try waits Retry-After (jittered)
// and nothing is said; other failures back off a little more each time.
int RoomsNet::hub_get(const char *path, std::uint64_t &next_ok, int *status) noexcept
{
    char url[400];
    std::snprintf(url, sizeof url, "%s%s", base_, path);
    int retry = 0;
    const int st = http_.get(url, body_, sizeof body_, kTimeoutMs, &retry);
    *status = st;
    const std::uint64_t now = net::now_us();
    if (st == 429)
    {
        next_ok = now + jitter_us(static_cast<std::uint64_t>(retry > 0 ? retry : 5) * 1000000);
        log("rooms: the hub asks to slow down (%d s)", retry);
    }
    else if (st < 0 || st >= 500)
    {
        fails_ = fails_ < 6 ? fails_ + 1 : 6;
        next_ok = now + jitter_us(1000000ull << fails_);
        if (st < 0)
            log("rooms: %s: %s", path, http_.error());
    }
    else
        fails_ = 0;
    return st;
}

void RoomsNet::fetch_list(bool more) noexcept
{
    pthread_mutex_lock(&lock_);
    const rooms::Sort s = sort_;
    const rooms::Filter f = filter_;
    const int page = more ? v_.list.next : 0;
    if (!more)
        v_.state = State::loading;
    pthread_mutex_unlock(&lock_);
    char path[320];
    if (!rooms::list_query(path, sizeof path, s, f, page))
        return;
    int st = 0;
    hub_get(path, next_list_, &st);
    if (st == 429)
    {
        // silently again later (the screen keeps what it has)
        pthread_mutex_lock(&lock_);
        if (more)
            want_more_ = true;
        else
            want_list_ = true;
        pthread_mutex_unlock(&lock_);
        return;
    }
    static rooms::List l;
    pthread_mutex_lock(&lock_);
    l = more ? v_.list : rooms::List{};
    pthread_mutex_unlock(&lock_);
    const bool ok = st == 200 && rooms::parse_list(body_, std::strlen(body_), l);
    pthread_mutex_lock(&lock_);
    if (ok)
    {
        v_.list = l;
        v_.state = l.off ? State::off : State::ok;
    }
    else if (!more)
        v_.state = st == 404 ? State::off : State::unreachable;
    rev_.fetch_add(1);
    pthread_mutex_unlock(&lock_);
    log("rooms: list page %d: http %d, %d rooms", page, st, ok ? l.n : -1);
}

void RoomsNet::fetch_friends() noexcept
{
    const char *ids[8];
    rooms::Card cards[8];
    pthread_mutex_lock(&lock_);
    const int n = n_lobbies_;
    for (int i = 0; i < n; ++i)
    {
        cards[i] = rooms::Card{};
        cards[i].group = rooms::Group::friends;
        std::snprintf(cards[i].room, sizeof cards[i].room, "%s", lobbies_[i]);
        std::snprintf(cards[i].host, sizeof cards[i].host, "%s", names_[i]);
        ids[i] = cards[i].room;
    }
    pthread_mutex_unlock(&lock_);
    if (!n)
        return;
    char path[512];
    if (!rooms::status_query(path, sizeof path, ids, n))
        return;
    int st = 0;
    hub_get(path, next_status_, &st);
    if (st != 200)
    {
        pthread_mutex_lock(&lock_);
        next_friends_ = net::now_us() + jitter_us(30000000);
        pthread_mutex_unlock(&lock_);
        return;
    }
    int every = 60;
    const int kept = rooms::parse_status(body_, std::strlen(body_), cards, n, &every);
    pthread_mutex_lock(&lock_);
    v_.n_friends = 0;
    for (int i = 0; i < kept; ++i)
        if (!cards[i].locked)
            v_.friends[v_.n_friends++] = cards[i];
    v_.friends_known = true;
    every_s_ = every < 10 ? 10 : every > 900 ? 900 : every;
    next_friends_ = net::now_us() + jitter_us(static_cast<std::uint64_t>(every_s_) * 1000000);
    rev_.fetch_add(1);
    pthread_mutex_unlock(&lock_);
}

void RoomsNet::fetch_details() noexcept
{
    pthread_mutex_lock(&lock_);
    const rooms::Card c = detail_card_;
    char key[72];
    std::snprintf(key, sizeof key, "%s", v_.detail_key);
    pthread_mutex_unlock(&lock_);
    rooms::Preview p;
    Detail d = Detail::failed;
    int measured = -1;
    if (c.addr[0] && c.port)
    {
        // A direct way (home network / Tailscale): the engine's own preview and a real round trip.
        static char buf[8192];
        const int st = net::http_get(c.addr, c.port, "/api/join/preview", buf, sizeof buf, 3000);
        if (st == 200 && rooms::parse_preview(buf, std::strlen(buf), p))
            d = Detail::ok;
        else
            d = st == 404 ? Detail::old_host : st == 409 ? Detail::gone : Detail::failed;
        static route::Probe probe;
        route::Meter m;
        bool no_socket = false;
        if (probe.open(c.addr, c.port, 2000, m, &no_socket))
        {
            for (int i = 0; i < 5 && run_.load(); ++i)
            {
                probe.check(m, net::now_us());
                probe.poll(m, 200);
            }
            probe.poll(m, 300);
            probe.close();
        }
        const route::Reading r = m.reading(net::now_us());
        measured = r.rtt_ms;
    }
    else if (c.room[0] && c.cap_preview)
    {
        char path[96];
        std::snprintf(path, sizeof path, "/rooms/%s/preview", c.room);
        int st = 0;
        hub_get(path, next_preview_, &st);
        if (st == 429)
        {
            pthread_mutex_lock(&lock_);
            want_detail_ = true;
            pthread_mutex_unlock(&lock_);
            return;
        }
        if (st == 200 && rooms::parse_preview(body_, std::strlen(body_), p))
            d = Detail::ok;
        else
            d = st == 404 ? Detail::gone : st == 503 ? Detail::no_direct : st == 504 ? Detail::no_answer : Detail::failed;
    }
    else
        d = Detail::old_host;
    pthread_mutex_lock(&lock_);
    if (!std::strcmp(key, v_.detail_key))
    {
        v_.detail = d;
        v_.preview = p;
        v_.measured_ms = measured;
        rev_.fetch_add(1);
    }
    pthread_mutex_unlock(&lock_);
}

void RoomsNet::main() noexcept
{
    while (run_.load())
    {
        pthread_mutex_lock(&lock_);
        const bool list = want_list_, more = want_more_, detail = want_detail_;
        const bool friends = n_lobbies_ > 0 && net::now_us() >= next_friends_;
        pthread_mutex_unlock(&lock_);
        const std::uint64_t now = net::now_us();
        if (detail && now >= next_preview_)
        {
            pthread_mutex_lock(&lock_);
            want_detail_ = false;
            pthread_mutex_unlock(&lock_);
            fetch_details();
        }
        else if ((list || more) && now >= next_list_)
        {
            pthread_mutex_lock(&lock_);
            want_list_ = want_more_ = false;
            pthread_mutex_unlock(&lock_);
            fetch_list(!list && more);
        }
        else if (friends && now >= next_status_)
            fetch_friends();
        else
            net::sleep_ms(100);
    }
}
} // namespace play
