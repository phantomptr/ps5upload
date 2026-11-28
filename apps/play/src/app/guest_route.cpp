// PS5Upload Play - the guest's ways to the host's engine (docs/ROUTES.md).
//
// At connect every way is checked at once over the engine's checks socket (a few timed checks and
// one short burst, kRaceMs at most); the best by tier (then the shortest kind, then the round
// trip) is joined. The others stay as backups: checked gently while playing (a check every 2 s
// each, never more than kBackupBytesPerS for all of them together, no bursts), and the way in use
// is measured by the guest socket's own pings and the pictures' arrival. When the selector says
// so (a better tier held for a while, the way in use Poor for a few seconds, or gone) the guest
// joins again on the new way with its resume token beside the old socket; the engine hands it the
// seat (the old socket is told "replaced") and the picture starts again at the next keyframe.
// The hub is never used for any of this: a way through this home's PS5Upload (the bridge) is
// opened only when nothing direct answers, and never checked while it is not in use.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "../core/json.hpp"
#include "../net/route_probe.hpp"
#include "guest.hpp"

#include "../net/seal.hpp"
#include "helper.hpp"
#include "log.hpp"

#include <cstdio>
#include <cstring>

namespace play
{
namespace
{
constexpr unsigned kOpenMs = 1500;
constexpr unsigned kNeedKbps = 15000; // the 1080p stream's default (engine auto_bitrate_kbps)
constexpr std::uint64_t kReopenEveryUs = 30000000;
constexpr unsigned kReopenWireBytes = 600; // a connect, the upgrade and its answer
constexpr int kProbes = route::Selector::kMax; // every way can be raced and probed
constexpr unsigned kBridgeMs = 20000;

// One per way that is checked (the bridge never is); static: they are large and live for the app.
route::Probe g_probes[kProbes];
std::uint64_t g_last_check[kProbes], g_last_open[kProbes];
bool g_no_socket[kProbes];
// the guest socket of the way being changed to (its own small message buffer: only the stages
// are read on it before it is taken over)
std::uint8_t g_next_buf[16 * 1024];
net::WsClient g_next(g_next_buf, sizeof g_next_buf);

struct RaceArg
{
    Guest *guest;
    int way;
    const route::Candidate *c;
    route::Meter *m;
    pthread_mutex_t *lock;
    std::atomic<bool> done;
};
RaceArg g_race[kProbes];
pthread_t g_race_thread[kProbes];
bool g_race_started[kProbes];

// The address to dial for a way checked directly (a Tailscale way through the helper's forward).
bool dial_addr(const route::Candidate &c, char *addr, std::size_t cap, std::uint16_t *port) noexcept
{
    if (c.way == route::Way::helper)
    {
        std::uint16_t local = 0;
        char err[32];
        if (!helper::forward(c.addr, c.port, &local, err, sizeof err))
        {
            log("route: the helper's forward to %s: %s", c.addr, err);
            return false;
        }
        std::snprintf(addr, cap, "%s", "127.0.0.1");
        *port = local;
        return true;
    }
    std::snprintf(addr, cap, "%s", c.addr);
    *port = c.port;
    return true;
}

// At connect: open the way's checks socket, a few checks, the burst.
void *race_main(void *p) noexcept
{
    auto *a = static_cast<RaceArg *>(p);
    route::Probe &pr = g_probes[a->way];
    char addr[64];
    std::uint16_t port = 0;
    route::Meter m; // measured here, copied in under the lock
    m.need_kbps(kNeedKbps);
    bool no_socket = false;
    if (dial_addr(*a->c, addr, sizeof addr, &port) && pr.open(addr, port, kOpenMs, m, &no_socket, a->c->path))
    {
        for (int i = 0; i < 5; ++i)
        {
            pr.check(m, net::now_us());
            pr.poll(m, 60);
        }
        pr.poll(m, 200);
        pr.burst(m, route::kBurstBytes, 600);
    }
    g_no_socket[a->way] = no_socket;
    g_last_open[a->way] = g_last_check[a->way] = net::now_us();
    pthread_mutex_lock(a->lock);
    *a->m = m;
    pthread_mutex_unlock(a->lock);
    a->done = true;
    return nullptr;
}

const char *kind_text(int k) noexcept
{
    return k < 0 ? "none" : route::kind_word(static_cast<route::Kind>(k));
}
} // namespace

void Guest::set_routes(const route::Candidate *c, int n) noexcept
{
    pthread_mutex_lock(&route_lock_);
    sel_.clear();
    for (int i = 0; i < n; ++i)
        sel_.add(c[i]);
    n_routes_ = sel_.size();
    for (int i = 0; i < n_routes_; ++i)
        sel_.meter(i).need_kbps(kNeedKbps);
    pthread_mutex_unlock(&route_lock_);
}

// Opens the guest socket on `way` (-1: the one in use; none set: start()'s address).
bool Guest::route_open(net::WsClient &ws, int way, int *status) noexcept
{
    pthread_mutex_lock(&route_lock_);
    const int i = way >= 0 ? way : sel_.active();
    route::Candidate c;
    const bool have = n_routes_ > 0 && i >= 0;
    if (have)
        c = sel_.at(i);
    pthread_mutex_unlock(&route_lock_);
    if (!have)
        return ws.open(addr_, port_, extras_.path[0] ? extras_.path : "/api/join/ws", 4000, status);
    if (c.way != route::Way::bridge)
    {
        char addr[64];
        std::uint16_t port = 0;
        if (!dial_addr(c, addr, sizeof addr, &port))
            return false;
        return ws.open(addr, port, c.path[0] ? c.path : "/api/join/ws", 4000, status);
    }
    // This home's PS5Upload joins the host's internet room for us (engine join_net/bridge.rs):
    // it answers "connecting", then "open" (or "failed" with why), and the socket is then the
    // guest socket.
    bridge_why_[0] = 0;
    std::snprintf(bridge_at_, sizeof bridge_at_, "%s:%u", c.addr, static_cast<unsigned>(c.port));
    if (!ws.open(c.addr, c.port, "/api/join/bridge", 4000, status))
    {
        // It answered without the bridge (older than 13.0.0: 404), or not at all.
        std::snprintf(bridge_why_, sizeof bridge_why_, "%s", *status > 0 ? "old" : "unreachable");
        log("route: through this home's PS5Upload at %s:%u: failed (%s, http %d)", c.addr,
            static_cast<unsigned>(c.port), bridge_why_, *status);
        return false;
    }
    char m[512];
    json::Writer w(m, sizeof m);
    if (c.lobby)
    {
        // A room from Rooms (docs/ROOMS.md): the bridge knocks on the host's lobby for us; the
        // knock's answers come back here (a challenge is sealed here, the password never goes
        // to the bridge in the clear), then "open".
        w.raw("{\"bridge\":{\"v\":2,\"lobby\":").str(c.room);
        w.raw(",\"knock\":{\"via\":").str(extras_.via[0] ? extras_.via : "rooms");
        w.raw(",\"name\":").str(name_).raw(",\"device\":").str(extras_.device);
        w.raw(",\"client\":\"ps5\",\"ver\":").str(version_).raw("}}}");
    }
    else
        w.raw("{\"bridge\":{\"v\":1,\"room\":").str(c.room).raw(",\"fp\":").str(c.fp).raw("}}");
    if (!w.ok() || !ws.send_text(m))
    {
        ws.close();
        return false;
    }
    std::uint64_t until = net::now_us() + kBridgeMs * 1000ull;
    while (net::now_us() < until && !stop_.load())
    {
        const auto got = ws.next(200);
        if (got == net::WsClient::Got::closed)
        {
            std::snprintf(bridge_why_, sizeof bridge_why_, "%s", "closed");
            return false;
        }
        if (got != net::WsClient::Got::message || ws.op() != ws::kText)
            continue;
        json::Token tok[16];
        json::Doc d(tok, 16);
        if (!d.parse(reinterpret_cast<const char *>(ws.data()), ws.size()))
            continue;
        const int b = d.get(0, "bridge");
        const int k = d.get(0, "knock");
        if (k >= 0)
        {
            // the lobby knock's answers, relayed (PROTOCOL.md "Knocks")
            char gid[40] = {};
            d.str(d.get(0, "gid"), gid, sizeof gid);
            if (d.str_is(k, "challenge"))
            {
                d.str(d.get(0, "c"), challenge_, sizeof challenge_);
                char pk[100] = {};
                if (d.str(d.get(0, "pk"), pk, sizeof pk) && pk[0])
                    std::snprintf(room_pk_, sizeof room_pk_, "%s", pk);
                char blob[1024];
                if (!extras_.password[0] || !seal::seal(room_pk_, challenge_, gid, extras_.password, blob, sizeof blob))
                {
                    std::snprintf(bridge_why_, sizeof bridge_why_, "%s", extras_.password[0] ? "seal" : "password");
                    ws.close();
                    return false;
                }
                char sm[1100];
                json::Writer sw(sm, sizeof sm);
                sw.raw("{\"knock\":\"sealed\",\"blob\":").str(blob).raw("}");
                ws.send_text(sm);
            }
            else if (d.str_is(k, "wait"))
            {
                // the host is being asked: up to the ask's 90 s
                set_phase(GuestView::asking, "");
                until = net::now_us() + 100000000ull;
            }
            else if (d.str_is(k, "no"))
            {
                char why[24] = {};
                d.str(d.get(0, "why"), why, sizeof why);
                const int left = d.get(0, "left");
                pthread_mutex_lock(&lock_);
                view_.tries_left = left >= 0 && !d.is_null(left) ? static_cast<int>(d.integer(left)) : -1;
                pthread_mutex_unlock(&lock_);
                std::snprintf(bridge_why_, sizeof bridge_why_, "knock_%s", why[0] ? why : "no");
                ws.close();
                return false;
            }
            continue;
        }
        if (d.str_is(b, "open"))
        {
            const int r = d.get(0, "rtt_ms");
            if (r >= 0 && !d.is_null(r))
            {
                pthread_mutex_lock(&route_lock_);
                sel_.meter(i).rtt(static_cast<std::uint64_t>(d.integer(r)) * 1000, net::now_us());
                pthread_mutex_unlock(&route_lock_);
            }
            log("route: through this home's PS5Upload: open");
            return true;
        }
        if (d.str_is(b, "failed"))
        {
            char why[24] = {};
            d.str(d.get(0, "why"), why, sizeof why);
            log("route: through this home's PS5Upload: failed (%s)", why);
            std::snprintf(bridge_why_, sizeof bridge_why_, "%s", why[0] ? why : "bad");
            ws.close();
            return false;
        }
    }
    log("route: through this home's PS5Upload: no answer in %u s", kBridgeMs / 1000);
    std::snprintf(bridge_why_, sizeof bridge_why_, "%s", "no_direct");
    ws.close();
    return false;
}

bool Guest::route_is_bridge(int way) noexcept
{
    pthread_mutex_lock(&route_lock_);
    const int i = way >= 0 ? way : sel_.active();
    const bool b = n_routes_ > 0 && i >= 0 && i < n_routes_ && sel_.at(i).way == route::Way::bridge;
    pthread_mutex_unlock(&route_lock_);
    return b;
}

void Guest::route_pick() noexcept
{
    if (!n_routes_)
        return;
    const std::uint64_t t0 = net::now_us();
    int started = 0;
    pthread_mutex_lock(&route_lock_);
    const int n = n_routes_;
    for (int i = 0; i < n && i < kProbes; ++i)
    {
        g_race_started[i] = false;
        if (sel_.at(i).way == route::Way::bridge)
            continue;
        RaceArg &a = g_race[i];
        a.guest = this;
        a.way = i;
        a.c = &sel_.at(i);
        a.m = &sel_.meter(i);
        a.lock = &route_lock_;
        a.done = false;
        g_race_started[i] = pthread_create(&g_race_thread[i], nullptr, race_main, &a) == 0;
        started += g_race_started[i];
    }
    pthread_mutex_unlock(&route_lock_);
    // Until every way said, kRaceMs, or a home-network way is already Excellent.
    while (started && net::now_us() - t0 < route::kRaceMs * 1000ull && !stop_.load())
    {
        bool all = true, lan_best = false;
        pthread_mutex_lock(&route_lock_);
        for (int i = 0; i < n && i < kProbes; ++i)
        {
            if (!g_race_started[i])
                continue;
            all = all && g_race[i].done.load();
            lan_best = lan_best || (g_race[i].done.load() && sel_.at(i).kind == route::Kind::lan &&
                                    sel_.meter(i).tier(net::now_us()) == route::Tier::excellent);
        }
        pthread_mutex_unlock(&route_lock_);
        if (all || lan_best)
            break;
        net::sleep_ms(20);
    }
    const std::uint64_t now = net::now_us();
    pthread_mutex_lock(&route_lock_);
    int pick = sel_.best(now);
    const char *why = "start";
    if (pick < 0)
    {
        // Nothing direct answered: this home's PS5Upload, else the first way (its connect says why).
        for (int i = 0; i < n && pick < 0; ++i)
            if (sel_.at(i).way == route::Way::bridge)
                pick = i, why = "bridge";
        if (pick < 0)
            pick = 0;
    }
    sel_.use(pick, now, why);
    char line[400];
    int k = std::snprintf(line, sizeof line, "route: picked %s after %u ms;", kind_text(static_cast<int>(sel_.at(pick).kind)),
                          static_cast<unsigned>((now - t0) / 1000));
    for (int i = 0; i < n && k > 0 && k < static_cast<int>(sizeof line); ++i)
    {
        const route::Reading r = sel_.meter(i).reading(now);
        k += std::snprintf(line + k, sizeof line - static_cast<std::size_t>(k), " %s%s %s rtt %d jitter %d head %d%s",
                           kind_text(static_cast<int>(sel_.at(i).kind)),
                           sel_.at(i).way == route::Way::helper ? "(helper)" : sel_.at(i).way == route::Way::bridge ? "(bridge)" : "",
                           route::tier_word(classify(r)), r.rtt_ms, r.jitter_ms, r.headroom_pct,
                           i < kProbes && g_no_socket[i] ? " old-engine" : "");
    }
    pthread_mutex_unlock(&route_lock_);
    log("%s", line);
    route_view();
    if (!route_run_.exchange(true) && pthread_create(&route_thread_, nullptr, route_entry, this) != 0)
    {
        route_thread_ = {};
        route_run_ = false;
    }
}

void *Guest::route_entry(void *self) noexcept
{
    static_cast<Guest *>(self)->route_main();
    return nullptr;
}

// The backups while playing, and when to change way.
void Guest::route_main() noexcept
{
    for (int i = 0; i < kProbes; ++i)
        if (g_race_started[i])
            pthread_join(g_race_thread[i], nullptr), g_race_started[i] = false;
    route::Pacer pacer(route::kBackupBytesPerS, route::kBackupBurst);
    std::uint64_t last_view = 0;
    while (route_run_.load())
    {
        net::sleep_ms(100);
        const std::uint64_t now = net::now_us();
        pthread_mutex_lock(&route_lock_);
        const int n = n_routes_ < kProbes ? n_routes_ : kProbes;
        const int active = sel_.active();
        route::Candidate cs[kProbes];
        for (int i = 0; i < n; ++i)
            cs[i] = sel_.at(i);
        pthread_mutex_unlock(&route_lock_);
        for (int i = 0; i < n; ++i)
        {
            route::Probe &p = g_probes[i];
            if (cs[i].way == route::Way::bridge)
                continue;
            if (i == active)
            {
                // measured by the guest socket itself
                if (p.is_open())
                    p.close();
                continue;
            }
            route::Meter m;
            pthread_mutex_lock(&route_lock_);
            m = sel_.meter(i);
            pthread_mutex_unlock(&route_lock_);
            if (!p.is_open())
            {
                if (g_no_socket[i] || now - g_last_open[i] < kReopenEveryUs || !pacer.take(now, kReopenWireBytes))
                    continue;
                g_last_open[i] = now;
                char addr[64];
                std::uint16_t port = 0;
                bool no_socket = false;
                if (!dial_addr(cs[i], addr, sizeof addr, &port) || !p.open(addr, port, kOpenMs, m, &no_socket, cs[i].path))
                {
                    g_no_socket[i] = no_socket;
                    m.lost_connection(now);
                }
            }
            else if (now - g_last_check[i] >= route::kBackupEveryUs && pacer.take(now, route::kCheckWireBytes))
            {
                g_last_check[i] = now;
                p.check(m, now);
            }
            if (p.is_open() && !p.poll(m, 0))
                m.lost_connection(now);
            pthread_mutex_lock(&route_lock_);
            sel_.meter(i) = m;
            pthread_mutex_unlock(&route_lock_);
        }
        if (joined_.load() && switch_to_.load() < 0)
        {
            pthread_mutex_lock(&route_lock_);
            const auto d = sel_.decide(now);
            pthread_mutex_unlock(&route_lock_);
            if (d.to >= 0 && d.to != active)
            {
                log("route: %s from %s to %s", d.why, kind_text(static_cast<int>(cs[active < 0 ? 0 : active].kind)),
                    kind_text(static_cast<int>(sel_.at(d.to).kind)));
                pthread_mutex_lock(&lock_);
                std::snprintf(view_.route_why, sizeof view_.route_why, "%s", d.why);
                pthread_mutex_unlock(&lock_);
                switch_to_ = d.to;
            }
        }
        if (now - last_view >= 500000)
        {
            last_view = now;
            route_view();
        }
    }
    for (auto &p : g_probes)
        p.close();
}

void Guest::route_stop() noexcept
{
    if (route_run_.exchange(false) && route_thread_)
    {
        pthread_join(route_thread_, nullptr);
        route_thread_ = {};
    }
    // The race threads are the routed guest's (the first player's): another controller's seat
    // has no ways of its own and must not join threads that are not its.
    if (!n_routes_)
        return;
    for (int i = 0; i < kProbes; ++i)
        if (g_race_started[i])
            pthread_join(g_race_thread[i], nullptr), g_race_started[i] = false;
}

// On the guest thread: a change the selector asked for.
void Guest::route_tick(std::uint64_t now) noexcept
{
    (void)now;
    const int to = switch_to_.load();
    if (to < 0 || !joined_.load())
        return;
    char why[12];
    pthread_mutex_lock(&lock_);
    std::snprintf(why, sizeof why, "%s", view_.route_why[0] ? view_.route_why : "upgrade");
    pthread_mutex_unlock(&lock_);
    route_switch(to, why);
    switch_to_ = -1;
}

// Joins again on way `to` beside the open socket (make before break) and takes it over.
bool Guest::route_switch(int to, const char *why) noexcept
{
    const std::uint64_t t0 = net::now_us();
    int status = 0;
    if (!route_open(g_next, to, &status))
    {
        log("route: could not open the %s way (%s)", why, g_next.error());
        pthread_mutex_lock(&route_lock_);
        sel_.meter(to).lost_connection(net::now_us());
        pthread_mutex_unlock(&route_lock_);
        return false;
    }
    char hello[768];
    make_hello(hello, sizeof hello, true, to);
    if (!g_next.send_text(hello))
    {
        g_next.close();
        return false;
    }
    while (net::now_us() - t0 < 3000000 && !stop_.load())
    {
        const auto got = g_next.next(100);
        if (got == net::WsClient::Got::closed)
            break;
        if (got != net::WsClient::Got::message || g_next.op() != ws::kText)
            continue;
        proto::GuestEvent e;
        if (!proto::parse_guest(reinterpret_cast<const char *>(g_next.data()), g_next.size(), e))
            continue;
        if (e.type == proto::GuestEvent::joined)
        {
            // keep the message: taking the socket over resets the reader
            char joined[512];
            const std::size_t jn = g_next.size() < sizeof joined ? g_next.size() : sizeof joined - 1;
            std::memcpy(joined, g_next.data(), jn);
            joined[jn] = 0;
            ws_.adopt(g_next);
            const std::uint64_t now = net::now_us();
            pthread_mutex_lock(&route_lock_);
            const int from = sel_.active();
            if (from >= 0 && sel_.at(from).way == route::Way::bridge)
                sel_.meter(from).lost_connection(now); // not checked while not in use
            sel_.use(to, now, why);
            pthread_mutex_unlock(&route_lock_);
            handle_text(joined, jn);
            ws_.send_text("{\"keyframe\":true}");
            pthread_mutex_lock(&lock_);
            view_.route_changed_us = now;
            pthread_mutex_unlock(&lock_);
            log("route: now on %s (%s) in %u ms", kind_text(static_cast<int>(sel_.at(to).kind)), why,
                static_cast<unsigned>((now - t0) / 1000));
            route_view();
            return true;
        }
        if (e.type == proto::GuestEvent::refused || e.type == proto::GuestEvent::bye)
            break;
    }
    log("route: the %s way did not take the seat", why);
    g_next.close();
    return false;
}

// The way in use dropped (or its connect failed): the best other way now.
void Guest::route_lost(const char *why) noexcept
{
    if (n_routes_ < 2)
        return;
    const std::uint64_t now = net::now_us();
    pthread_mutex_lock(&route_lock_);
    const int from = sel_.active();
    if (from >= 0)
        sel_.meter(from).lost_connection(now);
    int to = sel_.best(now, from);
    if (to < 0)
        for (int i = 0; i < n_routes_ && to < 0; ++i)
            if (i != from && sel_.at(i).way == route::Way::bridge)
                to = i;
    // nothing else either: try the same way again every other time
    if (to < 0 || (++lost_tries_ % 3 == 0))
        to = from;
    if (to >= 0 && to != from)
        sel_.use(to, now, "lost");
    pthread_mutex_unlock(&route_lock_);
    if (to != from)
    {
        log("route: %s on the way in use; trying %s", why, kind_text(static_cast<int>(sel_.at(to).kind)));
        pthread_mutex_lock(&lock_);
        view_.route_changed_us = now;
        std::snprintf(view_.route_why, sizeof view_.route_why, "%s", "lost");
        pthread_mutex_unlock(&lock_);
    }
}

void Guest::route_sent(std::uint32_t id) noexcept
{
    if (!n_routes_)
        return;
    pthread_mutex_lock(&route_lock_);
    if (sel_.active() >= 0)
        sel_.meter(sel_.active()).sent(id, net::now_us());
    pthread_mutex_unlock(&route_lock_);
}

void Guest::route_answer(std::uint32_t id) noexcept
{
    if (!n_routes_)
        return;
    pthread_mutex_lock(&route_lock_);
    if (sel_.active() >= 0)
        sel_.meter(sel_.active()).answered(id, net::now_us());
    pthread_mutex_unlock(&route_lock_);
}

void Guest::route_picture(std::uint64_t engine_us) noexcept
{
    if (!n_routes_)
        return;
    pthread_mutex_lock(&route_lock_);
    if (sel_.active() >= 0)
        sel_.meter(sel_.active()).picture(engine_us, net::now_us());
    pthread_mutex_unlock(&route_lock_);
}

void Guest::route_heard() noexcept
{
    if (!n_routes_)
        return;
    pthread_mutex_lock(&route_lock_);
    if (sel_.active() >= 0)
        sel_.meter(sel_.active()).heard(net::now_us());
    pthread_mutex_unlock(&route_lock_);
}

// The engine's status: its count of pictures it skipped for us (the way fell behind).
void Guest::route_status(const char *t, std::size_t n) noexcept
{
    if (!n_routes_)
        return;
    json::Token tok[96];
    json::Doc d(tok, 96);
    if (!d.parse(t, n))
        return;
    const int st = d.get(0, "status");
    const int sk = d.get(st, "skipped");
    if (sk < 0 || d.is_null(sk))
        return;
    const std::int64_t s = d.integer(sk);
    if (last_skipped_ >= 0 && s > last_skipped_)
    {
        pthread_mutex_lock(&route_lock_);
        if (sel_.active() >= 0)
            sel_.meter(sel_.active()).behind(net::now_us());
        pthread_mutex_unlock(&route_lock_);
    }
    last_skipped_ = s;
}

void Guest::route_view() noexcept
{
    if (!n_routes_)
        return;
    const std::uint64_t now = net::now_us();
    pthread_mutex_lock(&route_lock_);
    const int a = sel_.active();
    route::Reading r;
    route::Tier t = route::Tier::down, bt = route::Tier::down;
    int backups = 0;
    std::int8_t kind = -1;
    bool bridged = false;
    if (a >= 0)
    {
        r = sel_.meter(a).reading(now);
        t = classify(r);
        kind = static_cast<std::int8_t>(sel_.at(a).kind);
        bridged = sel_.at(a).way == route::Way::bridge;
    }
    for (int i = 0; i < n_routes_; ++i)
    {
        if (i == a)
            continue;
        const route::Tier x = sel_.meter(i).tier(now);
        if (x != route::Tier::down)
            ++backups;
        bt = x > bt ? x : bt;
    }
    const int changes = sel_.changes();
    pthread_mutex_unlock(&route_lock_);
    pthread_mutex_lock(&lock_);
    view_.route = kind;
    view_.tier = static_cast<std::uint8_t>(t);
    view_.bridged = bridged;
    view_.backups = static_cast<std::uint8_t>(backups);
    view_.backup_tier = static_cast<std::uint8_t>(bt);
    view_.route_rtt_ms = r.rtt_ms;
    view_.route_jitter_ms = r.jitter_ms;
    view_.route_loss = r.loss_permille;
    view_.route_switches = changes;
    pthread_mutex_unlock(&lock_);
}

// The members the host's panel reads (engine join.rs PageReport): a leading comma, or "".
void Guest::route_stats(char *out, std::size_t cap) noexcept
{
    out[0] = 0;
    if (!n_routes_)
        return;
    route_view();
    const GuestView v = view();
    if (v.route < 0)
        return;
    json::Writer w(out, cap);
    w.raw(",\"route\":").str(route::kind_word(static_cast<route::Kind>(v.route)));
    w.raw(",\"tier\":").str(route::tier_word(static_cast<route::Tier>(v.tier)));
    if (v.route_rtt_ms >= 0)
        w.raw(",\"route_rtt_ms\":").num(static_cast<long long>(v.route_rtt_ms));
    if (v.route_jitter_ms >= 0)
        w.raw(",\"route_jitter_ms\":").num(static_cast<long long>(v.route_jitter_ms));
    if (v.route_loss >= 0)
        w.raw(",\"route_loss\":").num(static_cast<long long>(v.route_loss));
    w.raw(",\"route_switches\":").num(static_cast<long long>(v.route_switches));
    w.raw(",\"backups\":").num(static_cast<long long>(v.backups));
    if (v.backups)
        w.raw(",\"backup_tier\":").str(route::tier_word(static_cast<route::Tier>(v.backup_tier)));
    if (!w.ok())
        out[0] = 0;
}
} // namespace play
