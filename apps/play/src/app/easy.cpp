// PS5Upload Play - easy join on the home screen (docs/EASY_JOIN.md): games on this network with
// one press, "Play again" with a remembered host, and the waiting room's QR code for the phone.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "app.hpp"

#include "../../third_party/qrcodegen/qrcodegen.h"
#include "../ui/i18n.hpp"
#include "log.hpp"
#include "platform.hpp"
#include "route_pick.hpp"

#include <cstdio>
#include <cstring>
#include <fcntl.h>
#include <unistd.h>

namespace play
{
namespace
{
using ui::Canvas;
using ui::Font;
using ui::S;
using ui::tr;
namespace col = ui::color;

constexpr std::uint64_t kInviteKeepUs = 5ull * 60 * 1000000; // a pushed invite waits this long

bool same_target(const char *a, std::uint16_t ap, const char *b, std::uint16_t bp) noexcept
{
    return ap == bp && !std::strcmp(a, b);
}
} // namespace

void App::easy_init() noexcept
{
    char path[160], text[6144]; // friends.txt: 8 hosts, each with its last internet invite
    std::snprintf(path, sizeof path, "%s/friends.txt", platform::data_dir());
    const long n = platform::read_file(path, text, sizeof text - 1);
    if (n > 0)
        friends_.parse(text, static_cast<std::size_t>(n));
    // A hub of one's own for tests (`hub.txt`: one line, ws:// or wss://), else the public one.
    char hub[128] = {};
    std::snprintf(path, sizeof path, "%s/hub.txt", platform::data_dir());
    const long hl = platform::read_file(path, hub, sizeof hub - 1);
    if (hl > 0)
    {
        hub[hl] = 0;
        for (long i = 0; i < hl; ++i)
            if (hub[i] == '\n' || hub[i] == '\r')
                hub[i] = 0;
    }
    char secret[160];
    std::snprintf(secret, sizeof secret, "%s/room.txt", platform::data_dir());
    const Friend *known[Friends::kMax];
    log("easy: %d remembered hosts; the waiting room on %s", friends_.recent(known, Friends::kMax),
        hub[0] ? hub : "the public hub");
    room_.init(secret, hub[0] ? hub : nullptr);
    log("easy: waiting room %s", room_.room()[0] ? "ready to open" : "not available");
}

void App::save_friends() noexcept
{
    char path[160], text[6144];
    std::snprintf(path, sizeof path, "%s/friends.txt", platform::data_dir());
    const std::size_t n = friends_.write(text, sizeof text);
    if (!platform::write_file(path, text, n))
        log("friends: could not save %s", path);
}

JoinExtras App::default_extras() const noexcept
{
    JoinExtras x;
    x.remember = true;
    // The waiting room, so the host's PS5Upload can send an invite here later.
    std::snprintf(x.inbox_room, sizeof x.inbox_room, "%s", room_.room());
    std::snprintf(x.inbox_hub, sizeof x.inbox_hub, "%s", room_.hub_host());
    return x;
}

bool App::own_ps5upload(char *addr, std::size_t cap, std::uint16_t *port) noexcept
{
    pthread_mutex_lock(&disc_lock_);
    const bool ok = fake_.on ? own_engine(fake_.answers, fake_.found, addr, cap, port)
                             : own_engine(found_, n_found_, addr, cap, port);
    pthread_mutex_unlock(&disc_lock_);
    return ok;
}

void App::start_join_how(const char *code, const JoinExtras &x, const route::Candidate *handed,
                         int n_handed) noexcept
{
    std::snprintf(code_, sizeof code_, "%s", code);
    // Only through this home's PS5Upload: start()'s address is that PS5Upload, not the host's
    // computer, so it is not remembered as one, and discovery has nothing to add.
    const bool bridge_only = n_handed > 0 && handed[0].way == route::Way::bridge;
    if (!bridge_only)
        remember_current();
    guest_->leave();
    guest_->join();
    guest_->set_extras(x);
    last_extras_ = x;
    friend_saved_ = link_saved_ = false;
    if (handed != last_ways_)
    {
        n_last_ways_ = n_handed < route::Selector::kMax ? n_handed : route::Selector::kMax;
        for (int i = 0; i < n_last_ways_; ++i)
            last_ways_[i] = handed[i];
    }
    constexpr int kMax = route::Selector::kMax;
    route::Candidate ways[kMax];
    int n_ways = 0;
    // A handed join's own address first (it keeps its socket's path).
    for (int i = 0; i < n_handed && n_ways < kMax; ++i)
        if (handed[i].way != route::Way::bridge)
            ways[n_ways++] = handed[i];
    if (!bridge_only)
    {
        // Every address discovery found for this engine is a way to it (docs/ROUTES.md): the best
        // is checked at connect, the rest kept as backups. The picked address keeps its path.
        route::Candidate found[kMax];
        pthread_mutex_lock(&disc_lock_);
        const int nf = route_candidates(found_, n_found_, engine_name_, "", handed ? nullptr : addr_, port_, found, kMax);
        pthread_mutex_unlock(&disc_lock_);
        for (int i = 0; i < nf && n_ways < kMax; ++i)
        {
            bool dup = false;
            for (int k = 0; k < n_ways; ++k)
                dup = dup || (ways[k].port == found[i].port && !std::strcmp(ways[k].addr, found[i].addr));
            if (dup)
                continue;
            if (!handed && same_target(found[i].addr, found[i].port, addr_, port_))
                std::snprintf(found[i].path, sizeof found[i].path, "%s", x.path);
            ways[n_ways++] = found[i];
        }
    }
    // The bridge last: used only when nothing direct answers (docs/ROUTES.md §6).
    bool bridge = false;
    for (int i = 0; i < n_handed; ++i)
        if (handed[i].way == route::Way::bridge)
        {
            if (n_ways == kMax)
                --n_ways; // the last discovered way makes room
            ways[n_ways++] = handed[i];
            bridge = true;
        }
    guest_->set_routes(ways, n_ways);
    guest_->start(addr_, port_, code_, platform::user_name(), version_);
    last_phase_ = -1;
    if (screen_ != Screen::joining)
        go(Screen::joining);
    log("app: joining %s:%u (%s; %d way(s)%s)", addr_, static_cast<unsigned>(port_),
        code_[0] ? "code" : x.invite[0] ? "invite" : x.again[0] ? "again" : "nearby", n_ways,
        bridge_only ? ", only through this home's PS5Upload" : bridge ? ", this home's PS5Upload as the last" : "");
}

void App::join_invite(const invite::Invite &inv) noexcept
{
    const char *who = inv.name[0] ? inv.name : tr(S::host_friend);
    // An internet invite (room and certificate) can go through this home's PS5Upload (the bridge,
    // docs/ROUTES.md §6) when nothing direct answers.
    char own[64] = {};
    std::uint16_t own_port = 0;
    const bool can_bridge = inv.room[0] && own_ps5upload(own, sizeof own, &own_port);
    if (!inv.n_routes && !can_bridge)
    {
        // Nothing direct answers, and no PS5Upload here to play it through: say what it needs
        // (the PS5Upload screen to look at), never fail quietly.
        std::snprintf(note_, sizeof note_, tr(inv.room[0] ? S::inv_bridge_need : S::inv_unreachable), who);
        note_until_ = net::now_us() + (inv.room[0] ? 15000000 : 8000000);
        platform::blip(platform::Blip::bad);
        log("app: an invite this PS5 can't play: %s", inv.room[0] ? "internet only, and no PS5Upload on this network"
                                                                    : "no way to the host");
        return;
    }
    route::Candidate ways[route::Selector::kMax];
    const int n = invite_ways(inv, can_bridge ? own : nullptr, own_port, ways, route::Selector::kMax);
    target_ = JoinTarget{};
    JoinExtras x = default_extras();
    std::snprintf(x.invite, sizeof x.invite, "%s", inv.key);
    if (inv.n_routes)
    {
        const invite::Route &r = inv.routes[0];
        std::snprintf(addr_, sizeof addr_, "%s", r.dial_addr[0] ? r.dial_addr : r.addr);
        port_ = r.dial_port ? r.dial_port : r.port;
        std::snprintf(target_.addr, sizeof target_.addr, "%s", r.addr);
        target_.port = r.port;
        target_.tailnet = r.tailnet;
        std::snprintf(target_.path, sizeof target_.path, "%s", r.path);
        std::snprintf(x.path, sizeof x.path, "%s", r.path);
    }
    else
    {
        // Over the internet only: start()'s address is this home's PS5Upload. The host as the link
        // names it, so a remembered host keeps this invite for Play again.
        std::snprintf(addr_, sizeof addr_, "%s", own);
        port_ = own_port;
        std::snprintf(target_.addr, sizeof target_.addr, "%s", inv.home_addr);
        target_.port = inv.home_port;
    }
    std::snprintf(target_.room, sizeof target_.room, "%s", inv.room);
    std::snprintf(target_.fp, sizeof target_.fp, "%s", inv.fp);
    std::snprintf(target_.key, sizeof target_.key, "%s", inv.room[0] ? inv.key : "");
    std::snprintf(engine_name_, sizeof engine_name_, "%s", inv.name);
    std::snprintf(target_.name, sizeof target_.name, "%s", inv.name);
    depth_ = 0;
    screen_ = Screen::home;
    for_host_ = false;
    start_join_how("", x, ways, n);
}

void App::join_again(const Friend &f) noexcept
{
    // The host's last internet invite, through this home's PS5Upload, when the host's own address
    // doesn't answer (docs/ROUTES.md §6).
    char own[64] = {};
    std::uint16_t own_port = 0;
    const bool can_bridge = f.room[0] && f.key[0] && own_ps5upload(own, sizeof own, &own_port);
    bool direct = true;
    std::snprintf(addr_, sizeof addr_, "%s", f.addr);
    port_ = f.port;
    if (f.tailnet)
    {
        std::uint16_t local = 0;
        char err[32] = {};
        if (helper::forward(f.addr, f.port, &local, err, sizeof err))
        {
            std::snprintf(addr_, sizeof addr_, "%s", "127.0.0.1");
            port_ = local;
        }
        else if (can_bridge)
        {
            direct = false;
            log("app: Tailscale to %s: %s; through this home's PS5Upload", f.name[0] ? f.name : f.addr, err);
        }
        else
        {
            std::snprintf(note_, sizeof note_, tr(S::pick_tailnet_fail), f.name[0] ? f.name : f.addr, err);
            note_until_ = net::now_us() + 6000000;
            platform::blip(platform::Blip::bad);
            return;
        }
    }
    route::Candidate ways[route::Selector::kMax];
    const int n = again_ways(f, direct, can_bridge ? own : nullptr, own_port, ways, route::Selector::kMax);
    if (!direct)
    {
        std::snprintf(addr_, sizeof addr_, "%s", own);
        port_ = own_port;
    }
    std::snprintf(engine_name_, sizeof engine_name_, "%s", f.name);
    target_ = JoinTarget{};
    std::snprintf(target_.addr, sizeof target_.addr, "%s", f.addr);
    target_.port = f.port;
    target_.tailnet = f.tailnet;
    std::snprintf(target_.path, sizeof target_.path, "%s", f.path);
    std::snprintf(target_.name, sizeof target_.name, "%s", f.name);
    JoinExtras x = default_extras();
    x.remember = false; // it has its pass
    std::snprintf(x.again, sizeof x.again, "%s", f.pass);
    std::snprintf(x.path, sizeof x.path, "%s", f.path);
    // On the bridge the engine takes no pass: the last internet invite's key asks there.
    if (can_bridge)
        std::snprintf(x.bridge_invite, sizeof x.bridge_invite, "%s", f.key);
    for_host_ = false;
    start_join_how("", x, ways, n);
}

int App::easy_rows(EasyRow *out, int cap) noexcept
{
    int n = 0;
    const std::uint64_t now = net::now_us();
    if (has_invite_ && now < invite_until_ && n < cap)
    {
        EasyRow &r = out[n++];
        r = EasyRow{};
        r.kind = EasyRow::invite;
        std::snprintf(r.name, sizeof r.name, "%s", invite_.name[0] ? invite_.name : tr(S::host_friend));
    }
    net::Answer found[8];
    int nf;
    pthread_mutex_lock(&disc_lock_);
    nf = fake_.on ? fake_.found : n_found_;
    std::memcpy(found, fake_.on ? fake_.answers : found_, sizeof(net::Answer) * static_cast<std::size_t>(nf > 8 ? 8 : nf));
    pthread_mutex_unlock(&disc_lock_);
    for (int i = 0; i < nf && n < cap; ++i)
    {
        if (found[i].tailnet || found[i].info.together <= 0)
            continue;
        EasyRow &r = out[n++];
        r = EasyRow{};
        r.kind = EasyRow::nearby;
        net::ip_text(found[i].ip_be, r.addr);
        r.port = found[i].info.port;
        std::snprintf(r.name, sizeof r.name, "%s", found[i].info.name[0] ? found[i].info.name : r.addr);
    }
    const Friend *fr[Friends::kMax];
    const int nfr = friends_.recent(fr, Friends::kMax);
    for (int i = 0; i < nfr && n < cap; ++i)
    {
        bool dup = false;
        for (int k = 0; k < n; ++k)
            dup = dup || (out[k].kind == EasyRow::nearby && same_target(out[k].addr, out[k].port, fr[i]->addr, fr[i]->port));
        if (dup)
            continue;
        EasyRow &r = out[n++];
        r = EasyRow{};
        r.kind = EasyRow::again;
        r.friend_ = fr[i];
        std::snprintf(r.addr, sizeof r.addr, "%s", fr[i]->addr);
        r.port = fr[i]->port;
        std::snprintf(r.name, sizeof r.name, "%s", fr[i]->name[0] ? fr[i]->name : fr[i]->addr);
    }
    return n;
}

bool App::easy_choose(const EasyRow &r) noexcept
{
    platform::blip(platform::Blip::choose);
    switch (r.kind)
    {
    case EasyRow::invite:
        has_invite_ = false;
        join_invite(invite_);
        return true;
    case EasyRow::nearby:
    {
        std::snprintf(addr_, sizeof addr_, "%s", r.addr);
        port_ = r.port;
        std::snprintf(engine_name_, sizeof engine_name_, "%s", r.name);
        target_ = JoinTarget{};
        std::snprintf(target_.addr, sizeof target_.addr, "%s", r.addr);
        target_.port = r.port;
        std::snprintf(target_.name, sizeof target_.name, "%s", r.name);
        for_host_ = false;
        start_join_how("", default_extras());
        return true;
    }
    case EasyRow::again:
        if (r.friend_)
            join_again(*r.friend_);
        return true;
    }
    return false;
}

void App::easy_frame(std::uint64_t now) noexcept
{
    if (!fake_.on)
    {
        // Discovery while the home screen (or the pick screen) is up.
        const bool want_disc = screen_ == Screen::home || screen_ == Screen::pick;
        if (want_disc && !disc_run_.load())
            start_discovery();
        else if (!want_disc && disc_run_.load())
            disc_run_ = false;
        // The waiting room while the home screen is up (and nothing is being played).
        const bool busy = streaming_ || guest_->running();
        room_.set_busy(busy);
        // An internet invite nothing direct reaches is still taken when this home has a
        // PS5Upload to play it through (docs/ROUTES.md §6).
        char own[64];
        std::uint16_t own_port = 0;
        room_.set_bridge(own_ps5upload(own, sizeof own, &own_port));
        if (screen_ == Screen::home && !busy)
        {
            if (!room_paused_)
                room_.arm();
        }
        else
        {
            room_.disarm();
            room_paused_ = false;
        }
        const auto rv = room_.view();
        if (rv.state != last_room_state_ || rv.expired != last_room_expired_)
        {
            last_room_state_ = rv.state;
            last_room_expired_ = rv.expired;
            if (rv.expired)
                room_paused_ = true; // shown again with △
            dirty_ = true;
        }
    }
    // An invite handed over.
    invite::Invite inv;
    while (room_.take(inv))
    {
        dirty_ = true;
        const bool calm = screen_ == Screen::home || screen_ == Screen::pick || screen_ == Screen::way ||
                          screen_ == Screen::code || screen_ == Screen::address ||
                          (screen_ == Screen::joining && !guest_->running());
        if (inv.nonce[0] && calm)
        {
            // The phone in front of this TV: go.
            log("app: an invite from the phone");
            join_invite(inv);
            continue;
        }
        // A host's push: the person decides (a row on the home screen, for a while).
        invite_ = inv;
        has_invite_ = true;
        invite_until_ = now + kInviteKeepUs;
        sel_ = 0;
        std::snprintf(note_, sizeof note_, tr(S::inv_got), inv.name[0] ? inv.name : tr(S::host_friend));
        note_until_ = now + 6000000;
        platform::blip(platform::Blip::choose);
    }
    // The host's engine gave a pass ("joined"): keep it for Play again. A pass that stopped
    // working is forgotten.
    const GuestView gv = guest_view();
    if (gv.phase == GuestView::joined && gv.friend_pass[0] && !friend_saved_ && target_.addr[0])
    {
        friend_saved_ = true;
        if (friends_.remember(target_.addr, target_.port, target_.tailnet, target_.path,
                              target_.name[0] ? target_.name : gv.host, gv.friend_pass))
        {
            save_friends();
            log("app: remembered this host for Play again");
        }
    }
    // Joined with an internet invite: a remembered host keeps it, so Play again can go through
    // this home's PS5Upload while it is open (docs/ROUTES.md §6).
    if (gv.phase == GuestView::joined && target_.room[0] && target_.key[0] && !link_saved_ && target_.addr[0] &&
        (!gv.friend_pass[0] || friend_saved_))
    {
        link_saved_ = true;
        Friend *f = friends_.find(target_.addr, target_.port);
        if (f && Friends::set_link(*f, target_.room, target_.fp, target_.key))
        {
            save_friends();
            log("app: kept this internet invite for Play again");
        }
    }
    // A kept invite that no longer works through the bridge is let go (the pass stays).
    if (gv.phase == GuestView::refused && last_extras_.bridge_invite[0] &&
        (!std::strcmp(gv.why, "key") || !std::strcmp(gv.why, "expired")))
    {
        if (Friend *f = friends_.find(target_.addr, target_.port))
            if (!std::strcmp(f->key, last_extras_.bridge_invite))
            {
                f->room[0] = f->fp[0] = f->key[0] = 0;
                save_friends();
                log("app: the host's internet invite has ended: let it go");
            }
        last_extras_.bridge_invite[0] = 0;
    }
    if (gv.phase == GuestView::refused && !std::strcmp(gv.why, "friend") && last_extras_.again[0])
    {
        for (const auto &f : friends_.list)
            if (f.addr[0] && !std::strcmp(f.pass, last_extras_.again))
            {
                friends_.forget(&f);
                save_friends();
                log("app: the host no longer knows this PS5: forgot its pass");
                break;
            }
        last_extras_.again[0] = 0;
    }
}

void App::draw_easy_qr(Canvas &c) noexcept
{
    const WaitingRoom::View v = fake_.on ? fake_.room : room_.view();
    const int x = 1330, w = 470;
    if (v.state == WaitingRoom::View::ready && v.qr[0])
    {
        static std::uint8_t qr[qrcodegen_BUFFER_LEN_MAX], tmp[qrcodegen_BUFFER_LEN_MAX];
        if (qrcodegen_encodeText(v.qr, tmp, qr, qrcodegen_Ecc_MEDIUM, 1, 20, qrcodegen_Mask_AUTO, true))
        {
            const int size = qrcodegen_getSize(qr);
            const int module = 360 / (size + 4) > 2 ? 360 / (size + 4) : 2;
            c.text_wrap(Font::body, x, 230, w, tr(S::phone_title), col::text);
            c.qr(qr, x, 300, module);
            c.text_wrap(Font::small, x, 300 + (size + 4) * module + 16, w, tr(S::phone_text), col::muted);
            return;
        }
    }
    c.image(logo_, 1480, 330);
    const char *line = v.expired || room_paused_ ? tr(S::phone_again)
                       : v.state == WaitingRoom::View::offline ? tr(S::phone_offline)
                       : v.state == WaitingRoom::View::connecting ? tr(S::phone_wait)
                                                                 : nullptr;
    if (line)
        c.text_wrap(Font::small, x, 860, w, line, col::muted);
}

void App::draw_easy_row(Canvas &c, int x, int y, int w, int h, bool sel, const EasyRow &r) noexcept
{
    char title[160];
    const S t = r.kind == EasyRow::invite ? S::home_invite : r.kind == EasyRow::nearby ? S::home_nearby : S::home_again;
    const S s = r.kind == EasyRow::invite   ? S::home_invite_sub
                : r.kind == EasyRow::nearby ? S::home_nearby_sub
                                            : S::home_again_sub;
    std::snprintf(title, sizeof title, tr(t), r.name);
    draw_row(c, x, y, w, h, sel, title, tr(s));
    c.circle(x + 14, y + h / 2, 7, r.kind == EasyRow::again ? col::accent : col::good);
}
} // namespace play
