// PS5Upload Play - the guest: joining a friend's Play Together over the engine's guest socket.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "guest.hpp"

#include "../core/json.hpp"
#include "log.hpp"

#include <cstdio>
#include <cstring>

namespace play
{
namespace
{
constexpr std::uint64_t kResumeWindowUs = 58ull * 1000000; // the engine keeps a seat 60 s
constexpr std::uint64_t kPingEveryUs = 2000000;

void copy(char *dst, std::size_t cap, const char *src) noexcept
{
    std::snprintf(dst, cap, "%s", src ? src : "");
}
} // namespace

Guest::Guest(GuestSinks &sinks, std::uint8_t *msg_buf, std::size_t msg_cap) noexcept
    : sinks_{sinks}, ws_{msg_buf, msg_cap}
{
    pthread_mutex_init(&lock_, nullptr);
    pthread_mutex_init(&route_lock_, nullptr);
}

bool Guest::start(const char *addr, std::uint16_t port, const char *code, const char *name,
                  const char *version) noexcept
{
    if (running_.load())
        return false;
    join();
    copy(addr_, sizeof addr_, addr);
    port_ = port;
    copy(code_, sizeof code_, code);
    copy(name_, sizeof name_, name);
    copy(version_, sizeof version_, version);
    token_[0] = 0;
    stop_ = leave_ = nudge_ = false;
    joined_ = raw_pad_ = false;
    have_offset_ = false;
    switch_to_ = -1;
    last_skipped_ = -1;
    lost_tries_ = 0;
    bridge_why_[0] = 0;
    pthread_mutex_lock(&lock_);
    view_ = GuestView{};
    std::snprintf(view_.engine, sizeof view_.engine, "%s:%u", addr, static_cast<unsigned>(port));
    pthread_mutex_unlock(&lock_);
    running_ = true;
    if (pthread_create(&thread_, nullptr, entry, this) != 0)
    {
        running_ = false;
        return false;
    }
    return true;
}

void Guest::leave() noexcept
{
    leave_ = true;
    stop_ = true;
}

void Guest::join() noexcept
{
    if (thread_)
    {
        pthread_join(thread_, nullptr);
        thread_ = {};
    }
}

GuestView Guest::view() noexcept
{
    pthread_mutex_lock(&lock_);
    GuestView v = view_;
    pthread_mutex_unlock(&lock_);
    return v;
}

void Guest::set_phase(GuestView::Phase p, const char *why) noexcept
{
    pthread_mutex_lock(&lock_);
    if (view_.phase != p)
        view_.phase_since_us = net::now_us();
    view_.phase = p;
    copy(view_.why, sizeof view_.why, why);
    pthread_mutex_unlock(&lock_);
    log("guest: phase %d%s%s", static_cast<int>(p), why && *why ? " why " : "", why ? why : "");
}

bool Guest::send_pad(const proto::PadState &s) noexcept
{
    if (!joined_.load())
        return false;
    std::uint8_t f[proto::kPadFrameMax];
    const std::size_t n = proto::pad_frame(f, sizeof f, pad_seq_.fetch_add(1), s, raw_pad_.load());
    if (!n || !ws_.send(ws::kBinary, f, n))
        return false;
    pthread_mutex_lock(&lock_);
    ++view_.pads_sent;
    pthread_mutex_unlock(&lock_);
    return true;
}

void *Guest::entry(void *self) noexcept
{
    static_cast<Guest *>(self)->run();
    return nullptr;
}

bool Guest::connect_and_hello(bool resume) noexcept
{
    int status = 0;
    if (!route_open(ws_, -1, &status))
    {
        log("guest: connect %s:%u failed: %s (http %d)", addr_, static_cast<unsigned>(port_), ws_.error(), status);
        return false;
    }
    char hello[768];
    make_hello(hello, sizeof hello, resume, -1);
    if (!ws_.send_text(hello))
    {
        ws_.close();
        return false;
    }
    log("guest: hello sent (%s, %s)", resume ? "resume" : code_[0] ? "code" : "nearby", addr_);
    return true;
}

// The hello for a join or a resume, on any way (a change of way sends the same one, with the token).
std::size_t Guest::make_hello(char *out, std::size_t cap, bool resume, int way) noexcept
{
    // main10: HDR pictures are shown, tone-mapped to SDR (display.cpp, core/hdr.cpp).
    const proto::ClientCaps caps{version_, true, true, true, true};
    // Coming back: the token, and the code too in case the seat is gone but the invite still works.
    proto::HelloExtras x;
    x.invite = extras_.invite;
    // Play again through this home's PS5Upload: the host's engine takes no pass from an internet
    // room, so the last internet invite's key asks there (the pass still goes with it).
    if (!extras_.invite[0] && extras_.bridge_invite[0] && route_is_bridge(way))
        x.invite = extras_.bridge_invite;
    x.again = extras_.again;
    x.remember = extras_.remember;
    x.inbox_room = extras_.inbox_room;
    x.inbox_hub = extras_.inbox_hub;
    return proto::hello_ex(out, cap, code_, resume ? token_ : nullptr, name_, caps, x);
}

void Guest::run() noexcept
{
    set_phase(GuestView::connecting, "");
    route_pick();
    bool ever_joined = false;
    bool final_state = false;
    while (!final_state)
    {
        if (stop_.load() && !leave_.load())
            break;
        const bool resume = token_[0] != 0;
        if (leave_.load() && !ws_.is_open())
        {
            set_phase(GuestView::ended, "left");
            break;
        }
        if (!connect_and_hello(resume))
        {
            if (!ever_joined)
            {
                const char *e = ws_.error();
                if (bridge_why_[0] && route_is_bridge(-1))
                {
                    // Through this home's PS5Upload: its own reason, for a plain message.
                    char why[24];
                    std::snprintf(why, sizeof why, "bridge_%s", bridge_why_);
                    pthread_mutex_lock(&lock_);
                    std::snprintf(view_.bridge_at, sizeof view_.bridge_at, "%s", bridge_at_);
                    pthread_mutex_unlock(&lock_);
                    set_phase(GuestView::failed, why);
                    break;
                }
                set_phase(GuestView::failed, !std::strcmp(e, "busy") ? "busy"
                                             : !std::strcmp(e, "upgrade") ? "old_engine"
                                                                          : "unreachable");
                break;
            }
            route_lost("connect");
            if (net::now_us() - dropped_at_ > kResumeWindowUs)
            {
                set_phase(GuestView::ended, "lost");
                break;
            }
            for (int i = 0; i < 10 && !stop_.load() && !nudge_.exchange(false); ++i)
                net::sleep_ms(100);
            continue;
        }
        std::uint64_t last_ping = 0;
        for (;;)
        {
            if (leave_.load())
            {
                if (joined_.load())
                    ws_.send_text("{\"leave\":1}");
                net::sleep_ms(50);
                ws_.close();
                joined_ = false;
                set_phase(GuestView::ended, "left");
                final_state = true;
                break;
            }
            const auto got = ws_.next(100);
            if (got == net::WsClient::Got::message)
            {
                if (ws_.op() == ws::kText)
                    handle_text(reinterpret_cast<const char *>(ws_.data()), ws_.size());
                else
                    handle_binary(ws_.data(), ws_.size());
                const auto p = view().phase;
                if (p == GuestView::refused || p == GuestView::ended)
                {
                    final_state = true;
                    ws_.close();
                    break;
                }
                if (p == GuestView::joined && !ever_joined)
                    ever_joined = true;
            }
            else if (got == net::WsClient::Got::closed)
                break;
            const std::uint64_t now = net::now_us();
            if (joined_.load() && now - last_ping >= kPingEveryUs)
            {
                last_ping = now;
                ping_sent_us_ = now;
                route_sent(static_cast<std::uint32_t>(now / 1000));
                char m[1400];
                std::snprintf(m, sizeof m, "{\"ping\":%llu}", static_cast<unsigned long long>(now / 1000));
                ws_.send_text(m);
                char st[640];
                sinks_.stats(st, sizeof st);
                char rs[320];
                route_stats(rs, sizeof rs);
                std::snprintf(m, sizeof m, "{\"stats\":{%s%s}}", st, rs);
                ws_.send_text(m);
            }
            if (joined_.load() && sinks_.wants_keyframe() && key_ask_.take(now))
                ws_.send_text("{\"keyframe\":true}");
            route_tick(now);
        }
        if (final_state)
            break;
        // The socket dropped.
        if (joined_.exchange(false))
        {
            sinks_.end();
            dropped_at_ = net::now_us();
            route_lost("drop");
            set_phase(GuestView::reconnecting, ws_.error());
            continue;
        }
        if (ever_joined)
        {
            // dropped again while coming back: keep trying inside the window
            if (net::now_us() - dropped_at_ > kResumeWindowUs)
            {
                set_phase(GuestView::ended, "lost");
                break;
            }
            net::sleep_ms(500);
            continue;
        }
        // Closed before the host answered.
        set_phase(GuestView::failed, "closed");
        break;
    }
    route_stop();
    if (joined_.exchange(false))
        sinks_.end();
    ws_.close();
    running_ = false;
}

void Guest::handle_text(const char *t, std::size_t n) noexcept
{
    route_heard();
    proto::GuestEvent e;
    if (!proto::parse_guest(t, n, e))
    {
        log("guest: unread message (%zu bytes): %.*s", n, static_cast<int>(n > 160 ? 160 : n), t);
        return;
    }
    switch (e.type)
    {
    case proto::GuestEvent::asking:
        set_phase(GuestView::asking, "");
        break;
    case proto::GuestEvent::waiting:
        set_phase(GuestView::waiting, e.why);
        break;
    case proto::GuestEvent::joined:
    {
        copy(token_, sizeof token_, e.token);
        // An engine that read our caps says so with codec_warning in "joined" (13.0.0 on);
        // older ones read pad buttons as the apps' bits.
        raw_pad_ = e.caps_known;
        pthread_mutex_lock(&lock_);
        view_.player = e.player;
        view_.hevc = e.hevc;
        copy(view_.codec_warning, sizeof view_.codec_warning, e.codec_warning);
        if (e.friend_pass[0])
            copy(view_.friend_pass, sizeof view_.friend_pass, e.friend_pass);
        pthread_mutex_unlock(&lock_);
        log("guest: joined as player %d (hevc %d, engine reads caps %d)", e.player, e.hevc, e.caps_known);
        sinks_.begin(e.hevc);
        joined_ = true;
        set_phase(GuestView::joined, "");
        break;
    }
    case proto::GuestEvent::refused:
        // A seat that ran out while we were away is the same as being let go.
        set_phase(token_[0] && !std::strcmp(e.why, "expired") ? GuestView::ended : GuestView::refused,
                  token_[0] && !std::strcmp(e.why, "expired") ? "lost" : e.why);
        break;
    case proto::GuestEvent::status:
        route_status(t, n);
        pthread_mutex_lock(&lock_);
        view_.player = e.player ? e.player : view_.player;
        view_.rtt_ms = e.rtt_ms;
        copy(view_.host, sizeof view_.host, e.host);
        copy(view_.plays_as, sizeof view_.plays_as, e.plays_as);
        copy(view_.host_user, sizeof view_.host_user, e.host_user);
        copy(view_.codec_warning, sizeof view_.codec_warning, e.codec_warning);
        view_.block_system = e.block_system;
        view_.stream_seat = e.stream_seat;
        view_.move_first = e.move_first;
        view_.took_host = e.took_host;
        pthread_mutex_unlock(&lock_);
        break;
    case proto::GuestEvent::rumble_light:
        if (e.has_rumble)
            sinks_.rumble(e.rumble_lr[0], e.rumble_lr[1]);
        if (e.has_light)
            sinks_.light(e.light_rgb[0], e.light_rgb[1], e.light_rgb[2]);
        break;
    case proto::GuestEvent::bye:
        log("guest: bye %s", e.why);
        set_phase(GuestView::ended, e.why);
        break;
    case proto::GuestEvent::pong:
    {
        const std::uint64_t now = net::now_us();
        const std::uint64_t sent = static_cast<std::uint64_t>(e.pong_t * 1000.0);
        route_answer(static_cast<std::uint32_t>(static_cast<std::uint64_t>(e.pong_t)));
        if (sent && now >= sent)
        {
            const std::uint64_t rtt = now - sent;
            pthread_mutex_lock(&lock_);
            view_.rtt_ms = static_cast<int>(rtt / 1000);
            pthread_mutex_unlock(&lock_);
            if (e.has_engine_us)
            {
                const std::int64_t off = static_cast<std::int64_t>(e.engine_us) -
                                         static_cast<std::int64_t>(sent + rtt / 2);
                // keep a steady estimate: take a new one only when it is close or the first
                if (!have_offset_ || rtt < 20000)
                    offset_us_ = off, have_offset_ = true;
            }
        }
        break;
    }
    case proto::GuestEvent::none:
        break;
    }
}

void Guest::handle_binary(const std::uint8_t *d, std::size_t n) noexcept
{
    route_heard();
    if (n < 1 || !joined_.load())
        return;
    switch (d[0])
    {
    case 0:
        sinks_.video_header(d + 1, n - 1);
        break;
    case 1:
        sinks_.video(d + 1, n - 1, 0);
        break;
    case 7:
        if (n > 9)
        {
            std::uint64_t t = 0;
            for (int k = 0; k < 8; ++k)
                t = t << 8 | d[1 + k];
            sinks_.video(d + 9, n - 9, t);
            route_picture(t);
            if (have_offset_)
            {
                const std::int64_t delay =
                    static_cast<std::int64_t>(net::now_us()) + offset_us_ - static_cast<std::int64_t>(t);
                pthread_mutex_lock(&lock_);
                view_.delivery_ms = static_cast<int>(delay / 1000);
                pthread_mutex_unlock(&lock_);
            }
        }
        break;
    case 3:
        sinks_.audio_header(d + 1, n - 1);
        break;
    case 4:
        if (n > 3)
        {
            sinks_.audio(static_cast<std::uint16_t>(d[1] << 8 | d[2]), d + 3, n - 3);
            pthread_mutex_lock(&lock_);
            ++view_.sounds;
            pthread_mutex_unlock(&lock_);
        }
        return;
    default:
        return;
    }
    if (d[0] == 1 || d[0] == 7)
    {
        pthread_mutex_lock(&lock_);
        ++view_.pictures;
        pthread_mutex_unlock(&lock_);
    }
}
} // namespace play
