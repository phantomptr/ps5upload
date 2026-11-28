// PS5Upload Play - Play Rooms: lists, previews, the room push, sorting and filtering.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "rooms.hpp"

#include "json.hpp"

#include <cstdio>
#include <cstring>
#include <pthread.h>

namespace play::rooms
{
namespace
{
constexpr std::uint32_t kTokens = 2048;

void copy(char *dst, std::size_t cap, const json::Doc &d, int i) noexcept
{
    if (!d.str(i, dst, cap))
        dst[0] = 0;
}

int num_or(const json::Doc &d, int i, int fallback) noexcept
{
    return d.type(i) == json::Type::number ? static_cast<int>(d.integer(i)) : fallback;
}

bool has_cap(const json::Doc &d, int caps, const char *name) noexcept
{
    for (std::uint32_t i = 0; i < d.count(caps); ++i)
        if (d.str_is(d.item(caps, i), name))
            return true;
    return false;
}

char lower(char c) noexcept
{
    return c >= 'A' && c <= 'Z' ? static_cast<char>(c - 'A' + 'a') : c;
}

// Case-insensitive "contains" over ASCII (titles and ids).
bool contains(const char *hay, const char *needle) noexcept
{
    if (!*needle)
        return true;
    for (; *hay; ++hay)
    {
        std::size_t k = 0;
        while (needle[k] && hay[k] && lower(hay[k]) == lower(needle[k]))
            ++k;
        if (!needle[k])
            return true;
    }
    return false;
}

int cmp_ci(const char *a, const char *b) noexcept
{
    for (; *a && *b; ++a, ++b)
    {
        const char x = lower(*a), y = lower(*b);
        if (x != y)
            return x < y ? -1 : 1;
    }
    return *a ? 1 : *b ? -1 : 0;
}

void read_card(const json::Doc &d, int r, Card &c) noexcept
{
    copy(c.room, sizeof c.room, d, d.get(r, "room"));
    copy(c.name, sizeof c.name, d, d.get(r, "name"));
    copy(c.host, sizeof c.host, d, d.get(r, "host"));
    copy(c.tag, sizeof c.tag, d, d.get(r, "tag"));
    const int g = d.get(r, "game");
    copy(c.game_title, sizeof c.game_title, d, d.get(g, "title"));
    copy(c.game_id, sizeof c.game_id, d, d.get(g, "id"));
    c.proto = num_or(d, d.get(r, "proto"), 1);
    copy(c.ver, sizeof c.ver, d, d.get(r, "ver"));
    const int caps = d.get(r, "caps");
    c.cap_ps5app = has_cap(d, caps, "ps5app");
    c.cap_preview = has_cap(d, caps, "preview");
    c.cap_chat = has_cap(d, caps, "chat");
    c.cap_voice = has_cap(d, caps, "voice");
    c.cap_knock = has_cap(d, caps, "knock") || c.proto >= 2;
    const int slots = d.get(r, "slots");
    if (d.count(slots) >= 2)
    {
        c.in = num_or(d, d.item(slots, 0), 0);
        c.max = num_or(d, d.item(slots, 1), 0);
    }
    c.pw = d.boolean(d.get(r, "pw"));
    c.locked = d.boolean(d.get(r, "locked"));
    copy(c.vis, sizeof c.vis, d, d.get(r, "visibility"));
    copy(c.lang, sizeof c.lang, d, d.get(r, "lang"));
    copy(c.region, sizeof c.region, d, d.get(r, "region"));
    copy(c.pk, sizeof c.pk, d, d.get(r, "pk"));
    c.est_ms = num_or(d, d.get(r, "est_ms"), -1);
    c.age_s = num_or(d, d.get(r, "age_s"), 0);
}
} // namespace

Vis vis_from(const char *s) noexcept
{
    if (s && !std::strcmp(s, "public"))
        return Vis::pub;
    if (s && !std::strcmp(s, "friends"))
        return Vis::friends;
    return Vis::priv;
}

const char *vis_word(Vis v) noexcept
{
    return v == Vis::pub ? "public" : v == Vis::friends ? "friends" : "private";
}

int bars_of(const char *tier) noexcept
{
    if (!tier)
        return 0;
    if (!std::strcmp(tier, "excellent"))
        return 4;
    if (!std::strcmp(tier, "good"))
        return 3;
    if (!std::strcmp(tier, "playable"))
        return 2;
    if (!std::strcmp(tier, "poor"))
        return 1;
    return 0;
}

int bars_for_ms(int ms) noexcept
{
    return ms < 0 ? 0 : ms <= 20 ? 4 : ms <= 40 ? 3 : ms <= 80 ? 2 : 1;
}

bool parse_list(const char *text, std::size_t len, List &list) noexcept
{
    static json::Token toks[kTokens]; // one reader at a time: the rooms thread's
    json::Doc d(toks, kTokens);
    if (!d.parse(text, len) || d.type(0) != json::Type::object)
        return false;
    const int rooms = d.get(0, "rooms");
    if (d.type(rooms) != json::Type::array)
        return false;
    const int pr = d.get(0, "proto");
    list.proto_min = num_or(d, d.get(pr, "min"), list.proto_min);
    list.proto_latest = num_or(d, d.get(pr, "latest"), list.proto_latest);
    const int app = d.get(0, "app");
    if (d.type(app) == json::Type::object)
        copy(list.app_latest, sizeof list.app_latest, d, d.get(app, "latest"));
    const int you = d.get(0, "you");
    if (d.type(you) == json::Type::object)
    {
        copy(list.country, sizeof list.country, d, d.get(you, "country"));
        copy(list.region, sizeof list.region, d, d.get(you, "region"));
    }
    list.off = d.boolean(d.get(0, "off"));
    list.next = num_or(d, d.get(0, "next"), -1);
    for (std::uint32_t i = 0; i < d.count(rooms) && list.n < List::kMax; ++i)
    {
        Card c;
        read_card(d, d.item(rooms, i), c);
        if (!c.room[0])
            continue;
        // the same room twice (a page boundary moved): keep the first
        bool dup = false;
        for (int k = 0; k < list.n && !dup; ++k)
            dup = !std::strcmp(list.cards[k].room, c.room);
        if (!dup)
            list.cards[list.n++] = c;
    }
    return true;
}

int parse_status(const char *text, std::size_t len, Card *cards, int n, int *every_s) noexcept
{
    static json::Token toks[512];
    json::Doc d(toks, 512);
    if (!d.parse(text, len) || d.type(0) != json::Type::object)
        return n;
    if (every_s)
        *every_s = num_or(d, d.get(0, "every_s"), 60);
    // {"every_s": 60, "rooms": {"<id>": {...}}} (PROTOCOL.md "Rooms")
    const int root = d.type(d.get(0, "rooms")) == json::Type::object ? d.get(0, "rooms") : 0;
    int kept = 0;
    for (int i = 0; i < n; ++i)
    {
        const int r = cards[i].room[0] ? d.get(root, cards[i].room) : -1;
        if (d.type(r) != json::Type::object)
            continue; // not open now
        Card c = cards[i];
        const int slots = d.get(r, "slots");
        if (d.count(slots) >= 2)
        {
            c.in = num_or(d, d.item(slots, 0), c.in);
            c.max = num_or(d, d.item(slots, 1), c.max);
        }
        const int g = d.get(r, "game");
        if (d.type(g) == json::Type::object)
        {
            copy(c.game_title, sizeof c.game_title, d, d.get(g, "title"));
            copy(c.game_id, sizeof c.game_id, d, d.get(g, "id"));
        }
        c.pw = d.boolean(d.get(r, "pw"));
        c.locked = d.boolean(d.get(r, "locked"));
        c.proto = num_or(d, d.get(r, "proto"), c.proto);
        if (d.type(d.get(r, "listed")) == json::Type::boolean)
            std::snprintf(c.vis, sizeof c.vis, "%s", d.boolean(d.get(r, "listed")) ? "public" : "friends");
        if (d.type(d.get(r, "pk")) == json::Type::string)
            copy(c.pk, sizeof c.pk, d, d.get(r, "pk"));
        cards[kept++] = c;
    }
    return kept;
}

const char *sort_param(Sort s) noexcept
{
    switch (s)
    {
    case Sort::latency: return "latency";
    case Sort::game: return "game";
    case Sort::players: return "players";
    case Sort::space: return "space";
    case Sort::newest: return "newest";
    default: return "best";
    }
}

Sort sort_from(const char *s) noexcept
{
    for (int i = 0; i < static_cast<int>(Sort::count_); ++i)
        if (s && !std::strcmp(s, sort_param(static_cast<Sort>(i))))
            return static_cast<Sort>(i);
    return Sort::best;
}

bool compatible(const Card &c) noexcept
{
    const int d = c.proto - kProto;
    if (d < -kProtoWindow || d > kProtoWindow)
        return false;
    // A PS5 app guest only where the host takes them (the ad's caps); nearby games and pushed
    // invites come from engines that already answered this app.
    return c.group != Group::pub || c.cap_ps5app;
}

bool full(const Card &c) noexcept
{
    return c.max > 0 && c.in >= c.max;
}

int expected_ms(const Card &c, bool *measured) noexcept
{
    if (measured)
        *measured = c.measured_ms >= 0;
    if (c.measured_ms >= 0)
        return c.measured_ms;
    if (c.history_ms >= 0)
        return c.history_ms;
    return c.est_ms;
}

bool passes(const Card &c, const Filter &f) noexcept
{
    if (f.space && full(c))
        return false;
    if (f.nopw && c.pw)
        return false;
    if (f.myver && !compatible(c))
        return false;
    if (f.lang[0] && c.lang[0] && std::strcmp(f.lang, c.lang) != 0)
        return false;
    if (f.near)
    {
        const int ms = expected_ms(c);
        if (ms < 0 || ms > 80)
            return false;
    }
    if (f.game[0] && !contains(c.game_title, f.game) && !contains(c.game_id, f.game))
        return false;
    return true;
}

namespace
{
// a before b?
bool before(const Card &a, const Card &b, Sort s) noexcept
{
    bool ma = false, mb = false;
    const int ea = expected_ms(a, &ma), eb = expected_ms(b, &mb);
    const int xa = ea < 0 ? 1 << 20 : ea, xb = eb < 0 ? 1 << 20 : eb;
    auto latency = [&]() -> int {
        if (xa != xb)
            return xa < xb ? -1 : 1;
        if (ma != mb)
            return ma ? -1 : 1; // measured beats estimated at equal values
        return 0;
    };
    switch (s)
    {
    case Sort::best:
    {
        const bool sa = !full(a), sb = !full(b);
        if (sa != sb)
            return sa;
        if (const int l = latency())
            return l < 0;
        const bool ca = compatible(a), cb = compatible(b);
        if (ca != cb)
            return ca;
        return a.age_s < b.age_s;
    }
    case Sort::latency:
        if (const int l = latency())
            return l < 0;
        return a.age_s < b.age_s;
    case Sort::game:
        if (const int g = cmp_ci(a.game_title, b.game_title))
            return g < 0;
        return latency() < 0;
    case Sort::players:
        if (a.in != b.in)
            return a.in > b.in;
        return latency() < 0;
    case Sort::space:
    {
        const int fa = a.max - a.in, fb = b.max - b.in;
        if (fa != fb)
            return fa > fb;
        return latency() < 0;
    }
    case Sort::newest:
    default:
        return a.age_s < b.age_s;
    }
}
} // namespace

void sort(Card *cards, int n, Sort s) noexcept
{
    // Stable insertion sort over the public cards only (at most a few pages).
    for (int i = 1; i < n; ++i)
    {
        if (cards[i].group != Group::pub)
            continue;
        for (int k = i; k > 0 && cards[k - 1].group == Group::pub && before(cards[k], cards[k - 1], s); --k)
        {
            const Card t = cards[k];
            cards[k] = cards[k - 1];
            cards[k - 1] = t;
        }
    }
}

std::size_t list_query(char *out, std::size_t cap, Sort s, const Filter &f, int page) noexcept
{
    char game[3 * 72 + 1] = {};
    std::size_t g = 0;
    for (const char *p = f.game; *p && g + 4 < sizeof game; ++p)
    {
        const unsigned char ch = static_cast<unsigned char>(*p);
        if ((ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z') || (ch >= '0' && ch <= '9') || ch == '-' ||
            ch == '.' || ch == '_')
            game[g++] = static_cast<char>(ch);
        else
            g += static_cast<std::size_t>(std::snprintf(game + g, sizeof game - g, "%%%02X", ch));
    }
    // page 0 is the first: no page key (the hub's `next` names the later ones)
    char pg[24] = {};
    if (page > 0)
        std::snprintf(pg, sizeof pg, "&page=%d", page);
    const int n = std::snprintf(out, cap, "/rooms?sort=%s%s%s%s%s&proto=%d%s%s%s%s", sort_param(s),
                                f.game[0] ? "&game=" : "", game, f.space ? "&free=1" : "", f.nopw ? "&nopw=1" : "",
                                kProto, f.lang[0] ? "&lang=" : "", f.lang, f.near ? "&near=1" : "", pg);
    return n > 0 && static_cast<std::size_t>(n) < cap ? static_cast<std::size_t>(n) : 0;
}

std::size_t status_query(char *out, std::size_t cap, const char *const *lobbies, int n) noexcept
{
    json::Writer w(out, cap);
    w.raw("/rooms/status?r=");
    int used = 0;
    for (int i = 0; i < n && used < 16; ++i)
    {
        const char *id = lobbies[i];
        bool ok = id && *id && std::strlen(id) < 32;
        for (const char *p = id; ok && *p; ++p)
            ok = (*p >= 'a' && *p <= 'z') || (*p >= 'A' && *p <= 'Z') || (*p >= '0' && *p <= '9') || *p == '-' ||
                 *p == '_';
        if (!ok)
            continue;
        if (used++)
            w.raw(",");
        w.raw(id);
    }
    return w.ok() && used ? w.len() : 0;
}

bool parse_preview(const char *text, std::size_t len, Preview &p) noexcept
{
    static json::Token toks[512];
    json::Doc d(toks, 512);
    p = Preview{};
    if (!d.parse(text, len) || d.type(0) != json::Type::object)
        return false;
    // The hub relays it as {"data": {...}} or the engine answers it bare.
    int r = 0;
    if (d.type(d.get(0, "data")) == json::Type::object)
        r = d.get(0, "data");
    p.proto = num_or(d, d.get(r, "proto"), 1);
    const int caps = d.get(r, "caps");
    p.cap_chat = has_cap(d, caps, "chat");
    p.cap_voice = has_cap(d, caps, "voice");
    p.cap_ready = has_cap(d, caps, "ready");
    p.cap_group = has_cap(d, caps, "group");
    const int h = d.get(r, "host");
    copy(p.host, sizeof p.host, d, d.get(h, "name"));
    copy(p.tag, sizeof p.tag, d, d.get(h, "tag"));
    copy(p.ver, sizeof p.ver, d, d.get(h, "ver"));
    p.playing_min = num_or(d, d.get(h, "playing_for_min"), -1);
    const int rm = d.get(r, "room");
    copy(p.room_name, sizeof p.room_name, d, d.get(rm, "name"));
    copy(p.vis, sizeof p.vis, d, d.get(rm, "visibility"));
    copy(p.approval, sizeof p.approval, d, d.get(rm, "approval"));
    copy(p.chat, sizeof p.chat, d, d.get(rm, "chat"));
    copy(p.lang, sizeof p.lang, d, d.get(rm, "lang"));
    copy(p.region, sizeof p.region, d, d.get(rm, "region"));
    p.size = num_or(d, d.get(rm, "size"), 0);
    p.watchers = num_or(d, d.get(rm, "watchers"), 0);
    p.pw = d.boolean(d.get(rm, "pw"));
    const int g = d.get(r, "game");
    copy(p.game_id, sizeof p.game_id, d, d.get(g, "id"));
    copy(p.game_title, sizeof p.game_title, d, d.get(g, "title"));
    copy(p.game_players, sizeof p.game_players, d, d.get(g, "players"));
    const int s = d.get(r, "stream");
    copy(p.res, sizeof p.res, d, d.get(s, "res"));
    copy(p.codec, sizeof p.codec, d, d.get(s, "codec"));
    p.fps = num_or(d, d.get(s, "fps"), 0);
    p.kbps = num_or(d, d.get(s, "kbps"), 0);
    p.hdr = d.boolean(d.get(s, "hdr"));
    const int l = d.get(r, "host_link");
    p.link_rtt = num_or(d, d.get(l, "rtt_ms"), -1);
    copy(p.link_tier, sizeof p.link_tier, d, d.get(l, "tier"));
    const int pl = d.get(r, "players");
    for (std::uint32_t i = 0; i < d.count(pl) && p.n_players < 4; ++i)
    {
        const int x = d.item(pl, i);
        auto &o = p.players[p.n_players++];
        copy(o.name, sizeof o.name, d, d.get(x, "name"));
        o.seat = num_or(d, d.get(x, "seat"), 0);
        copy(o.kind, sizeof o.kind, d, d.get(x, "kind"));
        copy(o.tier, sizeof o.tier, d, d.get(x, "tier"));
        o.rtt_ms = num_or(d, d.get(x, "rtt_ms"), -1);
    }
    p.asking = num_or(d, d.get(r, "asking"), 0);
    p.ok = p.room_name[0] || p.host[0] || p.n_players > 0;
    return p.ok;
}

bool parse_room_push(const char *text, std::size_t len, RoomState &st) noexcept
{
    // Every guest socket reads its room push on its own thread (many controllers: one socket per
    // player), so the shared tokens are taken one reader at a time.
    static pthread_mutex_t push_mu = PTHREAD_MUTEX_INITIALIZER;
    struct Hold
    {
        pthread_mutex_t &m;
        explicit Hold(pthread_mutex_t &x) noexcept : m(x)
        {
            pthread_mutex_lock(&m);
        }
        ~Hold()
        {
            pthread_mutex_unlock(&m);
        }
    } hold(push_mu);
    static json::Token toks[384];
    json::Doc d(toks, 384);
    if (!d.parse(text, len) || d.type(0) != json::Type::object)
        return false;
    bool any = false;
    const int r = d.get(0, "room");
    if (d.type(r) == json::Type::object)
    {
        any = true;
        st.known = true;
        if (d.type(d.get(r, "name")) == json::Type::string)
            copy(st.name, sizeof st.name, d, d.get(r, "name"));
        st.size = num_or(d, d.get(r, "size"), st.size);
        const int pl = d.get(r, "players");
        if (d.type(pl) == json::Type::array)
        {
            // keep who was speaking (the voice message says that, not this one)
            RoomState::Player old[8];
            const int n_old = st.n_players;
            for (int i = 0; i < n_old; ++i)
                old[i] = st.players[i];
            st.n_players = 0;
            for (std::uint32_t i = 0; i < d.count(pl) && st.n_players < 8; ++i)
            {
                const int x = d.item(pl, i);
                auto &o = st.players[st.n_players++];
                o = RoomState::Player{};
                o.id = num_or(d, d.get(x, "id"), -1);
                copy(o.name, sizeof o.name, d, d.get(x, "name"));
                o.seat = num_or(d, d.get(x, "seat"), num_or(d, d.get(x, "player"), 0));
                o.you = d.boolean(d.get(x, "you"));
                o.muted = d.boolean(d.get(x, "muted"));
                const int c = d.get(x, "color");
                if (d.count(c) >= 3)
                    o.color = static_cast<std::uint32_t>(num_or(d, d.item(c, 0), 0) & 255) << 16 |
                              static_cast<std::uint32_t>(num_or(d, d.item(c, 1), 0) & 255) << 8 |
                              static_cast<std::uint32_t>(num_or(d, d.item(c, 2), 0) & 255);
                for (int k = 0; k < n_old; ++k)
                    if (old[k].id >= 0 && old[k].id == o.id)
                        o.speaking = old[k].speaking;
            }
        }
        const int you = d.get(r, "you");
        if (d.type(you) == json::Type::object)
        {
            st.you_rtt = num_or(d, d.get(you, "rtt_ms"), -1);
            copy(st.you_tier, sizeof st.you_tier, d, d.get(you, "tier"));
        }
        const int link = d.get(r, "host_link");
        if (d.type(link) == json::Type::object)
        {
            st.link_rtt = num_or(d, d.get(link, "rtt_ms"), -1);
            copy(st.link_tier, sizeof st.link_tier, d, d.get(link, "tier"));
        }
        if (d.type(d.get(r, "chat")) == json::Type::string)
            copy(st.chat, sizeof st.chat, d, d.get(r, "chat"));
        if (d.type(d.get(r, "voice")) == json::Type::string)
            copy(st.voice, sizeof st.voice, d, d.get(r, "voice"));
        const int m = d.get(r, "muted");
        if (d.type(m) == json::Type::object)
        {
            st.muted_voice = d.boolean(d.get(m, "voice"));
            st.muted_chat = d.boolean(d.get(m, "chat"));
        }
        else if (d.type(m) == json::Type::boolean)
            st.muted_voice = st.muted_chat = d.boolean(m);
        // The ready check as everyone sees it ({left_s, asked: [ids], ready: [ids], all} or null).
        const int rd = d.get(r, "ready");
        if (d.type(rd) == json::Type::object)
        {
            int me = -1;
            for (int i = 0; i < st.n_players; ++i)
                if (st.players[i].you)
                    me = st.players[i].id;
            auto has = [&](int arr) {
                for (std::uint32_t i = 0; i < d.count(arr); ++i)
                    if (me >= 0 && num_or(d, d.item(arr, i), -2) == me)
                        return true;
                return false;
            };
            st.ready_left_s = num_or(d, d.get(rd, "left_s"), 0);
            st.ready_all = d.boolean(d.get(rd, "all"));
            if (me >= 0)
            {
                st.ready_asked = has(d.get(rd, "asked"));
                st.ready_said = has(d.get(rd, "ready"));
            }
        }
        else if (rd >= 0)
        {
            st.ready_left_s = -1;
            st.ready_all = st.ready_asked = st.ready_said = false;
        }
    }
    // The host asks "Ready?" ({"ready": {"left_s"}}): this player is asked, not ready yet.
    const int ra = d.get(0, "ready");
    if (d.type(ra) == json::Type::object && r < 0)
    {
        any = true;
        st.ready_left_s = num_or(d, d.get(ra, "left_s"), 30);
        st.ready_all = st.ready_said = false;
        st.ready_asked = true;
        ++st.ready_rev;
    }
    const int cm = d.get(0, "comms");
    if (d.type(cm) == json::Type::object)
    {
        any = true;
        st.comms_known = true;
        st.notice_ver = num_or(d, d.get(cm, "notice"), st.notice_ver);
        st.accepted = d.boolean(d.get(cm, "accepted"), st.accepted);
        if (d.type(d.get(cm, "chat")) == json::Type::string)
            copy(st.chat, sizeof st.chat, d, d.get(cm, "chat"));
        if (d.type(d.get(cm, "voice")) == json::Type::string)
            copy(st.voice, sizeof st.voice, d, d.get(cm, "voice"));
        const int caps = d.get(cm, "caps");
        if (d.type(caps) == json::Type::array)
        {
            st.cap_chat = has_cap(d, caps, "chat");
            st.cap_voice = has_cap(d, caps, "voice");
        }
    }
    const int rf = d.get(0, "comms_refused");
    if (d.type(rf) == json::Type::object)
    {
        any = true;
        copy(st.refused_what, sizeof st.refused_what, d, d.get(rf, "what"));
        copy(st.refused_why, sizeof st.refused_why, d, d.get(rf, "why"));
        st.retry_ms = num_or(d, d.get(rf, "retry_ms"), 0);
        if (d.type(d.get(rf, "notice")) == json::Type::number)
            st.notice_ver = num_or(d, d.get(rf, "notice"), st.notice_ver);
        ++st.refused_rev;
    }
    const int v = d.get(0, "voice");
    if (d.type(v) == json::Type::object)
    {
        any = true;
        st.you_sid = num_or(d, d.get(v, "you"), st.you_sid);
        if (d.type(d.get(v, "mic")) == json::Type::string)
            st.mic_on = d.str_is(d.get(v, "mic"), "on");
        st.ptt = d.boolean(d.get(v, "ptt"), st.ptt);
        st.stepped = d.boolean(d.get(v, "stepped"), st.stepped);
        st.kbps = num_or(d, d.get(v, "kbps"), st.kbps);
        st.frame_ms = num_or(d, d.get(v, "frame_ms"), st.frame_ms);
        const int pp = d.get(v, "people");
        if (d.type(pp) == json::Type::array)
        {
            st.n_people = 0;
            for (std::uint32_t i = 0; i < d.count(pp) && st.n_people < 8; ++i)
            {
                const int x = d.item(pp, i);
                auto &o = st.people[st.n_people++];
                o = RoomState::Person{};
                o.sid = num_or(d, d.get(x, "sid"), -1);
                copy(o.name, sizeof o.name, d, d.get(x, "name"));
                o.player = num_or(d, d.get(x, "player"), 0);
                o.host = d.boolean(d.get(x, "host"));
                o.muted = d.boolean(d.get(x, "muted"));
                o.paused = d.type(d.get(x, "paused")) == json::Type::string || d.boolean(d.get(x, "paused"));
                o.mic = d.str_is(d.get(x, "mic"), "on") || d.boolean(d.get(x, "mic"));
            }
        }
        const int spk = d.get(v, "speaking");
        if (d.type(spk) == json::Type::array)
            for (int i = 0; i < st.n_people; ++i)
            {
                bool on = false;
                for (std::uint32_t k = 0; k < d.count(spk) && !on; ++k)
                    on = num_or(d, d.item(spk, k), -2) == st.people[i].sid;
                st.people[i].speaking = on;
            }
        const int sp = d.get(v, "speaking");
        if (d.type(sp) == json::Type::array)
            for (int i = 0; i < st.n_players; ++i)
            {
                bool on = false;
                for (std::uint32_t k = 0; k < d.count(sp) && !on; ++k)
                    on = st.players[i].id >= 0 && num_or(d, d.item(sp, k), -2) == st.players[i].id;
                st.players[i].speaking = on;
            }
        const int pz = d.get(v, "paused");
        if (d.type(pz) == json::Type::string)
            copy(st.paused, sizeof st.paused, d, pz);
        else if (pz >= 0)
            st.paused[0] = 0;
        if (d.type(d.get(v, "mode")) == json::Type::string)
            copy(st.voice, sizeof st.voice, d, d.get(v, "mode"));
    }
    const int mu = d.get(0, "muted");
    if (d.type(mu) == json::Type::object)
    {
        any = true;
        st.muted_voice = d.boolean(d.get(mu, "voice"), st.muted_voice);
        st.muted_chat = d.boolean(d.get(mu, "chat"), st.muted_chat);
    }
    return any;
}

namespace
{
// A code point a name may hold: letters and digits of any script, space, basic punctuation.
bool name_char(std::uint32_t c) noexcept
{
    if (c < 0x20 || c == 0x7f)
        return false;
    if (c < 0x80)
    {
        if ((c >= '0' && c <= '9') || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c == ' ')
            return true;
        return std::strchr("'-_.,!?()&+", static_cast<int>(c)) != nullptr;
    }
    // zero-width, bidi overrides and other format characters
    if ((c >= 0x200b && c <= 0x200f) || (c >= 0x202a && c <= 0x202e) || (c >= 0x2060 && c <= 0x206f) ||
        c == 0xfeff || c == 0x00ad || c == 0xfffd)
        return false;
    // emoji and symbols (pictographs, dingbats) are not names
    if ((c >= 0x2190 && c <= 0x2bff) || (c >= 0x1f000 && c <= 0x1faff) || (c >= 0xe000 && c <= 0xf8ff))
        return false;
    return true;
}
} // namespace

bool clean_name(const char *in, char *out, std::size_t cap) noexcept
{
    if (!cap)
        return false;
    out[0] = 0;
    if (!in)
        return false;
    const std::size_t len = std::strlen(in);
    std::size_t i = 0, o = 0;
    int chars = 0;
    bool space = false;
    // no links or handles in a name
    if (contains(in, "http") || contains(in, "www.") || std::strchr(in, '@') || contains(in, "://"))
        return false;
    while (i < len && chars < 24)
    {
        const std::size_t at = i;
        const std::uint32_t c = json::utf8_next(in, len, &i);
        if (!name_char(c))
            continue;
        if (c == ' ')
        {
            space = o > 0;
            continue;
        }
        const std::size_t n = i - at;
        if (o + n + (space ? 1 : 0) + 1 > cap)
            break;
        if (space)
        {
            out[o++] = ' ';
            ++chars;
            space = false;
        }
        std::memcpy(out + o, in + at, n);
        o += n;
        ++chars;
    }
    out[o] = 0;
    return o > 0;
}

bool password_ok(const char *pw) noexcept
{
    if (!pw)
        return false;
    const std::size_t len = std::strlen(pw);
    std::size_t i = 0;
    int chars = 0;
    while (i < len)
    {
        const std::uint32_t c = json::utf8_next(pw, len, &i);
        if (c < 0x20 || c == 0x7f)
            return false;
        ++chars;
    }
    return chars >= 4 && chars <= 64;
}
} // namespace play::rooms
