// PS5Upload Play - the waiting room on the hub (docs/EASY_JOIN.md).
// SPDX-License-Identifier: GPL-3.0-or-later
#include "waiting_room.hpp"

#include "../core/json.hpp"
#include "helper.hpp"
#include "log.hpp"

#include <cstdio>
#include <cstring>
#include <fcntl.h>
#include <unistd.h>

namespace play
{
namespace
{
// Reads the room's secret, or makes one (32 random bytes, base64url) and keeps it (0600).
bool load_secret(const char *path, char *out, std::size_t cap) noexcept
{
    char buf[64] = {};
    const int fd = path && *path ? ::open(path, O_RDONLY) : -1;
    if (fd >= 0)
    {
        const long n = ::read(fd, buf, sizeof buf - 1);
        ::close(fd);
        std::size_t len = n > 0 ? static_cast<std::size_t>(n) : 0;
        while (len && (buf[len - 1] == '\n' || buf[len - 1] == '\r'))
            --len;
        buf[len] = 0;
        if (len == 43 && len < cap)
        {
            std::memcpy(out, buf, len + 1);
            return true;
        }
    }
    std::uint8_t raw[32];
    log("room: making the room's secret");
    if (!net::secure_random(raw, sizeof raw))
    {
        log("room: no random bytes for the secret");
        return false;
    }
    log("room: secret made");
    invite::b64url(raw, sizeof raw, out, cap);
    if (path && *path)
    {
        const int w = ::open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
        if (w >= 0)
        {
            (void)!::write(w, out, std::strlen(out));
            ::close(w);
        }
    }
    return true;
}

bool get_str(const json::Doc &d, int obj, const char *k, char *out, std::size_t cap) noexcept
{
    return d.str(d.get(obj, k), out, cap);
}
} // namespace

void WaitingRoom::init(const char *secret_path, const char *hub_url) noexcept
{
    if (run_.load())
        return; // once
    std::snprintf(url_, sizeof url_, "%s", hub_url && *hub_url ? hub_url : kHubUrl);
    if (!invite::hub_host(url_, hub_host_, sizeof hub_host_) || !load_secret(secret_path, secret_, sizeof secret_))
    {
        log("room: no waiting room (%s)", secret_[0] ? "hub url" : "no randomness");
        hub_host_[0] = 0;
        return;
    }
    invite::room_for_secret(secret_, room_);
    // A new nonce each run: only the TV in front of the person shows it.
    std::uint8_t r[12];
    if (net::secure_random(r, sizeof r))
        invite::b64url(r, sizeof r, nonce_, sizeof nonce_);
    run_ = true;
    if (pthread_create(&thread_, nullptr, main, this) != 0)
    {
        run_ = false;
        thread_ = {};
    }
}

void WaitingRoom::arm() noexcept
{
    if (!room_[0])
        return;
    const std::uint64_t now = net::now_us();
    // Already open: keep its stretch (the screen asks each frame).
    if (armed_until_.load() > now)
        return;
    armed_until_ = now + kOpenForUs;
    pthread_mutex_lock(&lock_);
    view_.expired = false;
    pthread_mutex_unlock(&lock_);
}

void WaitingRoom::disarm() noexcept
{
    armed_until_ = 0;
}

void WaitingRoom::stop() noexcept
{
    disarm();
    if (run_.exchange(false) && thread_)
    {
        pthread_join(thread_, nullptr);
        thread_ = {};
    }
}

WaitingRoom::View WaitingRoom::view() noexcept
{
    pthread_mutex_lock(&lock_);
    View v = view_;
    pthread_mutex_unlock(&lock_);
    return v;
}

bool WaitingRoom::take(invite::Invite &out) noexcept
{
    pthread_mutex_lock(&lock_);
    const bool any = queued_ > 0;
    if (any)
    {
        out = queue_[0];
        for (int i = 1; i < queued_; ++i)
            queue_[i - 1] = queue_[i];
        --queued_;
    }
    pthread_mutex_unlock(&lock_);
    return any;
}

bool WaitingRoom::pick_route(invite::Invite &inv) noexcept
{
    for (int i = 0; i < inv.n_routes; ++i)
    {
        invite::Route r = inv.routes[i];
        std::snprintf(r.dial_addr, sizeof r.dial_addr, "%s", r.addr);
        r.dial_port = r.port;
        if (r.tailnet)
        {
            // Over Tailscale the app dials the helper's forward on this console.
            std::uint16_t local = 0;
            char err[32] = {};
            if (!helper::forward(r.addr, r.port, &local, err, sizeof err))
                continue;
            std::snprintf(r.dial_addr, sizeof r.dial_addr, "%s", "127.0.0.1");
            r.dial_port = local;
        }
        char body[256];
        // Any answer from an engine there will do (its guest socket refuses a plain GET at once).
        if (net::http_get(r.dial_addr, r.dial_port, r.path, body, sizeof body, 1500) > 0)
        {
            inv.routes[0] = r;
            inv.n_routes = 1;
            return true;
        }
    }
    inv.n_routes = 0;
    return false;
}

void WaitingRoom::answer(net::HubLink &l, const char *gid, bool ok, const char *why) noexcept
{
    char m[256];
    json::Writer w(m, sizeof m);
    w.raw("{\"v\":1,\"t\":\"signal\",\"to\":").str(gid).raw(",\"data\":{\"invite_ok\":").boolean(ok);
    if (why)
        w.raw(",\"why\":").str(why);
    w.raw("}}");
    if (w.ok())
        l.send_json(m);
    // That guest is done: it leaves by itself; the room stays for the next.
}

void *WaitingRoom::main(void *self) noexcept
{
    static_cast<WaitingRoom *>(self)->run();
    return nullptr;
}

void WaitingRoom::run() noexcept
{
    unsigned backoff_ms = 2000;
    while (run_.load())
    {
        if (armed_until_.load() <= net::now_us())
        {
            pthread_mutex_lock(&lock_);
            if (view_.state != View::off)
                view_.expired = armed_until_.load() != 0;
            view_.state = View::off;
            pthread_mutex_unlock(&lock_);
            net::sleep_ms(200);
            continue;
        }
        const std::uint64_t t0 = net::now_us();
        session();
        if (!run_.load() || armed_until_.load() <= net::now_us())
            continue;
        // It dropped while wanted: again after a while (longer each time, at most 30 s).
        if (net::now_us() - t0 > 60000000)
            backoff_ms = 2000;
        for (unsigned i = 0; i < backoff_ms / 100 && run_.load() && armed_until_.load() > net::now_us(); ++i)
            net::sleep_ms(100);
        backoff_ms = backoff_ms * 2 > 30000 ? 30000 : backoff_ms * 2;
    }
}

void WaitingRoom::session() noexcept
{
    pthread_mutex_lock(&lock_);
    view_.state = View::connecting;
    std::snprintf(view_.room, sizeof view_.room, "%s", room_);
    std::snprintf(view_.hub_host, sizeof view_.hub_host, "%s", hub_host_);
    invite::qr_link(view_.qr, sizeof view_.qr, kPage, room_, nonce_, hub_host_);
    pthread_mutex_unlock(&lock_);
    static net::HubLink link; // one room, one link (its buffers are large)
    const std::uint64_t t0 = net::now_us();
    auto set = [&](View::State s) {
        pthread_mutex_lock(&lock_);
        view_.state = s;
        pthread_mutex_unlock(&lock_);
    };
    if (!link.open(url_, 6000))
    {
        log("room: the hub can't be reached (%s)", link.error());
        set(View::offline);
        return;
    }
    char m[256];
    std::snprintf(m, sizeof m, "{\"v\":1,\"t\":\"host\",\"room\":\"%s\",\"secret\":\"%s\"}", room_, secret_);
    link.send_json(m);
    bool hosted = false;
    while (run_.load() && armed_until_.load() > net::now_us())
    {
        const auto got = link.next(500);
        if (got == net::HubLink::Got::closed)
        {
            log("room: the hub closed the room's link (%s)", link.error());
            set(View::offline);
            return;
        }
        if (got != net::HubLink::Got::message)
            continue;
        json::Token toks[48];
        json::Doc d(toks, 48);
        if (!d.parse(link.text(), link.size()) || d.type(0) != json::Type::object)
            continue;
        const int t = d.get(0, "t");
        if (d.str_is(t, "hosted"))
        {
            hosted = true;
            set(View::ready);
            // The room's id is on the TV anyway (the QR code); the secret and the nonce never go
            // to the log.
            log("room: open on the hub (room %s)", room_);
        }
        else if (d.str_is(t, "error"))
        {
            char why[32] = {};
            get_str(d, 0, "why", why, sizeof why);
            log("room: the hub said %s", why);
            if (!std::strcmp(why, "no_peer"))
                continue;
            set(View::offline);
            link.close();
            return;
        }
        else if (d.str_is(t, "signal") && hosted)
        {
            char gid[24] = {};
            get_str(d, 0, "from", gid, sizeof gid);
            std::size_t dl = 0;
            const char *data = d.raw(d.get(0, "data"), &dl);
            invite::Invite inv;
            if (!gid[0] || !data || !invite::parse_handoff(data, dl, inv))
            {
                if (gid[0])
                    answer(link, gid, false, "link");
                continue;
            }
            if (busy_.load())
            {
                answer(link, gid, false, "busy");
                continue;
            }
            const bool from_tv = inv.nonce[0] && nonce_[0] && !std::strcmp(inv.nonce, nonce_);
            if (!from_tv)
                inv.nonce[0] = 0; // a push: the screen asks first
            // The routes before answering: the phone hears at once whether this PS5 can play it.
            const bool reach = pick_route(inv);
            answer(link, gid, reach, reach ? nullptr : "route");
            log("room: an invite (%s, %s)", from_tv ? "the phone in front of this TV" : "sent by a host",
                reach ? "reachable" : "not reachable from here");
            pthread_mutex_lock(&lock_);
            if (queued_ == 2)
            {
                queue_[0] = queue_[1];
                queued_ = 1;
            }
            queue_[queued_++] = inv;
            pthread_mutex_unlock(&lock_);
        }
    }
    // Done (the screen went, or the stretch is over): close the room for good.
    link.send_json("{\"v\":1,\"t\":\"leave\"}");
    log("room: closed after %llu s (hub load: %u messages in, %u out, %llu bytes in, %llu out)",
        static_cast<unsigned long long>((net::now_us() - t0) / 1000000), link.msgs_in, link.msgs_out,
        static_cast<unsigned long long>(link.bytes_in), static_cast<unsigned long long>(link.bytes_out));
    link.close();
}
} // namespace play
