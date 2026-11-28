// PS5Upload Play - Play Rooms on the TV (docs/ROOMS.md): the Join list (Your friends' rooms,
// Shared with you, Nearby, Public rooms), Details, the filter sheet, the on-screen keyboard, the
// responsibility notice, Thanks & Support, the first-run guide and the controls. Controller-first:
// ✕ choose, ○ back, △ the screen's second sheet, □ the secondary action, L1/R1 sorts and pages.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "app.hpp"

#include "../../third_party/qrcodegen/qrcodegen.h"
#include "../core/json.hpp"
#include "../core/notice.hpp"
#include "../ui/i18n.hpp"
#include "log.hpp"
#include "platform.hpp"
#include "route_pick.hpp"
#include "route_ui.hpp"

#include <cstdio>
#include <cstring>

namespace play
{
namespace
{
using ui::Canvas;
using ui::Font;
using ui::S;
using ui::tr;
namespace col = ui::color;
namespace pad = proto::pad;

constexpr int kLeft = 140, kTop = 96;
constexpr std::uint32_t kUp = pad::kUp, kDown = pad::kDown, kLeftB = pad::kLeft, kRight = pad::kRight;
constexpr std::uint64_t kFocusUs = 1000000; // a card focused this long fetches its Details
constexpr const char *kKofi = "https://ko-fi.com/B0B81S0WUA";
constexpr const char *kThanksPage = "https://ps5upload.phantomptr.com/#thanks";

enum : int
{
    G_CROSS = static_cast<int>(Canvas::Glyph::cross),
    G_CIRCLE = static_cast<int>(Canvas::Glyph::circle),
    G_SQUARE = static_cast<int>(Canvas::Glyph::square),
    G_TRIANGLE = static_cast<int>(Canvas::Glyph::triangle),
    G_OPTIONS = static_cast<int>(Canvas::Glyph::options),
    G_L1 = static_cast<int>(Canvas::Glyph::l1),
    G_R1 = static_cast<int>(Canvas::Glyph::r1),
};

int clampi(int v, int lo, int hi) noexcept
{
    return v < lo ? lo : v > hi ? hi : v;
}

S sort_label(rooms::Sort s) noexcept
{
    switch (s)
    {
    case rooms::Sort::latency: return S::sort_latency;
    case rooms::Sort::game: return S::sort_game;
    case rooms::Sort::players: return S::sort_players;
    case rooms::Sort::space: return S::sort_space;
    case rooms::Sort::newest: return S::sort_newest;
    default: return S::sort_best;
    }
}

S group_label(rooms::Group g) noexcept
{
    switch (g)
    {
    case rooms::Group::friends: return S::grp_friends;
    case rooms::Group::shared: return S::grp_shared;
    case rooms::Group::nearby: return S::grp_nearby;
    default: return S::grp_public;
    }
}

void card_key(const rooms::Card &k, char *out, std::size_t cap) noexcept
{
    if (k.room[0])
        std::snprintf(out, cap, "%s", k.room);
    else
        std::snprintf(out, cap, "%d:%s:%u", static_cast<int>(k.group), k.addr, static_cast<unsigned>(k.port));
}

std::uint32_t bar_colour(int bars) noexcept
{
    return bars >= 3 ? col::good : bars == 2 ? col::warn : bars == 1 ? col::bad : col::muted;
}
} // namespace

// ---- small pieces -----------------------------------------------------------------------------
int App::draw_bars(Canvas &c, int x, int y, int bars, bool hollow) noexcept
{
    // four bars of growing height, bottom-aligned on y + 30; filled up to `bars`
    const std::uint32_t on = bar_colour(bars);
    for (int i = 0; i < 4; ++i)
    {
        const int h = 9 + i * 7, bx = x + i * 13, by = y + 30 - h;
        if (i < bars && !hollow)
            c.fill(bx, by, 9, h, on);
        else if (i < bars)
        {
            c.fill(bx, by, 9, 2, on);
            c.fill(bx, by + h - 2, 9, 2, on);
            c.fill(bx, by, 2, h, on);
            c.fill(bx + 7, by, 2, h, on);
        }
        else
            c.fill(bx, by, 9, h, col::panel_hi);
    }
    return 4 * 13;
}

const char *App::quick_text(int id) const noexcept
{
    if (id < 1 || id > chat::kQuick)
        return "";
    return tr(static_cast<S>(static_cast<int>(S::quick_1) + id - 1));
}

void App::load_device() noexcept
{
    // This PS5's device id for rooms (16 random bytes, kept): the host shows its 4-hex tag.
    char path[160], hex[64] = {};
    std::snprintf(path, sizeof path, "%s/app_id", platform::data_dir());
    std::uint8_t id[16] = {};
    const long n = platform::read_file(path, hex, sizeof hex - 1);
    bool ok = n >= 32;
    for (int i = 0; ok && i < 16; ++i)
    {
        unsigned v = 0;
        ok = std::sscanf(hex + i * 2, "%2x", &v) == 1;
        id[i] = static_cast<std::uint8_t>(v);
    }
    if (!ok)
    {
        net::random_bytes(id, sizeof id);
        for (int i = 0; i < 16; ++i)
            std::snprintf(hex + i * 2, 3, "%02x", id[i]);
        platform::write_file(path, hex, 32);
    }
    invite::b64url(id, sizeof id, device_, sizeof device_);
    std::snprintf(app_id_, sizeof app_id_, "%.32s", hex);
}

bool App::host_auto() noexcept
{
    // 1. A grant the helper holds for an engine that already trusts this console (no code).
    helper::Grant g[4];
    const int ng = helper::api_version() >= 2 ? helper::grants(g, 4) : 0;
    if (ng > 0)
    {
        std::snprintf(addr_, sizeof addr_, "%s", g[0].addr);
        port_ = g[0].port;
        std::snprintf(engine_name_, sizeof engine_name_, "%s", g[0].name);
        std::snprintf(host_id_, sizeof host_id_, "%s", g[0].engine_id);
        std::snprintf(host_addr_, sizeof host_addr_, "%s", g[0].addr);
        host_port_ = g[0].port;
        std::snprintf(host_grant_, sizeof host_grant_, "%s", g[0].grant);
        log("app: host through %s with the helper's grant", g[0].name);
        start_host();
        go(Screen::host);
        return true;
    }
    // 2. Discovery: the engine streaming this PS5, else a paired one, else the only one that
    //    knows this PS5 (integration design F2). Nothing else is picked for the person.
    net::Answer found[8];
    int nf;
    pthread_mutex_lock(&disc_lock_);
    nf = n_found_;
    std::memcpy(found, found_, sizeof(net::Answer) * static_cast<std::size_t>(nf > 8 ? 8 : nf));
    pthread_mutex_unlock(&disc_lock_);
    int pick = -1, known = 0, known_at = -1;
    for (int i = 0; i < nf && pick < 0; ++i)
        if (found[i].info.you_streaming)
            pick = i;
    for (int i = 0; i < nf && pick < 0; ++i)
        if (found[i].info.you_paired)
            pick = i;
    for (int i = 0; i < nf; ++i)
        if (found[i].info.you_known)
            ++known, known_at = i;
    if (pick < 0 && known == 1)
        pick = known_at;
    // a remembered, paired computer that answered (by its id)
    for (int i = 0; i < nf && pick < 0; ++i)
        if (const Host *h = settings_.find_id(found[i].info.id))
            if (h->token[0])
                pick = i;
    if (pick < 0 || found[pick].tailnet)
        return false;
    net::ip_text(found[pick].ip_be, addr_);
    port_ = found[pick].info.port;
    std::snprintf(engine_name_, sizeof engine_name_, "%s", found[pick].info.name);
    std::snprintf(host_id_, sizeof host_id_, "%s", found[pick].info.id);
    std::snprintf(host_addr_, sizeof host_addr_, "%s", addr_);
    host_port_ = port_;
    log("app: host with %s (%s)", engine_name_,
        found[pick].info.you_streaming ? "streaming this PS5" : found[pick].info.you_paired ? "paired" : "knows this PS5");
    start_host();
    go(Screen::host);
    return true;
}

void App::take_intent() noexcept
{
    char what[16];
    if (fake_.on || !helper::intent(what, sizeof what))
        return;
    log("app: opened from Minion for %s", what);
    if (!std::strcmp(what, "join") || !std::strcmp(what, "rooms"))
        open_rooms();
    else if (!std::strcmp(what, "host"))
    {
        for_host_ = true;
        if (!host_auto())
            go(Screen::pick);
    }
}

void App::load_supporters() noexcept
{
    if (n_names_ || n_anonymous_)
        return;
    std::snprintf(kofi_, sizeof kofi_, "%s", kKofi);
    std::snprintf(thanks_url_, sizeof thanks_url_, "%s", kThanksPage);
    // The engine's newer list (kept when connected), else the one in the app.
    char path[160];
    std::snprintf(path, sizeof path, "%s/supporters.json", platform::data_dir());
    long n = platform::read_file(path, supporters_, sizeof supporters_ - 1);
    std::size_t len = n > 0 ? static_cast<std::size_t>(n) : 0;
    if (!len)
    {
        std::size_t al = 0;
        const std::uint8_t *d = platform::asset("supporters.json", &al);
        if (d && al < sizeof supporters_)
        {
            std::memcpy(supporters_, d, al);
            len = al;
        }
    }
    supporters_[len] = 0;
    static json::Token toks[400];
    json::Doc d(toks, 400);
    if (!len || !d.parse(supporters_, len))
        return;
    const int names = d.get(0, "names");
    n_anonymous_ = static_cast<int>(d.integer(d.get(0, "anonymous")));
    // The names in place: each string's bytes are rewritten as a terminated copy in the buffer
    // (JSON escapes only shorten text, so it fits where it was).
    for (std::uint32_t i = 0; i < d.count(names) && n_names_ < 160; ++i)
    {
        const int t = d.item(names, i);
        if (d.type(t) != json::Type::string)
            continue;
        char *at = supporters_ + d.at(static_cast<std::uint32_t>(t)).start;
        char tmp[96];
        if (!d.str(t, tmp, sizeof tmp))
            continue;
        std::memcpy(at, tmp, std::strlen(tmp) + 1);
        names_[n_names_++] = at;
    }
    const int links = d.get(0, "links");
    if (d.type(links) == json::Type::object)
    {
        char u[96];
        if (d.str(d.get(links, "kofi"), u, sizeof u) && !std::strncmp(u, "https://", 8))
            std::snprintf(kofi_, sizeof kofi_, "%s", u);
        if (d.str(d.get(links, "thanks_page"), u, sizeof u) && !std::strncmp(u, "https://", 8))
            std::snprintf(thanks_url_, sizeof thanks_url_, "%s", u);
    }
}

void App::save_resume(const GuestView &v) noexcept
{
    // Back to the game after the host restarts (or this app was closed): kept 5 minutes.
    char path[160], text[400];
    std::snprintf(path, sizeof path, "%s/resume.txt", platform::data_dir());
    const int n = std::snprintf(text, sizeof text, "%s|%u|%s|%s|%s|%llu\n", addr_, static_cast<unsigned>(port_), v.token,
                                last_extras_.path, engine_name_[0] ? engine_name_ : v.host,
                                static_cast<unsigned long long>(net::wall_seconds()));
    if (n > 0 && std::strchr(v.token, '|') == nullptr)
        platform::write_file(path, text, static_cast<std::size_t>(n));
}

bool App::load_resume() noexcept
{
    char path[160], text[400] = {};
    std::snprintf(path, sizeof path, "%s/resume.txt", platform::data_dir());
    const long n = platform::read_file(path, text, sizeof text - 1);
    resume_ = Resume{};
    if (n <= 0)
        return false;
    text[n] = 0;
    // addr|port|token|path|name|unix seconds (fields may be empty: split by hand)
    char *f[6] = {};
    char *p = text;
    for (int k = 0; k < 6 && p; ++k)
    {
        f[k] = p;
        char *bar = k < 5 ? std::strchr(p, '|') : nullptr;
        if (bar)
            *bar = 0;
        p = bar ? bar + 1 : nullptr;
    }
    if (!f[5] || !f[0][0] || !f[2][0])
        return false;
    std::snprintf(resume_.addr, sizeof resume_.addr, "%s", f[0]);
    resume_.port = static_cast<std::uint16_t>(std::strtoul(f[1], nullptr, 10));
    std::snprintf(resume_.token, sizeof resume_.token, "%s", f[2]);
    std::snprintf(resume_.path, sizeof resume_.path, "%s", f[3]);
    std::snprintf(resume_.name, sizeof resume_.name, "%s", f[4]);
    resume_.at = std::strtoull(f[5], nullptr, 10);
    if (net::wall_seconds() - resume_.at > 300)
    {
        resume_ = Resume{};
        return false;
    }
    log("app: a game to go back to on %s (%llu s ago)", resume_.addr,
        static_cast<unsigned long long>(net::wall_seconds() - resume_.at));
    return true;
}

void App::clear_resume() noexcept
{
    char path[160];
    std::snprintf(path, sizeof path, "%s/resume.txt", platform::data_dir());
    platform::write_file(path, "", 0);
}

// ---- the notice -------------------------------------------------------------------------------
bool App::need_notice(int need, Pending then) noexcept
{
    if (!notice::needed(static_cast<notice::Need>(need), settings_.notice))
        return true;
    pending_ = then;
    go(Screen::notice);
    return false;
}

void App::run_pending() noexcept
{
    const Pending p = pending_;
    pending_ = Pending::none;
    switch (p)
    {
    case Pending::join_card: choose_card(pending_card_); break;
    case Pending::details:
        rooms_net_->details(pending_card_);
        go(Screen::details);
        break;
    case Pending::mic:
        mic_on_ = true;
        guest_->send_mic("on");
        break;
    case Pending::chat:
        open_keyboard(Kb::chat, tr(S::kb_title_chat), "", chat::kTextMax, false);
        break;
    case Pending::make_public:
    case Pending::chat_on:
    case Pending::none: break;
    }
}

void App::input_notice(std::uint32_t p) noexcept
{
    if (p & pad::kCross)
    {
        platform::blip(platform::Blip::choose);
        settings_.notice = notice::kVersion;
        save();
        log("app: the rooms notice was accepted (version %d)", notice::kVersion);
        if (depth_ > 0)
            screen_ = stack_[--depth_];
        else
            screen_ = Screen::home;
        sel_ = 0;
        run_pending();
        return;
    }
    if (p & pad::kCircle)
    {
        // Not now: those features stay off; the room still works.
        pending_ = Pending::none;
        back();
    }
}

void App::draw_notice(Canvas &c) noexcept
{
    draw_frame(c, nullptr);
    int y = 150;
    y += c.text_wrap(Font::title, kLeft, y, 1500, tr(S::notice_title), col::text) + 36;
    y += c.text_wrap(Font::body, kLeft, y, 1500, tr(S::notice_body), col::text) + 30;
    c.text_wrap(Font::body, kLeft, y, 1500, tr(S::notice_parents), col::warn);
    const int g[] = {G_CROSS, G_CIRCLE};
    const S l[] = {S::notice_accept, S::notice_later};
    draw_hints(c, g, l, 2);
}

// ---- the Join list ----------------------------------------------------------------------------
void App::open_rooms() noexcept
{
    go(Screen::rooms);
    sort_ = rooms::sort_from(settings_.sort);
    filter_.space = settings_.f_space;
    filter_.nopw = settings_.f_nopw;
    filter_.myver = settings_.f_myver;
    filter_.near = settings_.f_near;
    std::snprintf(filter_.lang, sizeof filter_.lang, "%s", settings_.f_lang);
    if (!fake_.on)
    {
        rooms_net_->start();
        rooms_net_->refresh(sort_, filter_);
    }
    focus_key_[0] = 0;
    if (!settings_.rooms_intro)
    {
        settings_.rooms_intro = true;
        save();
        std::snprintf(note_, sizeof note_, "%s", tr(S::rooms_intro));
        note_until_ = net::now_us() + 6000000;
    }
}

int App::join_items(JoinItem *out, int cap) noexcept
{
    int n = 0;
    auto add = [&](const rooms::Card &k) {
        if (n < cap - 4)
        {
            out[n].kind = JoinItem::card;
            out[n++].c = k;
        }
    };
    const RoomsNet::View &v = rv_;
    // 1. Your friends' rooms (never paged away)
    for (int i = 0; i < v.n_friends; ++i)
    {
        rooms::Card k = v.friends[i];
        k.group = rooms::Group::friends;
        if (!std::strcmp(v.detail_key, k.room) && v.measured_ms >= 0)
            k.measured_ms = v.measured_ms;
        for (const auto &f : friends_.list)
            if (f.addr[0] && !std::strcmp(f.lobby, k.room))
            {
                k.history_ms = f.last_rtt();
                if (!k.host[0])
                    std::snprintf(k.host, sizeof k.host, "%s", f.name);
            }
        add(k);
    }
    // 2. Shared with you (a pushed invite, for a while)
    if (has_invite_ && net::now_us() < invite_until_)
    {
        rooms::Card k;
        k.group = rooms::Group::shared;
        std::snprintf(k.from, sizeof k.from, "%s", invite_.name[0] ? invite_.name : tr(S::host_friend));
        std::snprintf(k.host, sizeof k.host, "%s", k.from);
        k.proto = rooms::kProto;
        add(k);
    }
    // 3. Nearby (discovery: a Play Together open on this network)
    net::Answer found[8];
    int nf;
    pthread_mutex_lock(&disc_lock_);
    nf = fake_.on ? fake_.found : n_found_;
    std::memcpy(found, fake_.on ? fake_.answers : found_, sizeof(net::Answer) * static_cast<std::size_t>(nf > 8 ? 8 : nf));
    pthread_mutex_unlock(&disc_lock_);
    for (int i = 0; i < nf; ++i)
    {
        if (found[i].tailnet || found[i].info.together <= 0)
            continue;
        rooms::Card k;
        k.group = rooms::Group::nearby;
        net::ip_text(found[i].ip_be, k.addr);
        k.port = found[i].info.port;
        std::snprintf(k.host, sizeof k.host, "%s", found[i].info.name[0] ? found[i].info.name : k.addr);
        k.proto = rooms::kProto;
        k.cap_preview = true;
        if (const Friend *f = friends_.find(k.addr, k.port))
            k.history_ms = f->last_rtt();
        add(k);
    }
    // 4. Public rooms: the hub's order, refined here by what this PS5 knows (the last games with a
    // host), filtered. A measured Details ping is shown but never moves the card under the cursor.
    static rooms::Card pub[rooms::List::kMax];
    int np = 0;
    for (int i = 0; i < v.list.n; ++i)
    {
        rooms::Card k = v.list.cards[i];
        k.group = rooms::Group::pub;
        for (const auto &f : friends_.list)
            if (f.addr[0] && f.lobby[0] && !std::strcmp(f.lobby, k.room))
                k.history_ms = f.last_rtt();
        if (rooms::passes(k, filter_))
            pub[np++] = k;
    }
    rooms::sort(pub, np, sort_);
    for (int i = 0; i < np; ++i)
    {
        if (!std::strcmp(v.detail_key, pub[i].room) && v.measured_ms >= 0)
            pub[i].measured_ms = v.measured_ms;
        add(pub[i]);
    }
    if (v.state == RoomsNet::State::unreachable)
        out[n++].kind = JoinItem::retry;
    else if (v.list.next > 0)
        out[n++].kind = JoinItem::more;
    out[n++].kind = JoinItem::code;
    out[n++].kind = JoinItem::address;
    return n;
}

void App::choose_card(const rooms::Card &k) noexcept
{
    if (rooms::full(k))
    {
        std::snprintf(note_, sizeof note_, "%s", tr(S::rooms_is_full));
        note_until_ = net::now_us() + 4000000;
        platform::blip(platform::Blip::bad);
        return;
    }
    if (!rooms::compatible(k))
    {
        if (k.proto > rooms::kProto)
            std::snprintf(note_, sizeof note_, tr(S::rooms_update), k.ver[0] ? k.ver : "13.0.0");
        else
            std::snprintf(note_, sizeof note_, "%s", tr(k.cap_ps5app ? S::why_old : S::rooms_no_ps5));
        note_until_ = net::now_us() + 6000000;
        platform::blip(platform::Blip::bad);
        return;
    }
    // Anything from Rooms needs the notice once (a public room, a friend's room).
    if (k.group == rooms::Group::pub || k.group == rooms::Group::friends)
    {
        pending_card_ = k;
        if (!need_notice(static_cast<int>(k.group == rooms::Group::pub ? notice::Need::public_room
                                                                        : notice::Need::rooms_list),
                         Pending::join_card))
            return;
    }
    if (k.pw && k.group != rooms::Group::shared)
    {
        pending_card_ = k;
        char t[160];
        std::snprintf(t, sizeof t, tr(S::kb_title_pw), k.name[0] ? k.name : k.host);
        open_keyboard(Kb::password, t, "", 64, true);
        return;
    }
    join_card(k);
}

void App::join_card(const rooms::Card &k) noexcept
{
    platform::blip(platform::Blip::choose);
    join_card_ = k;
    join_from_card_ = true;
    if (k.group == rooms::Group::shared)
    {
        has_invite_ = false;
        join_invite(invite_);
        return;
    }
    JoinExtras x = default_extras();
    std::snprintf(x.device, sizeof x.device, "%s", device_);
    std::snprintf(x.password, sizeof x.password, "%s", kb_for_ == Kb::password ? kb_.text() : "");
    std::snprintf(x.pk, sizeof x.pk, "%s", k.pk);
    for_host_ = false;
    if (k.group == rooms::Group::nearby)
    {
        std::snprintf(x.via, sizeof x.via, "%s", "nearby");
        std::snprintf(addr_, sizeof addr_, "%s", k.addr);
        port_ = k.port;
        std::snprintf(engine_name_, sizeof engine_name_, "%s", k.host);
        target_ = JoinTarget{};
        std::snprintf(target_.addr, sizeof target_.addr, "%s", k.addr);
        target_.port = k.port;
        std::snprintf(target_.name, sizeof target_.name, "%s", k.host);
        start_join_how("", x);
        return;
    }
    // A room on the hub (public, or a friend's): this home's PS5Upload knocks on its lobby and
    // plays it (no WebRTC in the app yet). Without one: say what is needed (the fallback rule).
    char own[64] = {};
    std::uint16_t own_port = 0;
    if (!own_ps5upload(own, sizeof own, &own_port))
    {
        std::snprintf(note_, sizeof note_, "%s", tr(S::rooms_need_home));
        note_until_ = net::now_us() + 10000000;
        platform::blip(platform::Blip::bad);
        log("app: a room over the internet, and no PS5Upload on this network to play it through");
        return;
    }
    std::snprintf(x.via, sizeof x.via, "%s", k.group == rooms::Group::friends ? "friends" : "rooms");
    route::Candidate way;
    way.kind = route::Kind::internet;
    way.way = route::Way::bridge;
    way.lobby = true;
    std::snprintf(way.addr, sizeof way.addr, "%s", own);
    way.port = own_port;
    std::snprintf(way.room, sizeof way.room, "%s", k.room);
    std::snprintf(addr_, sizeof addr_, "%s", own);
    port_ = own_port;
    std::snprintf(engine_name_, sizeof engine_name_, "%s", k.host);
    target_ = JoinTarget{};
    std::snprintf(target_.name, sizeof target_.name, "%s", k.host);
    x.remember = true;
    start_join_how("", x, &way, 1);
}

void App::input_rooms(std::uint32_t p) noexcept
{
    JoinItem items[64];
    const int n = join_items(items, 64);
    if (p & pad::kCircle)
    {
        rooms_net_->clear_details();
        return back();
    }
    if (p & (pad::kL1 | pad::kR1))
    {
        const int c = static_cast<int>(rooms::Sort::count_);
        sort_ = static_cast<rooms::Sort>((static_cast<int>(sort_) + ((p & pad::kL1) ? c - 1 : 1)) % c);
        std::snprintf(settings_.sort, sizeof settings_.sort, "%s", rooms::sort_param(sort_));
        save();
        if (!fake_.on)
            rooms_net_->refresh(sort_, filter_);
        platform::blip(platform::Blip::move);
        return;
    }
    if (p & pad::kTriangle)
    {
        platform::blip(platform::Blip::choose);
        go(Screen::filter);
        return;
    }
    if (p & kUp)
        sel_ = (sel_ + n - 1) % n, platform::blip(platform::Blip::move);
    if (p & kDown)
        sel_ = (sel_ + 1) % n, platform::blip(platform::Blip::move);
    sel_ = clampi(sel_, 0, n - 1);
    const JoinItem &it = items[sel_];
    if ((p & pad::kSquare) && it.kind == JoinItem::card && it.c.group != rooms::Group::shared)
    {
        pending_card_ = it.c;
        if (it.c.group == rooms::Group::pub && !need_notice(static_cast<int>(notice::Need::rooms_list), Pending::details))
            return;
        rooms_net_->details(it.c);
        platform::blip(platform::Blip::choose);
        go(Screen::details);
        return;
    }
    if (!(p & pad::kCross))
        return;
    switch (it.kind)
    {
    case JoinItem::card: choose_card(it.c); break;
    case JoinItem::retry:
        platform::blip(platform::Blip::choose);
        rooms_net_->refresh(sort_, filter_);
        break;
    case JoinItem::more:
        platform::blip(platform::Blip::choose);
        rooms_net_->more();
        break;
    case JoinItem::code:
        platform::blip(platform::Blip::choose);
        for_host_ = false;
        go(Screen::pick); // which computer, then the code
        break;
    case JoinItem::address:
        platform::blip(platform::Blip::choose);
        for_host_ = false;
        typed_[0] = 0;
        go(Screen::address);
        break;
    }
}

void App::draw_card(Canvas &c, int x, int y, int w, int h, bool sel, const rooms::Card &k) noexcept
{
    const bool dim = rooms::full(k) || !rooms::compatible(k);
    c.round_rect(x, y, w, h, 18, sel ? col::panel_hi : col::panel, sel ? 245 : 205);
    if (sel)
        c.round_frame(x, y, w, h, 18, 3, col::accent);
    const std::uint32_t tc = dim ? col::muted : col::text;
    char line1[200], line2[240], ms[48];
    // [game] Room name · Host · tag / Game · 2/4 · lock · language
    switch (k.group)
    {
    case rooms::Group::nearby:
        std::snprintf(line1, sizeof line1, tr(S::home_nearby), k.host);
        std::snprintf(line2, sizeof line2, "%s", tr(S::home_nearby_sub));
        break;
    case rooms::Group::shared:
        std::snprintf(line1, sizeof line1, tr(S::card_from), k.from);
        std::snprintf(line2, sizeof line2, "%s", tr(S::home_invite_sub));
        break;
    default:
    {
        std::snprintf(line1, sizeof line1, "%s · %s%s%s", k.name[0] ? k.name : k.host, k.host, k.tag[0] ? " · " : "", k.tag);
        int o = std::snprintf(line2, sizeof line2, "%s", k.game_title[0] ? k.game_title : "");
        if (k.max > 0)
            o += std::snprintf(line2 + o, sizeof line2 - static_cast<std::size_t>(o), "%s%d/%d", o ? " · " : "", k.in, k.max);
        if (k.pw && o < static_cast<int>(sizeof line2))
            o += std::snprintf(line2 + o, sizeof line2 - static_cast<std::size_t>(o), " · %s", tr(S::card_pw));
        if (k.lang[0] && o < static_cast<int>(sizeof line2))
        {
            const int li = ui::i18n::find(k.lang);
            o += std::snprintf(line2 + o, sizeof line2 - static_cast<std::size_t>(o), " · %s",
                               li >= 0 ? ui::i18n::name(li) : k.lang);
        }
        break;
    }
    }
    // right side: the round trip (bars + words) and what ✕ does
    bool measured = false;
    const int e = rooms::expected_ms(k, &measured);
    const char *act = rooms::full(k) ? tr(S::card_full)
                      : !rooms::compatible(k) ? tr(k.proto > rooms::kProto ? S::card_update : S::card_no_ps5)
                      : k.group == rooms::Group::friends || k.group == rooms::Group::shared ? tr(S::card_join)
                                                                                             : tr(S::card_ask);
    ms[0] = 0;
    if (measured)
        std::snprintf(ms, sizeof ms, tr(S::card_measured), e);
    else if (k.history_ms >= 0)
        std::snprintf(ms, sizeof ms, tr(S::card_last), k.history_ms);
    else if (e >= 0)
        std::snprintf(ms, sizeof ms, tr(S::card_about_ms), e);
    const int rw = 340;
    const int rx = x + w - rw;
    c.text(Font::title, x + 32, y + 22, line1, tc, w - rw - 64);
    c.text(Font::body, x + 32, y + 22 + ui::font_height(Font::title) + 8, line2, col::muted, w - rw - 64);
    if (e >= 0)
    {
        draw_bars(c, rx, y + 24, rooms::bars_for_ms(e), !measured);
        c.text(Font::small, rx + 66, y + 28, ms, col::muted, rw - 80);
    }
    if (sel)
    {
        const int bw = c.button(Canvas::Glyph::cross, rx, y + h - 64, 40);
        c.text(Font::body, rx + bw + 12, y + h - 64 + 20 - ui::font_height(Font::body) / 2, act, dim ? col::muted : col::text);
    }
    else
        c.text(Font::small, rx, y + h - 54, act, col::muted);
}

void App::draw_rooms(Canvas &c) noexcept
{
    char t[160];
    draw_frame(c, tr(S::rooms_title));
    std::snprintf(t, sizeof t, tr(S::rooms_sort), tr(sort_label(sort_)));
    int x = kLeft + Canvas::text_width(Font::title, tr(S::rooms_title)) + 60;
    x += c.button(Canvas::Glyph::l1, x, kTop + 6, 40) + 10;
    x += c.text(Font::body, x, kTop + 10, t, col::muted) + 10;
    c.button(Canvas::Glyph::r1, x, kTop + 6, 40);
    if (filter_.any())
        c.text(Font::small, 1340, kTop + 16, tr(S::flt_title), col::accent);
    JoinItem items[64];
    const int n = join_items(items, 64);
    sel_ = clampi(sel_, 0, n - 1);
    // the list: cards 2x a row's height, section headers when the group changes
    const int w = 1160, ch = 132, lh = 96;
    int y = 200;
    // keep the selected one in view: about 6 cards fit
    int first = 0;
    {
        int hgt = 0;
        for (int i = sel_; i >= 0; --i)
        {
            hgt += items[i].kind == JoinItem::card ? ch + 14 : lh + 10;
            if (hgt > 760)
            {
                first = i + 1;
                break;
            }
        }
    }
    int last_group = -1;
    bool any_public = false;
    for (int i = 0; i < n; ++i)
        any_public = any_public || (items[i].kind == JoinItem::card && items[i].c.group == rooms::Group::pub);
    for (int i = first; i < n; ++i)
    {
        const JoinItem &it = items[i];
        // nothing below the hint bar
        if (y + (it.kind == JoinItem::card ? ch + 38 : lh) > 975)
            break;
        if (it.kind == JoinItem::card)
        {
            if (static_cast<int>(it.c.group) != last_group)
            {
                last_group = static_cast<int>(it.c.group);
                c.text(Font::small, kLeft + 6, y, tr(group_label(it.c.group)), col::muted, w - 12);
                y += 38;
            }
            draw_card(c, kLeft, y, w, ch, sel_ == i, it.c);
            y += ch + 14;
            continue;
        }
        if (last_group != 99)
        {
            // the end of the public rooms: an empty or error state above the ways in by hand
            const RoomsNet::View &v = rv_;
            const char *msg = nullptr;
            if (v.state == RoomsNet::State::loading || v.state == RoomsNet::State::idle)
                msg = tr(S::rooms_loading);
            else if (v.state == RoomsNet::State::off)
                msg = tr(S::rooms_off);
            else if (v.state == RoomsNet::State::ok && !any_public)
                msg = v.list.n ? tr(S::rooms_nomatch) : tr(S::rooms_none);
            if (msg && it.kind != JoinItem::retry)
            {
                c.text_wrap(Font::small, kLeft + 6, y, w, msg, col::muted);
                y += 46;
            }
            last_group = 99;
        }
        const char *title = "";
        const char *sub = nullptr;
        switch (it.kind)
        {
        case JoinItem::retry: title = tr(S::rooms_unreachable); break;
        case JoinItem::more: title = tr(S::rooms_more); break;
        case JoinItem::code: title = tr(S::rooms_code), sub = tr(S::way_code_sub); break;
        case JoinItem::address: title = tr(S::rooms_address), sub = tr(S::pick_type_sub); break;
        case JoinItem::card: break;
        }
        draw_row(c, kLeft, y, w, lh, sel_ == i, title, sub);
        y += lh + 10;
    }
    // right: the focused card's details strip (after 1 s focused)
    const JoinItem &cur = items[sel_];
    if (cur.kind == JoinItem::card && cur.c.group != rooms::Group::shared)
    {
        char key[72];
        card_key(cur.c, key, sizeof key);
        if (std::strcmp(key, focus_key_) != 0)
        {
            std::snprintf(focus_key_, sizeof focus_key_, "%s", key);
            focus_since_ = net::now_us();
        }
        else if (focus_since_ && net::now_us() - focus_since_ > kFocusUs && !fake_.on && rooms_net_)
        {
            // Details for a public card only once the notice is accepted (it is "from Rooms").
            if (cur.c.group != rooms::Group::pub || settings_.notice >= notice::kVersion)
                rooms_net_->details(cur.c);
            focus_since_ = 0;
        }
        if (!std::strcmp(rv_.detail_key, cur.c.room[0] ? cur.c.room : "") || fake_.on ||
            (cur.c.addr[0] && rv_.detail != RoomsNet::Detail::none))
            draw_details_panel(c, 1340, 200, 500, cur.c, false);
    }
    else
        focus_key_[0] = 0;
    const int g[] = {G_CROSS, G_SQUARE, G_TRIANGLE, G_CIRCLE};
    const S l[] = {S::hint_choose, S::hint_details, S::hint_filter, S::hint_back};
    draw_hints(c, g, l, 4);
}

// ---- Details ----------------------------------------------------------------------------------
void App::draw_details_panel(Canvas &c, int x, int y, int w, const rooms::Card &k, bool full) noexcept
{
    const RoomsNet::View &v = rv_;
    char b[320];
    const rooms::Preview &p = v.preview;
    const char *host = p.host[0] ? p.host : k.host;
    if (full)
    {
        c.text(Font::title, x, y, p.room_name[0] ? p.room_name : k.name[0] ? k.name : host, col::text, w);
        y += 64;
    }
    if (v.detail == RoomsNet::Detail::loading)
    {
        c.text(Font::small, x, y, tr(S::det_loading), col::muted, w);
        return;
    }
    if (v.detail != RoomsNet::Detail::ok)
    {
        const S why = v.detail == RoomsNet::Detail::old_host    ? S::det_old
                      : v.detail == RoomsNet::Detail::no_direct ? S::det_no_direct
                      : v.detail == RoomsNet::Detail::no_answer ? S::det_no_answer
                      : v.detail == RoomsNet::Detail::gone      ? S::det_gone
                                                                : S::det_loading;
        if (v.detail != RoomsNet::Detail::none)
            y += c.text_wrap(Font::small, x, y, w, tr(why), col::muted) + 16;
    }
    else
    {
        std::snprintf(b, sizeof b, tr(S::det_host), host, p.tag[0] ? p.tag : k.tag);
        c.text(Font::small, x, y, b, col::text, w);
        y += 40;
        if (p.game_title[0])
        {
            c.text(Font::small, x, y, p.game_title, col::text, w);
            y += 40;
        }
        if (p.res[0])
        {
            std::snprintf(b, sizeof b, tr(S::det_stream), p.res, p.fps, !std::strcmp(p.codec, "h264") ? "H.264" : "HEVC",
                          (p.kbps + 500) / 1000);
            c.text(Font::small, x, y, b, col::muted, w);
            y += 40;
        }
        if (p.playing_min >= 0)
        {
            std::snprintf(b, sizeof b, tr(S::det_playing), p.playing_min);
            c.text(Font::small, x, y, b, col::muted, w);
            y += 40;
        }
        if (p.n_players)
        {
            c.text(Font::small, x, y, tr(S::det_players), col::muted);
            y += 38;
            for (int i = 0; i < p.n_players; ++i)
            {
                std::snprintf(b, sizeof b, tr(S::det_seat), p.players[i].seat, p.players[i].name);
                const int bars = rooms::bars_of(p.players[i].tier);
                if (bars)
                    draw_bars(c, x, y + 2, bars, false);
                c.text(Font::small, x + 66, y + 4, b, col::text, w - 70);
                y += 38;
            }
        }
        if (p.pw)
        {
            c.text(Font::small, x, y, tr(S::det_pw), col::muted, w);
            y += 38;
        }
    }
    // your expected round trip: measured on a direct way, else the estimate
    bool measured = false;
    rooms::Card kk = k;
    if (v.measured_ms >= 0)
        kk.measured_ms = v.measured_ms;
    const int e = rooms::expected_ms(kk, &measured);
    y += 10;
    if (e < 0)
        std::snprintf(b, sizeof b, tr(S::det_you_unknown), host);
    else if (measured)
    {
        static const S tiers[] = {S::tier_down, S::tier_poor, S::tier_playable, S::tier_good, S::tier_excellent};
        std::snprintf(b, sizeof b, tr(S::det_you_measured), host, tr(tiers[rooms::bars_for_ms(e)]), e);
    }
    else
        std::snprintf(b, sizeof b, tr(S::det_you_about), host, e);
    if (e >= 0)
        draw_bars(c, x, y + 2, rooms::bars_for_ms(e), !measured);
    c.text_wrap(Font::small, x + (e >= 0 ? 66 : 0), y + 4, w - 70, b, col::text);
}

void App::input_details(std::uint32_t p) noexcept
{
    if (p & pad::kCircle)
        return back();
    if (p & pad::kCross)
    {
        const rooms::Card k = pending_card_;
        back();
        choose_card(k);
    }
}

void App::draw_details(Canvas &c) noexcept
{
    draw_frame(c, tr(S::det_title));
    draw_details_panel(c, kLeft, 200, 1300, pending_card_, true);
    const int g[] = {G_CROSS, G_CIRCLE};
    const S l[] = {S::card_ask, S::hint_back};
    draw_hints(c, g, l, 2);
}

// ---- the filter sheet -------------------------------------------------------------------------
void App::input_filter(std::uint32_t p) noexcept
{
    constexpr int kRows = 6;
    if (p & (pad::kCircle | pad::kTriangle))
    {
        settings_.f_space = filter_.space, settings_.f_nopw = filter_.nopw, settings_.f_myver = filter_.myver;
        settings_.f_near = filter_.near;
        std::snprintf(settings_.f_lang, sizeof settings_.f_lang, "%s", filter_.lang);
        save();
        if (!fake_.on)
            rooms_net_->refresh(sort_, filter_);
        return back();
    }
    if (p & pad::kSquare)
    {
        filter_ = rooms::Filter{};
        platform::blip(platform::Blip::back);
        return;
    }
    if (p & kUp)
        sel_ = (sel_ + kRows - 1) % kRows, platform::blip(platform::Blip::move);
    if (p & kDown)
        sel_ = (sel_ + 1) % kRows, platform::blip(platform::Blip::move);
    const bool left = p & kLeftB;
    if (!(p & (pad::kCross | kLeftB | kRight)))
        return;
    platform::blip(platform::Blip::choose);
    switch (sel_)
    {
    case 0: filter_.space = !filter_.space; break;
    case 1: filter_.nopw = !filter_.nopw; break;
    case 2: filter_.myver = !filter_.myver; break;
    case 3: filter_.near = !filter_.near; break;
    case 4:
    {
        const int n = ui::i18n::count();
        int at = filter_.lang[0] ? ui::i18n::find(filter_.lang) : -1;
        at = (at + 1 + (left ? n : 1)) % (n + 1) - 1;
        std::snprintf(filter_.lang, sizeof filter_.lang, "%s", at < 0 ? "" : ui::i18n::code(at));
        break;
    }
    case 5:
        if (p & pad::kCross)
            open_keyboard(Kb::game, tr(S::kb_title_game), filter_.game, 40, false);
        else
            filter_.game[0] = 0;
        break;
    }
}

void App::draw_filter(Canvas &c) noexcept
{
    draw_frame(c, tr(S::flt_title));
    const S rows[] = {S::flt_space, S::flt_nopw, S::flt_myver, S::flt_near};
    const bool vals[] = {filter_.space, filter_.nopw, filter_.myver, filter_.near};
    int y = 220;
    for (int i = 0; i < 4; ++i, y += 106)
        draw_row(c, kLeft, y, 1100, 90, sel_ == i, tr(rows[i]), nullptr, tr(vals[i] ? S::on : S::off));
    const int li = filter_.lang[0] ? ui::i18n::find(filter_.lang) : -1;
    draw_row(c, kLeft, y, 1100, 90, sel_ == 4, tr(S::flt_lang), nullptr, li >= 0 ? ui::i18n::name(li) : tr(S::flt_any));
    y += 106;
    draw_row(c, kLeft, y, 1100, 90, sel_ == 5, tr(S::flt_game), nullptr, filter_.game[0] ? filter_.game : tr(S::flt_game_any));
    const int g[] = {G_CROSS, G_SQUARE, G_CIRCLE};
    const S l[] = {S::hint_change, S::hint_clear, S::hint_back};
    draw_hints(c, g, l, 3);
}

// ---- the on-screen keyboard -------------------------------------------------------------------
void App::open_keyboard(Kb why, const char *title, const char *text, int max_chars, bool secret) noexcept
{
    kb_for_ = why;
    std::snprintf(kb_title_, sizeof kb_title_, "%s", title);
    kb_.reset(text, max_chars, secret);
    kb_typed_us_ = 0;
    go(Screen::keyboard);
}

void App::fake_keyboard(const char *title, const char *text, bool secret) noexcept
{
    std::snprintf(kb_title_, sizeof kb_title_, "%s", title);
    kb_.reset(text, 64, secret);
}

void App::input_keyboard(std::uint32_t p, std::uint32_t) noexcept
{
    if (p & pad::kCircle)
    {
        kb_.reset("", 1, false);
        pending_ = Pending::none;
        return back();
    }
    if (p & kUp)
        kb_.move(0, -1);
    if (p & kDown)
        kb_.move(0, 1);
    if (p & kLeftB)
        kb_.move(-1, 0);
    if (p & kRight)
        kb_.move(1, 0);
    if (p & (kUp | kDown | kLeftB | kRight))
        platform::blip(platform::Blip::move);
    if (p & pad::kSquare)
        kb_.del(), platform::blip(platform::Blip::back);
    if (p & pad::kTriangle)
        kb_.space(), kb_typed_us_ = net::now_us(), platform::blip(platform::Blip::choose);
    if (p & pad::kL1)
        kb_.toggle_symbols(), platform::blip(platform::Blip::move);
    if (p & pad::kR1)
        kb_.toggle_shift(), platform::blip(platform::Blip::move);
    bool done = (p & (pad::kR2 | pad::kOptions)) != 0;
    if (p & pad::kCross)
    {
        kb_typed_us_ = net::now_us();
        done = kb_.press() || done;
        platform::blip(platform::Blip::choose);
    }
    if (done)
    {
        // replace the keyboard with what comes next
        if (depth_ > 0)
            screen_ = stack_[--depth_];
        sel_ = 0;
        done_keyboard();
    }
}

void App::done_keyboard() noexcept
{
    const char *t = kb_.text();
    switch (kb_for_)
    {
    case Kb::password:
    {
        if (!*t)
            return;
        const GuestView gv = guest_view();
        if (gv.phase == GuestView::password && guest_->running())
        {
            // the engine's challenge is waiting: sealed and sent now
            guest_->send_password(t);
            return;
        }
        join_card(pending_card_);
        return;
    }
    case Kb::name:
    {
        char clean[48];
        if (!rooms::clean_name(t, clean, sizeof clean))
            return;
        std::snprintf(join_name_, sizeof join_name_, "%s", clean);
        // the same way again with the new name
        start_join_how(code_, last_extras_, n_last_ways_ ? last_ways_ : nullptr, n_last_ways_);
        return;
    }
    case Kb::chat:
        if (*t && !guest_->send_text(t))
        {
            std::snprintf(note_, sizeof note_, "%s", tr(S::chat_slow));
            note_until_ = net::now_us() + 3000000;
        }
        return;
    case Kb::game:
        std::snprintf(filter_.game, sizeof filter_.game, "%s", t);
        return;
    case Kb::room_name:
    {
        char clean[48];
        if (!rooms::clean_name(t, clean, sizeof clean))
            return;
        char js[120];
        json::Writer w(js, sizeof js);
        w.str(clean);
        if (w.ok())
            host_->room_set("name", js);
        return;
    }
    case Kb::room_pw:
    {
        if (!rooms::password_ok(t))
        {
            std::snprintf(note_, sizeof note_, "%s", tr(S::pw_short));
            note_until_ = net::now_us() + 4000000;
            platform::blip(platform::Blip::bad);
            return;
        }
        char js[300];
        json::Writer w(js, sizeof js);
        w.str(t);
        if (w.ok())
            host_->room_set("password", js);
        return;
    }
    }
}

void App::draw_keyboard(Canvas &c) noexcept
{
    draw_frame(c, kb_title_);
    c.text(Font::small, kLeft, kTop + 70, tr(S::kb_help), col::muted);
    c.round_rect(kLeft, 220, 1560, 110, 16, col::panel, 230);
    c.round_frame(kLeft, 220, 1560, 110, 16, 3, col::accent);
    char shown[300];
    kb_.shown(shown, sizeof shown - 2, kb_typed_us_ && net::now_us() - kb_typed_us_ < 1000000);
    std::strncat(shown, "_", sizeof shown - std::strlen(shown) - 1);
    c.text(Font::title, kLeft + 30, 220 + (110 - ui::font_height(Font::title)) / 2, shown, col::text, 1500);
    const int kw = 144, kh = 104, kg = 14, ky = 380;
    for (int r = 0; r < textkb::kRows; ++r)
        for (int col_i = 0; col_i < textkb::kCols; ++col_i)
        {
            const char *label;
            const textkb::Special sp = kb_.key(r, col_i, &label);
            int span = 1;
            if (r == 3)
            {
                // the wide keys draw once, at their first column
                if (sp == textkb::k_space && col_i != 2)
                    continue;
                if (sp == textkb::k_delete && col_i != 7)
                    continue;
                span = sp == textkb::k_space ? 5 : sp == textkb::k_delete ? 2 : 1;
            }
            const int kx = kLeft + col_i * (kw + kg), yy = ky + r * (kh + kg);
            const int ww = span * kw + (span - 1) * kg;
            const bool sel = kb_.row() == r && (kb_.col() == col_i || (span > 1 && kb_.col() >= col_i && kb_.col() < col_i + span));
            const bool lit = (sp == textkb::k_shift && kb_.shift()) || (sp == textkb::k_symbols && kb_.symbols());
            c.round_rect(kx, yy, ww, kh, 14, sel ? col::accent : lit ? col::accent_dim : col::panel, sel ? 255 : 220);
            const char *t = label;
            if (sp == textkb::k_shift)
                t = tr(S::kb_shift);
            else if (sp == textkb::k_symbols)
                t = tr(kb_.symbols() ? S::kb_letters : S::kb_symbols);
            else if (sp == textkb::k_space)
                t = tr(S::kb_space);
            else if (sp == textkb::k_delete)
                t = "←";
            else if (sp == textkb::k_done)
                t = tr(S::kb_done);
            const Font f = sp == textkb::k_char ? Font::title : Font::small;
            c.text_center(f, kx + ww / 2, yy + (kh - ui::font_height(f)) / 2, t, sel ? col::on_accent : col::text);
        }
    const int g[] = {G_CROSS, G_SQUARE, G_TRIANGLE, G_CIRCLE};
    const S l[] = {S::hint_type, S::hint_delete, S::kb_space, S::hint_back};
    draw_hints(c, g, l, 4);
}

// ---- Thanks & Support -------------------------------------------------------------------------
void App::input_thanks(std::uint32_t p) noexcept
{
    if (p & pad::kCircle)
        return back();
    const int pages = n_names_ / 48 + 1;
    if (p & (pad::kR1 | kRight | kDown))
        thanks_page_ = (thanks_page_ + 1) % pages, platform::blip(platform::Blip::move);
    if (p & (pad::kL1 | kLeftB | kUp))
        thanks_page_ = (thanks_page_ + pages - 1) % pages, platform::blip(platform::Blip::move);
}

void App::draw_thanks(Canvas &c) noexcept
{
    load_supporters();
    draw_frame(c, nullptr);
    // a heart, drawn (two circles and a triangle)
    const int hx = kLeft + 30, hy = 130;
    c.circle(hx, hy, 22, col::bad);
    c.circle(hx + 34, hy, 22, col::bad);
    for (int i = 0; i < 39; ++i)
        c.fill(hx - 22 + i, hy + 6 + i, 78 - 2 * i, 1, col::bad);
    // the title size: the big sizes carry only ASCII, and translations need the text sizes
    c.text(Font::title, kLeft + 110, 108, tr(S::thanks_title), col::text, 1100);
    char b[200];
    std::snprintf(b, sizeof b, tr(S::thanks_count), n_names_, n_anonymous_);
    c.text(Font::body, kLeft, 210, b, col::muted, 1100);
    c.text(Font::small, kLeft, 260, tr(S::thanks_text), col::muted, 1100);
    // the names, most first (the list's own order), three columns of 16
    const int first = thanks_page_ * 48;
    for (int i = first; i < n_names_ && i < first + 48; ++i)
    {
        const int k = i - first;
        c.text(Font::small, kLeft + (k / 16) * 370, 320 + (k % 16) * 38, names_[i], col::text, 350);
    }
    // two QR codes: Ko-fi and the site's thanks page (scan with a phone)
    static std::uint8_t qr[qrcodegen_BUFFER_LEN_MAX], tmp[qrcodegen_BUFFER_LEN_MAX];
    const char *urls[2] = {kofi_, thanks_url_};
    const S labels[2] = {S::thanks_kofi, S::thanks_more};
    for (int i = 0; i < 2; ++i)
    {
        const int x = 1360, y = 150 + i * 420;
        c.text(Font::body, x, y, tr(labels[i]), col::text, 480);
        if (qrcodegen_encodeText(urls[i], tmp, qr, qrcodegen_Ecc_MEDIUM, 1, 20, qrcodegen_Mask_AUTO, true))
        {
            const int size = qrcodegen_getSize(qr);
            const int module = 300 / (size + 4) > 2 ? 300 / (size + 4) : 2;
            c.qr(qr, x, y + 50, module);
        }
    }
    c.text(Font::small, 1360, 990, tr(S::thanks_scan), col::muted);
    const int g[] = {G_R1, G_CIRCLE};
    const S l[] = {S::hint_next, S::hint_back};
    draw_hints(c, g, l, 2);
}

// ---- the first-run guide and the controls -----------------------------------------------------
void App::input_welcome(std::uint32_t p) noexcept
{
    if ((p & pad::kCircle) || ((p & pad::kCross) && welcome_page_ >= 2))
    {
        settings_.welcome_done = true;
        save();
        welcome_page_ = 0;
        platform::blip(platform::Blip::choose);
        return back();
    }
    if (p & (pad::kCross | kRight))
        welcome_page_ = welcome_page_ < 2 ? welcome_page_ + 1 : 2, platform::blip(platform::Blip::move);
    if ((p & kLeftB) && welcome_page_ > 0)
        --welcome_page_, platform::blip(platform::Blip::move);
}

void App::draw_welcome(Canvas &c) noexcept
{
    draw_frame(c, nullptr);
    c.image(logo_, 1480, 330, 255);
    c.text(Font::big, kLeft, 140, tr(S::app_name), col::text);
    const S pages[] = {S::wel_1, S::wel_2, S::wel_3};
    c.text_wrap(Font::title, kLeft, 330, 1200, tr(pages[welcome_page_ % 3]), col::text);
    // page dots
    for (int i = 0; i < 3; ++i)
        c.circle(kLeft + 14 + i * 40, 820, 10, i == welcome_page_ ? col::accent : col::panel_hi);
    const int g[] = {G_CROSS, G_CIRCLE};
    const S l[] = {welcome_page_ >= 2 ? S::wel_got_it : S::hint_next, S::hint_skip};
    draw_hints(c, g, l, 2);
}

void App::input_help(std::uint32_t p) noexcept
{
    if (p & (pad::kCircle | pad::kTriangle | pad::kCross))
        back();
}

void App::draw_help(Canvas &c) noexcept
{
    draw_frame(c, tr(S::help_title));
    const S lines[] = {S::help_move, S::help_tabs, S::help_ps, S::help_leave, S::help_room, S::help_talk};
    int y = 230;
    for (const S s : lines)
        y += c.text_wrap(Font::body, kLeft, y, 1500, tr(s), col::text) + 28;
    const int g[] = {G_CIRCLE};
    const S l[] = {S::hint_back};
    draw_hints(c, g, l, 1);
}
} // namespace play
