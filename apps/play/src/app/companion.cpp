// PS5Upload Play - the host's side: the app channel to PS5Upload.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "companion.hpp"

#include "../core/json.hpp"
#include "log.hpp"

#include <cstdio>
#include <cstring>

namespace play
{
namespace
{
constexpr std::size_t kTokens = 1400;

void copy(char *dst, std::size_t cap, const char *src) noexcept
{
    std::snprintf(dst, cap, "%s", src ? src : "");
}

void jstr(const json::Doc &d, int i, char *out, std::size_t cap) noexcept
{
    if (!d.str(i, out, cap))
        out[0] = 0;
}
} // namespace

bool parse_companion_doc(const char *text, std::size_t len, CompanionDoc &doc) noexcept
{
    static json::Token toks[kTokens]; // one parser at a time: the companion thread's
    json::Doc d(toks, kTokens);
    if (!d.parse(text, len))
        return false;
    const int root = d.get(0, "doc");
    if (root < 0)
        return false;
    const std::uint32_t rev = doc.rev;
    doc = CompanionDoc{};
    doc.rev = rev + 1;
    doc.paired = d.boolean(d.get(root, "paired"));
    const int eng = d.get(root, "engine");
    if (d.type(eng) == json::Type::object)
    {
        jstr(d, d.get(eng, "name"), doc.engine_name, sizeof doc.engine_name);
        jstr(d, d.get(eng, "version"), doc.engine_version, sizeof doc.engine_version);
    }
    else
        jstr(d, d.get(root, "version"), doc.engine_version, sizeof doc.engine_version);
    const int con = d.get(root, "console");
    jstr(d, d.get(con, "name"), doc.console_name, sizeof doc.console_name);
    jstr(d, d.get(con, "addr"), doc.console_addr, sizeof doc.console_addr);
    const int ses = d.get(root, "session");
    doc.playing = d.str_is(d.get(ses, "state"), "playing");
    jstr(d, d.get(ses, "codec"), doc.codec, sizeof doc.codec);
    doc.bitrate_kbps = static_cast<int>(d.integer(d.get(ses, "bitrate_kbps")));
    const int tog = d.get(root, "together");
    doc.together_open = d.boolean(d.get(tog, "open"));
    doc.together_playing = d.boolean(d.get(tog, "playing"));
    doc.max_guests = static_cast<int>(d.integer(d.get(tog, "max_guests"), 3));
    const int inv = d.get(tog, "invite");
    if (d.type(inv) == json::Type::object)
    {
        doc.has_invite = true;
        jstr(d, d.get(inv, "code"), doc.code, sizeof doc.code);
        jstr(d, d.get(inv, "code_display"), doc.code_display, sizeof doc.code_display);
        jstr(d, d.get(inv, "url"), doc.url, sizeof doc.url);
        jstr(d, d.get(inv, "works"), doc.works, sizeof doc.works);
        doc.invite_left_s = static_cast<int>(d.integer(d.get(inv, "expires_in_s")));
    }
    const int pend = d.get(tog, "pending");
    for (std::uint32_t i = 0; i < d.count(pend) && doc.n_asks < 4; ++i)
    {
        const int a = d.item(pend, i);
        auto &x = doc.asks[doc.n_asks++];
        x.id = d.integer(d.get(a, "id"));
        jstr(d, d.get(a, "name"), x.name, sizeof x.name);
        jstr(d, d.get(a, "client_kind"), x.client_kind, sizeof x.client_kind);
        jstr(d, d.get(a, "codec_warning"), x.codec_warning, sizeof x.codec_warning);
        // How they found the room: the engine's word is `came` (`via` was the stand-in's).
        jstr(d, d.get(a, "came") >= 0 ? d.get(a, "came") : d.get(a, "via"), x.via, sizeof x.via);
        jstr(d, d.get(a, "tag"), x.tag, sizeof x.tag);
        x.own = d.boolean(d.get(a, "own"));
        const int pw = d.get(a, "password_ok");
        x.password_ok = d.type(pw) == json::Type::boolean ? (d.boolean(pw) ? 1 : 0) : -1;
        const int rh = d.get(a, "rtt_hint");
        x.rtt_hint = d.type(rh) == json::Type::number ? static_cast<int>(d.integer(rh)) : -1;
        const int gp = d.get(a, "players");
        x.group_size = d.type(gp) == json::Type::array ? static_cast<int>(d.count(gp)) : 0;
    }
    const int gs = d.get(tog, "guests");
    for (std::uint32_t i = 0; i < d.count(gs) && doc.n_guests < 4; ++i)
    {
        const int g = d.item(gs, i);
        auto &x = doc.guests[doc.n_guests++];
        x.id = d.integer(d.get(g, "id"));
        x.player = static_cast<int>(d.integer(d.get(g, "player")));
        jstr(d, d.get(g, "name"), x.name, sizeof x.name);
        jstr(d, d.get(g, "state"), x.state, sizeof x.state);
        const int r = d.get(g, "rtt_ms");
        x.rtt_ms = d.type(r) == json::Type::number ? static_cast<int>(d.integer(r)) : -1;
        jstr(d, d.get(g, "via"), x.via, sizeof x.via);
        jstr(d, d.get(g, "came"), x.came, sizeof x.came);
        jstr(d, d.get(g, "client_kind"), x.client_kind, sizeof x.client_kind);
        jstr(d, d.get(g, "route"), x.route, sizeof x.route);
        jstr(d, d.get(g, "tier"), x.tier, sizeof x.tier);
        jstr(d, d.get(g, "tag"), x.tag, sizeof x.tag);
        x.from_rooms = d.boolean(d.get(g, "from_rooms"));
        const int mu = d.get(g, "muted");
        if (d.type(mu) == json::Type::object)
        {
            x.muted_voice = d.boolean(d.get(mu, "voice"));
            x.muted_chat = d.boolean(d.get(mu, "chat"));
        }
        else
            x.muted_voice = d.boolean(mu);
        x.group = d.integer(d.get(g, "group"));
    }
    // The room (GET /api/join `room`): in the doc beside "together", or inside it.
    int rm = d.get(root, "room");
    if (d.type(rm) != json::Type::object)
        rm = d.get(tog, "room");
    if (d.type(rm) == json::Type::object)
    {
        auto &r = doc.room;
        r.known = true;
        jstr(d, d.get(rm, "name"), r.name, sizeof r.name);
        jstr(d, d.get(rm, "visibility"), r.vis, sizeof r.vis);
        r.size = static_cast<int>(d.integer(d.get(rm, "size")));
        r.password = d.boolean(d.get(rm, "password"));
        const int ap = d.get(rm, "approval");
        r.approve_friends = d.boolean(d.get(ap, "friends"), true);
        r.approve_password = d.boolean(d.get(ap, "password"));
        r.locked = d.boolean(d.get(rm, "lock")) || d.boolean(d.get(rm, "locked"));
        jstr(d, d.get(rm, "chat"), r.chat, sizeof r.chat);
        jstr(d, d.get(rm, "voice"), r.voice, sizeof r.voice);
        r.start_muted = d.boolean(d.get(rm, "start_muted"));
        const int li = d.get(rm, "listed");
        jstr(d, d.get(li, "state"), r.listed, sizeof r.listed);
        jstr(d, d.get(li, "why"), r.listed_why, sizeof r.listed_why);
        jstr(d, d.get(rm, "lang"), r.lang, sizeof r.lang);
        jstr(d, d.get(rm, "region"), r.region, sizeof r.region);
        r.nearby = d.boolean(d.get(rm, "nearby"), true);
        const int gm = d.get(rm, "game");
        if (d.type(gm) == json::Type::object)
            jstr(d, d.get(gm, "title"), r.game, sizeof r.game);
        else
            jstr(d, gm, r.game, sizeof r.game);
        const int hl = d.get(rm, "host_link");
        const int lr = d.get(hl, "rtt_ms");
        r.link_rtt = d.type(lr) == json::Type::number ? static_cast<int>(d.integer(lr)) : -1;
        jstr(d, d.get(hl, "tier"), r.link_tier, sizeof r.link_tier);
        const int bl = d.get(rm, "blocked");
        for (std::uint32_t i = 0; i < d.count(bl) && r.n_blocked < 8; ++i)
        {
            const int b = d.item(bl, i);
            jstr(d, d.get(b, "name"), r.blocked[r.n_blocked].name, sizeof r.blocked[0].name);
            jstr(d, d.get(b, "tag"), r.blocked[r.n_blocked].tag, sizeof r.blocked[0].tag);
            ++r.n_blocked;
        }
        // The ready check (or null): {left_s, asked: [ids], ready: [ids], all}; each guest's ✓.
        const int rd = d.get(rm, "ready");
        if (d.type(rd) == json::Type::object)
        {
            r.ready_left_s = static_cast<int>(d.integer(d.get(rd, "left_s")));
            r.ready_asked = static_cast<int>(d.count(d.get(rd, "asked")));
            r.ready_all = d.boolean(d.get(rd, "all"));
            const int ids = d.get(rd, "ready");
            r.ready_n = static_cast<int>(d.count(ids));
            for (std::uint32_t i = 0; i < d.count(ids); ++i)
                for (int k = 0; k < doc.n_guests; ++k)
                    if (doc.guests[k].id == d.integer(d.item(ids, i)))
                        doc.guests[k].ready = true;
        }
    }
    jstr(d, d.get(ses, "user"), doc.stream_user, sizeof doc.stream_user);
    jstr(d, d.get(con, "owner"), doc.console_owner, sizeof doc.console_owner);
    const int st = d.get(tog, "settings");
    doc.block_system_buttons = d.boolean(d.get(st, "block_system_buttons"), true);
    doc.max_players = static_cast<int>(d.integer(d.get(st, "max_players"), 3));
    doc.stream_seat = d.boolean(d.get(st, "stream_seat"));
    jstr(d, d.get(st, "invites"), doc.invites, sizeof doc.invites);
    doc.settings_rev = d.integer(d.get(st, "rev"));
    static const char *const kRp[] = {"remote_play", "paired"};
    static const char *const kHelper[] = {"helper", "present"};
    doc.rp_paired = d.boolean(d.path(root, kRp, 2));
    const int hp = d.path(root, kHelper, 2);
    doc.helper_present = d.type(hp) == json::Type::boolean ? (d.boolean(hp) ? 1 : 0) : -1;
    const int ka = d.get(root, "keep_awake");
    if (d.type(ka) == json::Type::string)
        jstr(d, ka, doc.keep_awake, sizeof doc.keep_awake);
    return true;
}

Companion::Companion(std::uint8_t *msg_buf, std::size_t msg_cap) noexcept : ws_{msg_buf, msg_cap}
{
    pthread_mutex_init(&lock_, nullptr);
}

bool Companion::start(const char *addr, std::uint16_t port, const char *token, const char *version,
                      TokenFn fn, void *ctx) noexcept
{
    if (running_.load())
        return false;
    if (thread_)
    {
        pthread_join(thread_, nullptr);
        thread_ = {};
    }
    copy(addr_, sizeof addr_, addr);
    port_ = port;
    copy(token_, sizeof token_, token);
    copy(version_, sizeof version_, version);
    token_fn_ = fn;
    ctx_ = ctx;
    stop_ = false;
    pthread_mutex_lock(&lock_);
    view_ = CompanionView{};
    pthread_mutex_unlock(&lock_);
    running_ = true;
    if (pthread_create(&thread_, nullptr, entry, this) != 0)
    {
        running_ = false;
        return false;
    }
    return true;
}

void Companion::stop() noexcept
{
    stop_ = true;
    if (thread_)
    {
        pthread_join(thread_, nullptr);
        thread_ = {};
    }
}

CompanionView Companion::view() noexcept
{
    pthread_mutex_lock(&lock_);
    CompanionView v = view_;
    pthread_mutex_unlock(&lock_);
    if (v.phase == CompanionView::pairing && v.pair_until_us)
    {
        const std::uint64_t now = net::now_us();
        v.pair_left_s = now < v.pair_until_us ? static_cast<int>((v.pair_until_us - now) / 1000000) : 0;
    }
    return v;
}

void *Companion::entry(void *self) noexcept
{
    static_cast<Companion *>(self)->run();
    return nullptr;
}

void Companion::run() noexcept
{
    int fails = 0;
    while (!stop_.load())
    {
        pthread_mutex_lock(&lock_);
        if (view_.phase != CompanionView::retrying)
            view_.phase = CompanionView::connecting;
        pthread_mutex_unlock(&lock_);
        int status = 0;
        if (!ws_.open(addr_, port_, "/api/play/app", 4000, &status))
        {
            log("companion: connect %s:%u: %s (http %d)", addr_, static_cast<unsigned>(port_), ws_.error(), status);
            pthread_mutex_lock(&lock_);
            // An engine without the app channel answers the upgrade with 404.
            const bool old = status == 404;
            if (old || ++fails >= 3)
            {
                view_.phase = CompanionView::failed;
                copy(view_.why, sizeof view_.why, old ? "old_engine" : status == 429 ? "busy" : "unreachable");
            }
            else
                view_.phase = CompanionView::retrying;
            pthread_mutex_unlock(&lock_);
            for (int i = 0; i < 20 && !stop_.load(); ++i)
                net::sleep_ms(100);
            continue;
        }
        fails = 0;
        char hello[400];
        json::Writer w(hello, sizeof hello);
        w.raw("{\"hello\":{");
        if (token_[0])
            w.raw("\"token\":").str(token_).raw(",");
        else if (grant_[0])
            w.raw("\"grant\":").str(grant_).raw(",");
        if (app_id_[0])
            w.raw("\"app_id\":").str(app_id_).raw(",");
        w.raw("\"v\":").str(version_).raw("}}");
        ws_.send_text(hello);
        app_state(true);
        std::uint64_t last_ping = net::now_us();
        while (!stop_.load())
        {
            const auto got = ws_.next(200);
            if (got == net::WsClient::Got::closed)
                break;
            if (got == net::WsClient::Got::message && ws_.op() == ws::kText)
                handle(reinterpret_cast<const char *>(ws_.data()), ws_.size());
            const std::uint64_t now = net::now_us();
            if (now - last_ping > 10000000)
            {
                last_ping = now;
                char m[64];
                std::snprintf(m, sizeof m, "{\"ping\":%llu}", static_cast<unsigned long long>(now / 1000));
                ws_.send_text(m);
            }
        }
        if (stop_.load())
        {
            app_state(false);
            net::sleep_ms(30);
            break;
        }
        pthread_mutex_lock(&lock_);
        view_.phase = CompanionView::retrying;
        pthread_mutex_unlock(&lock_);
        for (int i = 0; i < 20 && !stop_.load(); ++i)
            net::sleep_ms(100);
    }
    ws_.close();
    running_ = false;
}

void Companion::handle(const char *t, std::size_t n) noexcept
{
    json::Token toks[48];
    json::Doc d(toks, 48);
    // Small messages first; the doc (large) has its own parser.
    if (n > 3 && std::strncmp(t, "{\"doc\"", 6) == 0)
    {
        CompanionDoc doc;
        pthread_mutex_lock(&lock_);
        doc.rev = view_.doc.rev;
        pthread_mutex_unlock(&lock_);
        if (!parse_companion_doc(t, n, doc))
        {
            log("companion: unread doc (%zu bytes): %.*s", n, static_cast<int>(n > 160 ? 160 : n), t);
            return;
        }
        pthread_mutex_lock(&lock_);
        view_.doc = doc;
        if (doc.paired)
            view_.phase = CompanionView::paired;
        else if (view_.phase != CompanionView::pairing)
            view_.phase = CompanionView::unpaired;
        pthread_mutex_unlock(&lock_);
        return;
    }
    if (!d.parse(t, n) || d.type(0) != json::Type::object)
        return;
    int v;
    if ((v = d.get(0, "pairing")) >= 0)
    {
        pthread_mutex_lock(&lock_);
        jstr(d, d.get(v, "code"), view_.pair_code, sizeof view_.pair_code);
        jstr(d, d.get(v, "code_display"), view_.pair_display, sizeof view_.pair_display);
        const long long s = d.integer(d.get(v, "expires_in_s"), 300);
        view_.pair_until_us = net::now_us() + static_cast<std::uint64_t>(s) * 1000000ull;
        view_.phase = CompanionView::pairing;
        pthread_mutex_unlock(&lock_);
        log("companion: pairing code shown (%lld s)", s);
    }
    else if ((v = d.get(0, "paired")) >= 0)
    {
        char tok[80];
        jstr(d, d.get(v, "token"), tok, sizeof tok);
        if (tok[0])
        {
            copy(token_, sizeof token_, tok);
            if (token_fn_)
                token_fn_(addr_, port_, tok, ctx_);
            log("companion: paired");
        }
    }
    else if ((v = d.get(0, "auth")) >= 0)
    {
        pthread_mutex_lock(&lock_);
        jstr(d, v, view_.auth, sizeof view_.auth);
        view_.phase = CompanionView::unpaired;
        pthread_mutex_unlock(&lock_);
        log("companion: token not accepted (%s)", view_.auth);
        token_[0] = 0;
        if (token_fn_)
            token_fn_(addr_, port_, "", ctx_);
    }
    else if ((v = d.get(0, "done")) >= 0)
    {
        if (!d.boolean(d.get(0, "ok"), true))
        {
            pthread_mutex_lock(&lock_);
            jstr(d, d.get(0, "error"), view_.last_error, sizeof view_.last_error);
            view_.last_error_us = net::now_us();
            pthread_mutex_unlock(&lock_);
            log("companion: PS5Upload said no: %s", view_.last_error);
        }
    }
    else if ((v = d.get(0, "error")) >= 0)
    {
        pthread_mutex_lock(&lock_);
        jstr(d, v, view_.last_error, sizeof view_.last_error);
        view_.last_error_us = net::now_us();
        pthread_mutex_unlock(&lock_);
        log("companion: PS5Upload said no: %s", view_.last_error);
    }
}

bool Companion::intent(const char *body) noexcept
{
    if (!ws_.is_open())
        return false;
    pthread_mutex_lock(&lock_);
    view_.last_error[0] = 0;
    pthread_mutex_unlock(&lock_);
    char m[512];
    std::snprintf(m, sizeof m, "{%s,\"req\":%u}", body, req_.fetch_add(1));
    return ws_.send_text(m);
}

void Companion::set_hello(const char *grant, const char *app_id) noexcept
{
    copy(grant_, sizeof grant_, grant);
    copy(app_id_, sizeof app_id_, app_id);
}

bool Companion::pair() noexcept
{
    return intent("\"pair\":{}");
}

bool Companion::decide(long long ask, bool allow) noexcept
{
    char b[96];
    std::snprintf(b, sizeof b, "\"decide\":{\"ask\":%lld,\"allow\":%s}", ask, allow ? "true" : "false");
    return intent(b);
}

bool Companion::kick(int player) noexcept
{
    char b[64];
    std::snprintf(b, sizeof b, "\"kick\":{\"player\":%d}", player);
    return intent(b);
}

bool Companion::invite(bool fresh) noexcept
{
    return intent(fresh ? "\"invite\":{\"new\":true}" : "\"invite\":{}");
}

bool Companion::close_together() noexcept
{
    return intent("\"close\":{}");
}

bool Companion::set_switch(const char *key, const char *json_value) noexcept
{
    char b[160];
    pthread_mutex_lock(&lock_);
    const long long rev = view_.doc.settings_rev;
    pthread_mutex_unlock(&lock_);
    std::snprintf(b, sizeof b, "\"settings\":{\"rev\":%lld,\"set\":{\"%s\":%s}}", rev, key, json_value);
    return intent(b);
}

bool Companion::room_set(const char *key, const char *json_value) noexcept
{
    char b[400];
    json::Writer w(b, sizeof b);
    w.raw("\"room\":{").str(key).raw(":").raw(json_value).raw("}");
    return w.ok() && intent(b);
}

bool Companion::kick_block(int player, const char *block) noexcept
{
    char b[96];
    if (block && *block)
        std::snprintf(b, sizeof b, "\"kick\":{\"player\":%d,\"block\":\"%s\"}", player, block);
    else
        std::snprintf(b, sizeof b, "\"kick\":{\"player\":%d}", player);
    return intent(b);
}

bool Companion::unblock(const char *tag) noexcept
{
    char b[96];
    json::Writer w(b, sizeof b);
    w.raw("\"unblock\":{\"tag\":").str(tag).raw("}");
    return w.ok() && intent(b);
}

bool Companion::mute(int player, bool voice, bool chat) noexcept
{
    char b[96];
    std::snprintf(b, sizeof b, "\"mute\":{\"player\":%d,\"voice\":%s,\"chat\":%s}", player, voice ? "true" : "false",
                  chat ? "true" : "false");
    return intent(b);
}

bool Companion::mute_all(bool on) noexcept
{
    return intent(on ? "\"mute\":{\"all\":true,\"voice\":true}" : "\"mute\":{\"all\":true,\"voice\":false}");
}

bool Companion::allow_group(long long ask) noexcept
{
    char b[96];
    std::snprintf(b, sizeof b, "\"decide\":{\"ask\":%lld,\"allow\":\"group\"}", ask);
    return intent(b);
}

bool Companion::its_me(long long ask) noexcept
{
    char b[96];
    std::snprintf(b, sizeof b, "\"decide\":{\"ask\":%lld,\"allow\":\"me\"}", ask);
    return intent(b);
}

bool Companion::ready_check() noexcept
{
    return intent("\"ready\":{\"start\":true}");
}

bool Companion::chat_quick(int id, int notice) noexcept
{
    char b[80];
    // `notice`: the rooms notice version accepted on this PS5 (the engine takes the host's line
    // only once it is), 0 when not
    if (notice > 0)
        std::snprintf(b, sizeof b, "\"chat\":{\"quick\":%d,\"notice\":%d}", id, notice);
    else
        std::snprintf(b, sizeof b, "\"chat\":{\"quick\":%d}", id);
    return intent(b);
}

bool Companion::keep_awake(const char *mode) noexcept
{
    char b[80];
    std::snprintf(b, sizeof b, "\"keep_awake\":{\"mode\":\"%s\"}", mode);
    return intent(b);
}

bool Companion::app_state(bool foreground) noexcept
{
    char b[128];
    std::snprintf(b, sizeof b, "\"app\":{\"open\":true,\"foreground\":%s,\"v\":\"%s\"}",
                  foreground ? "true" : "false", version_);
    return intent(b);
}
} // namespace play
