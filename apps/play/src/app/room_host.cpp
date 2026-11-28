// PS5Upload Play - the host's side on the TV (docs/ROOMS.md, the Play Rooms design's 7.3): three
// tabs (L1/R1) - My room (asks, players with pings, the room's settings, the invite), Remote Play
// (this console's readiness, turning it on, a pairing PIN) and This PS5 (keep awake, the old
// switches). Rows are kept by identity (a new ask never moves the cursor onto another row), and
// ending or loosening actions are said in words first and only for the stream's user or the
// console's owner (the scenario audit).
// SPDX-License-Identifier: GPL-3.0-or-later
#include "app.hpp"

#include "../../third_party/qrcodegen/qrcodegen.h"
#include "../core/json.hpp"
#include "../core/notice.hpp"
#include "../ui/i18n.hpp"
#include "log.hpp"
#include "platform.hpp"
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

enum : int
{
    G_CROSS = static_cast<int>(Canvas::Glyph::cross),
    G_CIRCLE = static_cast<int>(Canvas::Glyph::circle),
    G_SQUARE = static_cast<int>(Canvas::Glyph::square),
    G_TRIANGLE = static_cast<int>(Canvas::Glyph::triangle),
    G_L1 = static_cast<int>(Canvas::Glyph::l1),
};

int clampi(int v, int lo, int hi) noexcept
{
    return v < lo ? lo : v > hi ? hi : v;
}

const char *src_text(const char *via) noexcept
{
    if (!std::strcmp(via, "rooms"))
        return tr(S::src_rooms);
    if (!std::strcmp(via, "friends"))
        return tr(S::src_friends);
    if (!std::strcmp(via, "nearby"))
        return tr(S::src_nearby);
    if (!std::strcmp(via, "again"))
        return tr(S::src_again);
    if (!std::strcmp(via, "group"))
        return tr(S::src_group);
    return tr(S::src_invite);
}

const char *vis_text(const char *v) noexcept
{
    return tr(!std::strcmp(v, "public") ? S::vis_public : !std::strcmp(v, "friends") ? S::vis_friends : S::vis_private);
}

const char *listed_why(const char *w) noexcept
{
    if (!std::strcmp(w, "internet_off"))
        return tr(S::nl_internet_off);
    if (!std::strcmp(w, "hub"))
        return tr(S::nl_hub);
    if (!std::strcmp(w, "name"))
        return tr(S::nl_name);
    if (!std::strcmp(w, "busy"))
        return tr(S::nl_busy);
    if (!std::strcmp(w, "limit"))
        return tr(S::nl_limit);
    if (!std::strcmp(w, "old_hub"))
        return tr(S::nl_old_hub);
    return w;
}

const CompanionDoc::Guest *guest_of(const CompanionDoc &d, long long id) noexcept
{
    for (int k = 0; k < d.n_guests; ++k)
        if (d.guests[k].id == id)
            return &d.guests[k];
    return nullptr;
}
} // namespace

bool App::owner_here(const CompanionDoc &d) const noexcept
{
    // The stream's PS5 user or the console's owner may end or loosen the room from this TV; when
    // the engine says neither (older engines), anyone here may, as before.
    const char *me = platform::user_name();
    if (!d.stream_user[0] && !d.console_owner[0])
        return true;
    return (d.stream_user[0] && !std::strcmp(me, d.stream_user)) || (d.console_owner[0] && !std::strcmp(me, d.console_owner));
}

void App::host_action_denied(const CompanionDoc &d) noexcept
{
    std::snprintf(note_, sizeof note_, tr(S::owner_only), d.stream_user[0] ? d.stream_user : d.console_owner);
    note_until_ = net::now_us() + 5000000;
    platform::blip(platform::Blip::bad);
}

void App::open_confirm_remove(const CompanionDoc::Guest &g) noexcept
{
    confirm_ = Confirm{};
    confirm_.act = Confirm::remove;
    confirm_.player = g.player;
    confirm_.from_rooms = g.from_rooms;
    confirm_.block_room = g.from_rooms; // checked by default for people from Rooms
    std::snprintf(confirm_.title, sizeof confirm_.title, tr(S::rm_title), g.name[0] ? g.name : tr(S::host_friend));
    go(Screen::confirm);
}

void App::open_confirm_close(const CompanionDoc &d) noexcept
{
    confirm_ = Confirm{};
    confirm_.act = Confirm::close_room;
    if (!d.n_guests)
        std::snprintf(confirm_.title, sizeof confirm_.title, "%s", tr(S::close_q_empty));
    else
    {
        char names[300] = {};
        for (int i = 0; i < d.n_guests; ++i)
        {
            if (i)
                std::strncat(names, i + 1 == d.n_guests ? " & " : ", ", sizeof names - std::strlen(names) - 1);
            std::strncat(names, d.guests[i].name[0] ? d.guests[i].name : tr(S::host_friend), sizeof names - std::strlen(names) - 1);
        }
        std::snprintf(confirm_.title, sizeof confirm_.title, tr(S::close_q), names);
    }
    go(Screen::confirm);
}

void App::fake_confirm_close() noexcept
{
    open_confirm_close(host_view().doc);
    depth_ = 0;
}

int App::host_rows(const CompanionView &v, Row *rows, int cap) const noexcept
{
    int n = 0;
    auto add = [&](Row::Kind k, long long id = 0, int player = 0) {
        if (n < cap)
            rows[n++] = Row{k, id, player};
    };
    if (v.phase == CompanionView::unpaired || v.phase == CompanionView::pairing)
    {
        add(Row::pair);
        return n;
    }
    if (v.phase != CompanionView::paired)
        return 0;
    const CompanionDoc &d = v.doc;
    const HelperState &hs = helper_view_;
    switch (host_tab_)
    {
    case 0: // My room
        for (int i = 0; i < d.n_asks; ++i)
            add(Row::ask, d.asks[i].id);
        if (!d.together_open)
        {
            add(Row::open);
            add(Row::tab_hint);
            break;
        }
        for (int i = 0; i < d.n_guests; ++i)
            add(Row::guest, d.guests[i].id, d.guests[i].player);
        if (d.room.known && d.n_guests)
            add(Row::room_ready);
        if (d.room.known)
        {
            add(Row::room_vis);
            add(Row::room_pw);
            add(Row::sw_max);
            add(Row::room_ap_friends);
            if (d.room.password)
                add(Row::room_ap_password);
            add(Row::room_chat);
            add(Row::room_voice);
            add(Row::room_start_muted);
            if (d.n_guests)
                add(Row::room_mute_all);
            add(Row::room_lock);
            if (d.room.n_blocked)
                add(Row::room_blocked);
            add(Row::room_name);
        }
        else
            add(Row::sw_max);
        add(Row::new_code);
        add(Row::close);
        break;
    case 1: // Remote Play
        if (hs.present == 1 && hs.rp_known && (!hs.rp.service_on || (hs.rp.needs_user_permission && !hs.rp.user_on)))
            add(Row::rp_enable);
        if (hs.present == 1)
            add(Row::rp_pin);
        if (!n)
            add(Row::tab_hint);
        break;
    default: // This PS5
        add(Row::sw_block);
        add(Row::sw_seat);
        add(Row::sw_invites);
        add(Row::sw_awake);
        break;
    }
    return n;
}

void App::input_host(std::uint32_t p) noexcept
{
    if (p & pad::kCircle)
        return back();
    const CompanionView v = host_view();
    refresh_helper_view();
    if (p & (pad::kL1 | pad::kR1))
    {
        host_tab_ = (host_tab_ + ((p & pad::kL1) ? 2 : 1)) % 3;
        host_sel_id_ = -1;
        sel_ = 0;
        platform::blip(platform::Blip::move);
        return;
    }
    Row rows[32];
    const int n = host_rows(v, rows, 32);
    if (v.phase == CompanionView::failed && (p & pad::kCross))
    {
        start_host();
        return;
    }
    if (!n)
        return;
    // the row kept by identity: a new ask above doesn't move the cursor onto something else
    for (int i = 0; i < n; ++i)
        if (rows[i].kind == host_sel_kind_ && rows[i].id == host_sel_id_)
            sel_ = i;
    if (p & kUp)
        sel_ = (sel_ + n - 1) % n, platform::blip(platform::Blip::move);
    if (p & kDown)
        sel_ = (sel_ + 1) % n, platform::blip(platform::Blip::move);
    sel_ = clampi(sel_, 0, n - 1);
    const Row &r = rows[sel_];
    host_sel_kind_ = r.kind;
    host_sel_id_ = r.id;
    const CompanionDoc &d = v.doc;
    const bool cross = p & pad::kCross, left = p & kLeftB, right = p & kRight;
    const bool change = cross || left || right;
    auto ok = [](bool sent) { platform::blip(sent ? platform::Blip::choose : platform::Blip::bad); };
    const bool owner = owner_here(d);
    switch (r.kind)
    {
    case Row::pair:
        if (cross)
            ok(host_->pair());
        break;
    case Row::rp_enable:
        if (cross)
        {
            // A change to the console's settings: a second press says yes.
            const std::uint64_t now = net::now_us();
            if (now > rp_confirm_until_)
            {
                rp_confirm_until_ = now + 5000000;
                platform::blip(platform::Blip::choose);
                break;
            }
            rp_confirm_until_ = 0;
            helper::Rp rp;
            char err[32] = {};
            const bool done = helper::rp_enable(rp, err, sizeof err);
            log("helper: turn on Remote Play: %s", done ? "done" : err);
            pthread_mutex_lock(&helper_lock_);
            if (done)
                helper_.rp = rp, helper_.rp_known = true, helper_.error[0] = 0;
            else
                std::snprintf(helper_.error, sizeof helper_.error, tr(S::host_rp_enable_failed), err);
            pthread_mutex_unlock(&helper_lock_);
            ok(done);
        }
        break;
    case Row::rp_pin:
        if (cross)
        {
            helper::Pin pin;
            char err[32] = {};
            bool done;
            if (!std::strcmp(helper_view_.pin.state, "waiting"))
            {
                done = helper::pin_cancel();
                std::snprintf(pin.state, sizeof pin.state, "%s", "idle");
            }
            else
                done = helper::pin_request(pin, err, sizeof err);
            log("helper: Remote Play PIN %s: %s", pin.state, done ? "ok" : err);
            pthread_mutex_lock(&helper_lock_);
            if (done)
                helper_.pin = pin, helper_.error[0] = 0;
            else
                std::snprintf(helper_.error, sizeof helper_.error, tr(S::host_rp_pin_failed), err);
            pthread_mutex_unlock(&helper_lock_);
            ok(done);
        }
        break;
    case Row::ask:
        if (cross)
            ok(host_->decide(r.id, true));
        if (p & pad::kSquare)
            ok(host_->decide(r.id, false));
        if (p & pad::kTriangle)
            for (int k = 0; k < d.n_asks; ++k)
                if (d.asks[k].id == r.id)
                {
                    // the host's own device: "It's me" puts it on Player 1 (the stream's controller)
                    if (d.asks[k].own)
                        ok(host_->its_me(r.id));
                    else if (d.asks[k].group_size > 1)
                        ok(host_->allow_group(r.id));
                }
        break;
    case Row::open:
        if (cross)
            ok(host_->invite(false));
        break;
    case Row::new_code:
        if (cross)
            ok(host_->invite(true));
        break;
    case Row::close:
        if (cross)
        {
            if (!owner)
                return host_action_denied(d);
            platform::blip(platform::Blip::choose);
            open_confirm_close(d);
        }
        break;
    case Row::guest:
        if (p & (pad::kTriangle | pad::kCross))
        {
            if (const CompanionDoc::Guest *g = guest_of(d, r.id))
            {
                menu_player_ = g->player;
                menu_guest_ = g->id;
                menu_blocked_ = false;
                platform::blip(platform::Blip::choose);
                go(Screen::menu);
            }
        }
        break;
    case Row::room_vis:
        if (change)
        {
            static const char *order[] = {"private", "friends", "public"};
            int at = !std::strcmp(d.room.vis, "public") ? 2 : !std::strcmp(d.room.vis, "friends") ? 1 : 0;
            const int to = (at + (left ? 2 : 1)) % 3;
            // wider is loosening: the owner only; Public also needs the notice and one confirm
            if (to > at && !owner)
                return host_action_denied(d);
            if (to == 2)
            {
                if (!need_notice(static_cast<int>(notice::Need::public_room), Pending::make_public))
                    return;
                if (!settings_.public_ok)
                {
                    confirm_ = Confirm{};
                    confirm_.act = Confirm::make_public;
                    std::snprintf(confirm_.title, sizeof confirm_.title, "%s", tr(S::public_first));
                    go(Screen::confirm);
                    return;
                }
            }
            char js[24];
            std::snprintf(js, sizeof js, "\"%s\"", order[to]);
            ok(host_->room_set("visibility", js));
            std::snprintf(note_, sizeof note_, "%s", tr(S::vis_note));
            note_until_ = net::now_us() + 4000000;
        }
        break;
    case Row::room_pw:
        if (cross)
        {
            if (!d.room.password)
                open_keyboard(Kb::room_pw, tr(S::kb_title_newpw), "", 64, true);
            else if (!owner)
                return host_action_denied(d);
            else
            {
                confirm_ = Confirm{};
                confirm_.act = Confirm::remove_pw;
                std::snprintf(confirm_.title, sizeof confirm_.title, "%s", tr(S::remove_pw_q));
                go(Screen::confirm);
            }
        }
        break;
    case Row::sw_max:
        if (change)
        {
            // Room size 2-4 counts you; the engine keeps max_players (friends) as size - 1.
            int m = d.max_players + (left ? -1 : 1);
            if (m < 1)
                m = d.max_guests;
            if (m > d.max_guests)
                m = 1;
            if (m < d.n_guests)
            {
                std::snprintf(note_, sizeof note_, tr(S::size_below), d.n_guests + 1);
                note_until_ = net::now_us() + 4000000;
                platform::blip(platform::Blip::bad);
                break;
            }
            char b[8];
            std::snprintf(b, sizeof b, "%d", m);
            ok(host_->set_switch("max_players", b));
        }
        break;
    case Row::room_ap_friends:
        if (change)
        {
            if (!d.room.approve_friends && !owner)
                return host_action_denied(d);
            ok(host_->room_set("approval", d.room.approve_friends ? "{\"friends\":false}" : "{\"friends\":true}"));
        }
        break;
    case Row::room_ap_password:
        if (change)
        {
            if (!d.room.approve_password && !owner)
                return host_action_denied(d);
            ok(host_->room_set("approval", d.room.approve_password ? "{\"password\":false}" : "{\"password\":true}"));
        }
        break;
    case Row::room_chat:
        if (change)
        {
            static const char *modes[] = {"off", "quick", "on"};
            int at = !std::strcmp(d.room.chat, "on") ? 2 : !std::strcmp(d.room.chat, "quick") ? 1 : 0;
            at = (at + (left ? 2 : 1)) % 3;
            if (at && !need_notice(static_cast<int>(notice::Need::chat), Pending::chat_on))
                return;
            char js[16];
            std::snprintf(js, sizeof js, "\"%s\"", modes[at]);
            ok(host_->room_set("chat", js));
        }
        break;
    case Row::room_voice:
        if (change)
        {
            static const char *modes[] = {"off", "ptt", "open"};
            int at = !std::strcmp(d.room.voice, "open") ? 2 : !std::strcmp(d.room.voice, "ptt") ? 1 : 0;
            at = (at + (left ? 2 : 1)) % 3;
            if (at && !need_notice(static_cast<int>(notice::Need::voice), Pending::chat_on))
                return;
            char js[16];
            std::snprintf(js, sizeof js, "\"%s\"", modes[at]);
            ok(host_->room_set("voice", js));
        }
        break;
    case Row::room_start_muted:
        if (change)
            ok(host_->room_set("start_muted", d.room.start_muted ? "false" : "true"));
        break;
    case Row::room_mute_all:
        if (cross)
        {
            bool any_unmuted = false;
            for (int k = 0; k < d.n_guests; ++k)
                any_unmuted = any_unmuted || !d.guests[k].muted_voice;
            ok(host_->mute_all(any_unmuted));
        }
        break;
    case Row::room_lock:
        if (cross)
        {
            if (d.room.locked && !owner)
                return host_action_denied(d);
            ok(host_->room_set("lock", d.room.locked ? "false" : "true"));
        }
        break;
    case Row::room_blocked:
        if (cross)
        {
            menu_blocked_ = true;
            platform::blip(platform::Blip::choose);
            go(Screen::menu);
        }
        break;
    case Row::room_name:
        if (cross)
            open_keyboard(Kb::room_name, tr(S::kb_title_room), d.room.name, 32, false);
        break;
    case Row::room_ready:
        if (cross)
        {
            // one at a time, at most one a minute (the engine says why it didn't start one)
            if (d.room.ready_left_s >= 0)
            {
                platform::blip(platform::Blip::bad);
                break;
            }
            ok(host_->ready_check());
        }
        break;
    case Row::sw_block:
        if (change)
        {
            if (d.block_system_buttons && !owner)
                return host_action_denied(d);
            ok(host_->set_switch("block_system_buttons", d.block_system_buttons ? "false" : "true"));
        }
        break;
    case Row::sw_seat:
        if (change)
        {
            if (!d.stream_seat && !owner)
                return host_action_denied(d);
            ok(host_->set_switch("stream_seat", d.stream_seat ? "false" : "true"));
        }
        break;
    case Row::sw_invites:
        if (change)
        {
            if (!std::strcmp(d.invites, "wifi") && !owner)
                return host_action_denied(d);
            ok(host_->set_switch("invites", std::strcmp(d.invites, "wifi") ? "\"wifi\"" : "\"anywhere\""));
        }
        break;
    case Row::sw_awake:
        if (change)
        {
            static const char *modes[] = {"off", "transfers", "always"};
            int at = 0;
            for (int i = 0; i < 3; ++i)
                if (!std::strcmp(d.keep_awake, modes[i]))
                    at = i;
            at = (at + (left ? 2 : 1)) % 3;
            ok(host_->keep_awake(modes[at]));
        }
        break;
    case Row::tab_hint: break;
    }
}

// ---- the player menu and the blocked list ----------------------------------------------------
namespace
{
// One row of a player's menu: mute voice, mute chat, seats (give / take back Player 1, move), remove.
struct MenuItem
{
    enum Act : std::uint8_t
    {
        mute_voice,
        mute_chat,
        give_p1,
        take_back,
        move,
        remove,
    } act;
    int to = 0;
};

int menu_items(const CompanionDoc &d, const CompanionDoc::Guest &g, MenuItem *out, int cap) noexcept
{
    int n = 0;
    auto add = [&](MenuItem::Act a, int to = 0) {
        if (n < cap)
            out[n++] = MenuItem{a, to};
    };
    add(MenuItem::mute_voice);
    add(MenuItem::mute_chat);
    if (g.player == 1)
        add(MenuItem::take_back);
    else
    {
        // Player 1 is the stream's controller (the host's user): never for people from Rooms,
        // and only where "A friend on the stream's controller" is on (the engine says why not).
        bool p1_taken = false;
        for (int k = 0; k < d.n_guests; ++k)
            p1_taken = p1_taken || d.guests[k].player == 1;
        if (!g.from_rooms && d.stream_seat && !p1_taken)
            add(MenuItem::give_p1);
        // the free guest seats (Players 2 to the room's size)
        for (int to = 2; to <= d.max_players + 1 && to <= 4; ++to)
        {
            bool taken = false;
            for (int k = 0; k < d.n_guests; ++k)
                taken = taken || d.guests[k].player == to;
            if (!taken)
                add(MenuItem::move, to);
        }
    }
    add(MenuItem::remove);
    return n;
}
} // namespace

void App::input_menu(std::uint32_t p) noexcept
{
    const CompanionDoc d = host_view().doc;
    if (p & pad::kCircle)
        return back();
    MenuItem items[8];
    const CompanionDoc::Guest *mg = menu_blocked_ ? nullptr : guest_of(d, menu_guest_);
    if (!menu_blocked_ && !mg)
        return back(); // they left meanwhile
    const int n = menu_blocked_ ? d.room.n_blocked : menu_items(d, *mg, items, 8);
    if (!n)
        return back();
    if (p & kUp)
        sel_ = (sel_ + n - 1) % n, platform::blip(platform::Blip::move);
    if (p & kDown)
        sel_ = (sel_ + 1) % n, platform::blip(platform::Blip::move);
    sel_ = clampi(sel_, 0, n - 1);
    if (!(p & pad::kCross))
        return;
    auto ok = [](bool sent) { platform::blip(sent ? platform::Blip::choose : platform::Blip::bad); };
    if (menu_blocked_)
    {
        ok(host_->unblock(d.room.blocked[sel_].tag));
        return;
    }
    const CompanionDoc::Guest *g = mg;
    switch (items[sel_].act)
    {
    case MenuItem::mute_voice: ok(host_->mute(g->player, !g->muted_voice, g->muted_chat)); break;
    case MenuItem::mute_chat: ok(host_->mute(g->player, g->muted_voice, !g->muted_chat)); break;
    case MenuItem::give_p1:
        // a friend on the host's user (saves, trophies): the owner only
        if (!owner_here(d))
            return host_action_denied(d);
        ok(host_->seat(g->player, 1));
        back();
        break;
    case MenuItem::take_back:
        ok(host_->seat(g->player, 0));
        back();
        break;
    case MenuItem::move:
        ok(host_->seat(g->player, items[sel_].to));
        back();
        break;
    case MenuItem::remove:
    {
        const CompanionDoc::Guest copy = *g;
        back();
        open_confirm_remove(copy);
        break;
    }
    }
}

void App::draw_menu(Canvas &c) noexcept
{
    const CompanionDoc d = host_view().doc;
    if (menu_blocked_)
    {
        char t[96];
        std::snprintf(t, sizeof t, tr(S::room_blocked), d.room.n_blocked);
        draw_frame(c, t);
        for (int i = 0; i < d.room.n_blocked && i < 8; ++i)
        {
            char r[160], who[64];
            std::snprintf(who, sizeof who, "%s · %s", d.room.blocked[i].name, d.room.blocked[i].tag);
            std::snprintf(r, sizeof r, tr(S::room_unblock), who);
            draw_row(c, kLeft, 230 + i * 100, 1100, 86, sel_ == i, r, nullptr);
        }
    }
    else
    {
        const CompanionDoc::Guest *g = guest_of(d, menu_guest_);
        draw_frame(c, g && g->name[0] ? g->name : tr(S::host_friend));
        if (g)
        {
            MenuItem items[8];
            const int n = menu_items(d, *g, items, 8);
            for (int i = 0; i < n; ++i)
            {
                char t[120];
                switch (items[i].act)
                {
                case MenuItem::mute_voice: std::snprintf(t, sizeof t, "%s", tr(g->muted_voice ? S::pm_unmute_voice : S::pm_mute_voice)); break;
                case MenuItem::mute_chat: std::snprintf(t, sizeof t, "%s", tr(g->muted_chat ? S::pm_unmute_chat : S::pm_mute_chat)); break;
                case MenuItem::give_p1: std::snprintf(t, sizeof t, "%s", tr(S::pm_give_p1)); break;
                case MenuItem::take_back: std::snprintf(t, sizeof t, "%s", tr(S::pm_take_p1)); break;
                case MenuItem::move: std::snprintf(t, sizeof t, tr(S::pm_move_to), items[i].to); break;
                case MenuItem::remove: std::snprintf(t, sizeof t, "%s", tr(S::pm_remove)); break;
                }
                draw_row(c, kLeft, 230 + i * 96, 1100, 84, sel_ == i, t, nullptr);
            }
        }
    }
    const int g[] = {G_CROSS, G_CIRCLE};
    const S l[] = {S::hint_choose, S::hint_back};
    draw_hints(c, g, l, 2);
}

// ---- confirms (ending or loosening, in words first) -------------------------------------------
void App::input_confirm(std::uint32_t p) noexcept
{
    const bool remove = confirm_.act == Confirm::remove;
    const int n = remove ? 4 : 2;
    if (p & pad::kCircle)
        return back();
    if (p & kUp)
        confirm_.sel = (confirm_.sel + n - 1) % n, platform::blip(platform::Blip::move);
    if (p & kDown)
        confirm_.sel = (confirm_.sel + 1) % n, platform::blip(platform::Blip::move);
    if (!(p & pad::kCross))
        return;
    const int go_row = remove ? 2 : 0, cancel_row = remove ? 3 : 1;
    if (remove && confirm_.sel == 0)
    {
        confirm_.block_room = !confirm_.block_room;
        platform::blip(platform::Blip::choose);
        return;
    }
    if (remove && confirm_.sel == 1)
    {
        confirm_.block_always = !confirm_.block_always;
        platform::blip(platform::Blip::choose);
        return;
    }
    if (confirm_.sel == cancel_row)
        return back();
    if (confirm_.sel != go_row)
        return;
    bool sent = false;
    switch (confirm_.act)
    {
    case Confirm::close_room: sent = host_->close_together(); break;
    case Confirm::remove:
        sent = host_->kick_block(confirm_.player, confirm_.block_always ? "always" : confirm_.block_room ? "room" : nullptr);
        break;
    case Confirm::make_public:
        settings_.public_ok = true;
        save();
        sent = host_->room_set("visibility", "\"public\"");
        break;
    case Confirm::remove_pw: sent = host_->room_set("password", "\"\""); break;
    }
    platform::blip(sent ? platform::Blip::choose : platform::Blip::bad);
    back();
}

void App::draw_confirm(Canvas &c) noexcept
{
    draw_frame(c, nullptr);
    int y = 200;
    y += c.text_wrap(Font::title, kLeft, y, 1500, confirm_.title, col::text) + 50;
    const bool remove = confirm_.act == Confirm::remove;
    S go_label = S::room_close, cancel_label = S::keep_open;
    if (remove)
    {
        draw_row(c, kLeft, y, 1100, 90, confirm_.sel == 0, tr(S::rm_block_room), nullptr, confirm_.block_room ? "✓" : "–");
        y += 106;
        draw_row(c, kLeft, y, 1100, 90, confirm_.sel == 1, tr(S::rm_block_always), nullptr, confirm_.block_always ? "✓" : "–");
        y += 126;
        go_label = S::rm_go, cancel_label = S::cancel;
    }
    else if (confirm_.act == Confirm::make_public)
        go_label = S::make_public, cancel_label = S::cancel;
    else if (confirm_.act == Confirm::remove_pw)
        go_label = S::remove_pw_go, cancel_label = S::cancel;
    const int go_row = remove ? 2 : 0;
    draw_row(c, kLeft, y, 800, 96, confirm_.sel == go_row, tr(go_label), nullptr);
    draw_row(c, kLeft, y + 112, 800, 96, confirm_.sel == go_row + 1, tr(cancel_label), nullptr);
    const int g[] = {G_CROSS, G_CIRCLE};
    const S l[] = {S::hint_choose, S::hint_back};
    draw_hints(c, g, l, 2);
}

// ---- drawing the host screen ------------------------------------------------------------------
void App::draw_host(Canvas &c, const CompanionView &v) noexcept
{
    refresh_helper_view();
    draw_frame(c, tr(S::home_host));
    char buf[400];
    const CompanionDoc &d = v.doc;
    // Which computer (or phone, or NAS) runs the room: "Hosted from Alex's PC".
    const char *name = d.engine_name[0] ? d.engine_name : engine_name_[0] ? engine_name_ : addr_;
    std::snprintf(buf, sizeof buf, tr(S::host_from), name);
    c.text(Font::small, kLeft, kTop + 66, buf, col::muted);
    // the tabs, top right: L1 [ My room | Remote Play | This PS5 ] R1 (top left in a right-to-left
    // language, where the title sits on the right)
    {
        const S tabs[] = {S::tab_room, S::tab_rp, S::tab_ps5};
        int tw = 0;
        for (const S t : tabs)
            tw += Canvas::text_width(Font::body, tr(t)) + 48;
        int x = ui::i18n::right_to_left() ? kLeft : ui::kW - 140 - tw - 120;
        x += c.button(Canvas::Glyph::l1, x, kTop + 2, 40) + 12;
        for (int i = 0; i < 3; ++i)
        {
            const int w = Canvas::text_width(Font::body, tr(tabs[i])) + 36;
            if (i == host_tab_)
                c.round_rect(x, kTop - 4, w, 54, 14, col::accent);
            c.text(Font::body, x + 18, kTop + 4, tr(tabs[i]), i == host_tab_ ? col::on_accent : col::muted);
            x += w + 12;
        }
        c.button(Canvas::Glyph::r1, x, kTop + 2, 40);
    }
    Row rows[32];
    const int n = host_rows(v, rows, 32);
    // the selection kept by identity
    for (int i = 0; i < n; ++i)
        if (rows[i].kind == host_sel_kind_ && rows[i].id == host_sel_id_)
            sel_ = i;
    sel_ = n ? clampi(sel_, 0, n - 1) : 0;
    if (v.phase == CompanionView::connecting || v.phase == CompanionView::idle || v.phase == CompanionView::retrying)
    {
        const float t = static_cast<float>(net::now_us() % 1200000) / 1200000.0f;
        c.ring(kLeft + 40, 420, 36, 8, col::accent, t, 0.3f);
        c.text(Font::title, kLeft + 110, 392, tr(v.phase == CompanionView::retrying ? S::host_retrying : S::host_connecting), col::text);
        return;
    }
    if (v.phase == CompanionView::failed)
    {
        if (!std::strcmp(v.why, "old_engine"))
            c.text_wrap(Font::title, kLeft, 300, 1500, tr(S::host_old_engine), col::text);
        else
        {
            std::snprintf(buf, sizeof buf, tr(S::fail_unreachable), addr_);
            c.text_wrap(Font::title, kLeft, 300, 1500, !std::strcmp(v.why, "busy") ? tr(S::fail_busy) : buf, col::text);
        }
        const int g[] = {G_CROSS, G_CIRCLE};
        const S l[] = {S::hint_retry, S::hint_back};
        draw_hints(c, g, l, 2);
        return;
    }
    if (v.phase == CompanionView::unpaired || v.phase == CompanionView::pairing)
    {
        c.text(Font::title, kLeft, 260, tr(S::host_pair_title), col::text);
        int y = 330;
        if (v.auth[0])
            y += c.text_wrap(Font::body, kLeft, y, 1500, tr(S::host_auth_dropped), col::warn) + 10;
        y += c.text_wrap(Font::body, kLeft, y, 1500, tr(S::host_pair_text), col::muted) + 30;
        if (v.phase == CompanionView::pairing && v.pair_left_s > 0)
        {
            c.round_rect(kLeft, y, 760, 200, 24, col::panel, 235);
            c.text_center(Font::huge, kLeft + 380, y + (200 - ui::font_height(Font::huge)) / 2, v.pair_display, col::text);
            y += 230;
            // one click on the computer: it shows the same code with a Pair button (F3)
            std::snprintf(buf, sizeof buf, tr(S::pair_confirm), d.engine_name[0] ? d.engine_name : "PS5Upload");
            y += c.text_wrap(Font::body, kLeft, y, 1500, buf, col::text) + 10;
            y += c.text_wrap(Font::small, kLeft, y, 1500, tr(S::host_pair_code_text), col::muted) + 10;
            std::snprintf(buf, sizeof buf, tr(S::host_pair_left), v.pair_left_s / 60, v.pair_left_s % 60);
            c.text(Font::small, kLeft, y, buf, col::muted);
        }
        else
        {
            if (v.phase == CompanionView::pairing)
                y += c.text_wrap(Font::body, kLeft, y, 1500, tr(S::host_pair_gone), col::warn) + 10;
            draw_row(c, kLeft, y, 800, 104, true, tr(S::host_pair_button), nullptr);
        }
        const int g[] = {G_CROSS, G_CIRCLE};
        const S l[] = {S::hint_choose, S::hint_back};
        draw_hints(c, g, l, 2);
        return;
    }
    if (v.phase != CompanionView::paired)
        return;

    int y = 200;
    const HelperState &hs = helper_view_;
    if (host_tab_ == 0)
    {
        // the room's line: name · visibility · n of size, the listing, your PS5 link
        if (d.together_open && d.room.known)
        {
            std::snprintf(buf, sizeof buf, tr(S::room_head), d.room.name, vis_text(d.room.vis), d.n_guests + 1,
                          d.room.size ? d.room.size : d.max_players + 1);
            if (d.room.game[0])
            {
                const std::size_t l = std::strlen(buf);
                std::snprintf(buf + l, sizeof buf - l, " · %s", d.room.game);
            }
            c.text(Font::body, kLeft, y, buf, col::text, 1100);
            y += 46;
            if (!std::strcmp(d.room.vis, "private"))
                buf[0] = 0;
            else if (!std::strcmp(d.room.listed, "listed"))
                std::snprintf(buf, sizeof buf, "%s", tr(S::listed_ok));
            else if (!std::strcmp(d.room.listed, "listing"))
                std::snprintf(buf, sizeof buf, "%s", tr(S::listed_listing));
            else if (d.room.listed_why[0])
                std::snprintf(buf, sizeof buf, tr(S::listed_not), listed_why(d.room.listed_why));
            else
                buf[0] = 0;
            if (d.room.locked)
                std::snprintf(buf, sizeof buf, "%s", tr(S::room_locked));
            if (buf[0])
            {
                c.text(Font::small, kLeft, y, buf, d.room.locked || d.room.listed_why[0] ? col::warn : col::muted, 1100);
                y += 40;
            }
            if (d.room.link_rtt >= 0)
            {
                const int bw = draw_bars(c, kLeft, y, rooms::bars_of(d.room.link_tier), false);
                std::snprintf(buf, sizeof buf, "%s · %d ms", tr(S::host_link), d.room.link_rtt);
                c.text(Font::small, kLeft + bw + 14, y + 4, buf, col::muted);
                y += 42;
            }
        }
        else if (d.playing)
        {
            std::snprintf(buf, sizeof buf, tr(S::host_playing),
                          !std::strcmp(d.codec, "h264") ? "H.264" : !std::strcmp(d.codec, "h265_hdr") ? "HEVC HDR" : "HEVC",
                          (d.bitrate_kbps + 500) / 1000);
            c.circle(kLeft + 10, y + 18, 9, col::good);
            c.text(Font::small, kLeft + 34, y + 4, buf, col::text, 1060);
            y += 44;
        }
        else
        {
            c.circle(kLeft + 10, y + 18, 9, col::muted);
            c.text(Font::small, kLeft + 34, y + 4, tr(S::host_idle), col::muted, 1060);
            y += 44;
        }
    }
    else if (host_tab_ == 1)
    {
        // this console's Remote Play: what the helper says, then what the engine knows
        if (hs.present == 1 && hs.rp_known && !hs.rp.ready)
        {
            const S why = !hs.rp.service_on                               ? S::host_rp_off_here
                          : hs.rp.needs_user_permission && !hs.rp.user_on ? S::host_rp_user_here
                          : !hs.rp.psn_activated                          ? S::host_rp_account
                                                                          : S::host_rp_setup;
            c.circle(kLeft + 10, y + 18, 9, col::warn);
            c.text(Font::small, kLeft + 34, y + 4, tr(why), col::warn, 1060);
        }
        else
        {
            const bool good = d.rp_paired || (hs.present == 1 && hs.rp_known && hs.rp.ready);
            if (d.rp_paired)
                std::snprintf(buf, sizeof buf, tr(S::host_rp_ready), d.engine_name[0] ? d.engine_name : "PS5Upload");
            else
                std::snprintf(buf, sizeof buf, "%s", tr(good ? S::host_rp_on_here : S::host_rp_setup));
            c.circle(kLeft + 10, y + 18, 9, good ? col::good : col::warn);
            c.text(Font::small, kLeft + 34, y + 4, buf, good ? col::text : col::warn, 1060);
        }
        y += 44;
        const int helper_on = hs.present >= 0 ? hs.present : d.helper_present;
        if (helper_on >= 0)
        {
            c.circle(kLeft + 10, y + 18, 9, helper_on ? col::good : col::warn);
            c.text(Font::small, kLeft + 34, y + 4, tr(helper_on ? S::host_helper_on : S::host_helper_off),
                   helper_on ? col::text : col::warn, 1060);
            y += 44;
        }
        if (hs.error[0])
        {
            c.text(Font::small, kLeft + 34, y + 4, hs.error, col::warn, 1060);
            y += 44;
        }
    }
    y += 10;
    const int row_h = 74, row_w = 1100;
    const int fit = (930 - y) / row_h;
    const int first = sel_ > fit - 1 ? sel_ - (fit - 1) : 0;
    for (int i = first; i < n && i < first + fit; ++i)
    {
        const Row &r = rows[i];
        const bool s = sel_ == i;
        char title[220];
        const char *value = nullptr;
        static char vbuf[96];
        title[0] = 0;
        int bars = -1;
        switch (r.kind)
        {
        case Row::ask:
            for (int k = 0; k < d.n_asks; ++k)
                if (d.asks[k].id == r.id)
                {
                    const auto &a = d.asks[k];
                    const char *who = a.name[0] ? a.name : tr(S::host_friend);
                    if (a.own)
                        std::snprintf(title, sizeof title, tr(S::ask_own), who);
                    else if (a.group_size > 1)
                        std::snprintf(title, sizeof title, tr(S::ask_group), who, a.group_size);
                    else if (a.via[0] || a.tag[0])
                        std::snprintf(title, sizeof title, tr(S::ask_from), who, a.tag[0] ? a.tag : "-", src_text(a.via));
                    else
                        std::snprintf(title, sizeof title, tr(!std::strcmp(a.client_kind, "ps5") ? S::host_asks_ps5 : S::host_asks), who);
                    vbuf[0] = 0;
                    if (a.password_ok == 1)
                        std::snprintf(vbuf, sizeof vbuf, "%s", tr(S::ask_pw_ok));
                    if (a.rtt_hint >= 0)
                    {
                        const std::size_t l = std::strlen(vbuf);
                        std::snprintf(vbuf + l, sizeof vbuf - l, "%s", l ? " · " : "");
                        const std::size_t l2 = std::strlen(vbuf);
                        std::snprintf(vbuf + l2, sizeof vbuf - l2, tr(S::ask_about), a.rtt_hint);
                    }
                    if (vbuf[0])
                        value = vbuf;
                }
            break;
        case Row::open: std::snprintf(title, sizeof title, "%s", tr(d.room.known ? S::host_open_room : S::host_together_open)); break;
        case Row::new_code: std::snprintf(title, sizeof title, "%s", tr(S::host_new_code)); break;
        case Row::close: std::snprintf(title, sizeof title, "%s", tr(d.room.known ? S::room_close : S::host_close)); break;
        case Row::guest:
            if (const CompanionDoc::Guest *g = guest_of(d, r.id))
            {
                const char *gn = g->name[0] ? g->name : tr(S::host_friend);
                int o = std::snprintf(title, sizeof title, "%d · %s", g->player, gn);
                if (g->tag[0])
                    o += std::snprintf(title + o, sizeof title - static_cast<std::size_t>(o), " · %s", g->tag);
                if (g->from_rooms)
                    o += std::snprintf(title + o, sizeof title - static_cast<std::size_t>(o), " · %s", tr(S::pl_from_rooms));
                if (g->muted_voice)
                    o += std::snprintf(title + o, sizeof title - static_cast<std::size_t>(o), " · %s", tr(S::ov_muted));

                bars = g->tier[0] ? rooms::bars_of(g->tier) : g->rtt_ms >= 0 ? rooms::bars_for_ms(g->rtt_ms) : -1;
                vbuf[0] = 0;
                if (g->rtt_ms >= 0)
                {
                    char way[64];
                    route_ui::guest_line(g->route, "", way, sizeof way);
                    std::snprintf(vbuf, sizeof vbuf, "%d ms%s%s", g->rtt_ms, way[0] ? " · " : "", way);
                    value = vbuf;
                }
                // said ready in the ready check shown now: first thing on the right
                if (g->ready && d.room.ready_left_s >= 0)
                {
                    char rv[96];
                    std::snprintf(rv, sizeof rv, "✓ %s%s%s", tr(S::ready_yes), vbuf[0] ? " · " : "", vbuf);
                    std::snprintf(vbuf, sizeof vbuf, "%s", rv);
                    value = vbuf;
                }
            }
            break;
        case Row::room_vis:
            std::snprintf(title, sizeof title, "%s", tr(S::set_vis));
            value = vis_text(d.room.vis);
            break;
        case Row::room_pw:
            std::snprintf(title, sizeof title, "%s", tr(S::set_password));
            value = tr(d.room.password ? S::pw_on : S::pw_none);
            break;
        case Row::sw_max:
            if (d.room.known)
            {
                std::snprintf(title, sizeof title, "%s", tr(S::set_size));
                std::snprintf(vbuf, sizeof vbuf, tr(S::size_val), d.max_players + 1);
            }
            else
            {
                std::snprintf(title, sizeof title, "%s", tr(S::host_sw_max));
                std::snprintf(vbuf, sizeof vbuf, "%d", d.max_players);
            }
            value = vbuf;
            break;
        case Row::room_ap_friends:
            std::snprintf(title, sizeof title, "%s", tr(S::set_ap_friends));
            value = tr(d.room.approve_friends ? S::on : S::off);
            break;
        case Row::room_ap_password:
            std::snprintf(title, sizeof title, "%s", tr(S::set_ap_password));
            value = tr(d.room.approve_password ? S::on : S::off);
            break;
        case Row::room_chat:
            std::snprintf(title, sizeof title, "%s", tr(S::set_chat));
            value = tr(!std::strcmp(d.room.chat, "on") ? S::chat_mode_on : !std::strcmp(d.room.chat, "quick") ? S::chat_mode_quick : S::chat_mode_off);
            break;
        case Row::room_voice:
            std::snprintf(title, sizeof title, "%s", tr(S::set_voice_mode));
            value = tr(!std::strcmp(d.room.voice, "open") ? S::voice_open : !std::strcmp(d.room.voice, "ptt") ? S::voice_ptt : S::voice_off);
            break;
        case Row::room_start_muted:
            std::snprintf(title, sizeof title, "%s", tr(S::set_start_muted));
            value = tr(d.room.start_muted ? S::on : S::off);
            break;
        case Row::room_mute_all:
        {
            bool any_unmuted = false;
            for (int k = 0; k < d.n_guests; ++k)
                any_unmuted = any_unmuted || !d.guests[k].muted_voice;
            std::snprintf(title, sizeof title, "%s", tr(any_unmuted ? S::mute_all : S::unmute_all));
            break;
        }
        case Row::room_lock:
            std::snprintf(title, sizeof title, "%s", tr(d.room.locked ? S::room_unlock : S::room_lock));
            break;
        case Row::room_blocked: std::snprintf(title, sizeof title, tr(S::room_blocked), d.room.n_blocked); break;
        case Row::room_name:
            std::snprintf(title, sizeof title, "%s", tr(S::set_room_name));
            value = d.room.name;
            break;
        case Row::room_ready:
            std::snprintf(title, sizeof title, "%s", tr(S::ready_check));
            if (d.room.ready_left_s >= 0 && d.room.ready_all)
                value = tr(S::ready_all);
            else if (d.room.ready_left_s >= 0)
            {
                std::snprintf(vbuf, sizeof vbuf, tr(S::ready_running), d.room.ready_n, d.room.ready_left_s);
                value = vbuf;
            }
            break;
        case Row::sw_block:
            std::snprintf(title, sizeof title, "%s", tr(S::host_sw_block));
            value = tr(d.block_system_buttons ? S::on : S::off);
            break;
        case Row::sw_seat:
            std::snprintf(title, sizeof title, "%s", tr(S::host_sw_seat));
            value = tr(d.stream_seat ? S::on : S::off);
            break;
        case Row::sw_invites:
            std::snprintf(title, sizeof title, "%s", tr(S::host_sw_invites));
            value = tr(!std::strcmp(d.invites, "wifi") ? S::wifi_only : S::anywhere);
            break;
        case Row::sw_awake:
            std::snprintf(title, sizeof title, "%s", tr(S::host_sw_awake));
            value = tr(!std::strcmp(d.keep_awake, "always") ? S::awake_always
                       : !std::strcmp(d.keep_awake, "transfers") ? S::awake_transfers
                                                                 : S::awake_off);
            break;
        case Row::pair: break;
        case Row::rp_enable:
            std::snprintf(title, sizeof title, "%s", tr(net::now_us() < rp_confirm_until_ ? S::host_rp_enable_confirm : S::host_rp_enable));
            break;
        case Row::rp_pin:
            std::snprintf(title, sizeof title, "%s", tr(!std::strcmp(hs.pin.state, "waiting") ? S::host_rp_pin_stop : S::host_rp_pin));
            break;
        case Row::tab_hint:
            std::snprintf(title, sizeof title, "%s",
                          tr(host_tab_ == 0 ? (d.playing ? S::host_together_closed : S::host_room_closed) : S::host_rp_setup));
            break;
        }
        if (r.kind == Row::tab_hint)
        {
            c.text_wrap(Font::small, kLeft + 10, y + 10, row_w - 20, title, col::muted);
            y += row_h;
            continue;
        }
        const bool with_bars = r.kind == Row::guest && bars >= 0;
        draw_row(c, kLeft, y, row_w, row_h - 10, s, title, nullptr, value, with_bars ? 80 : 0);
        if (r.kind == Row::ask)
            c.circle(kLeft + 14, y + (row_h - 10) / 2, 7, col::warn);
        if (with_bars)
        {
            // beside the value: left of it, or right of it in a right-to-left language
            const int vw = value ? Canvas::text_width(Font::body, value) : 0;
            const int bx = ui::i18n::right_to_left() ? kLeft + 32 + vw + 14 : kLeft + row_w - 32 - vw - 70;
            draw_bars(c, bx, y + (row_h - 10) / 2 - 16, bars, false);
        }
        y += row_h;
    }
    // Right: the Remote Play PIN while it is up, else the room's invite (code + QR).
    const int rx = 1330;
    const bool pin_up = !std::strcmp(hs.pin.state, "waiting") || !std::strcmp(hs.pin.state, "paired");
    if (pin_up)
    {
        c.text(Font::title, rx, 210, tr(S::host_rp_pin_title), col::text);
        if (!std::strcmp(hs.pin.state, "paired"))
            c.text_wrap(Font::body, rx, 290, 520, tr(S::host_rp_pin_paired), col::good);
        else
        {
            char shown[20];
            const std::size_t pl = std::strlen(hs.pin.pin);
            if (pl == 8)
                std::snprintf(shown, sizeof shown, "%.4s %.4s", hs.pin.pin, hs.pin.pin + 4);
            else
                std::snprintf(shown, sizeof shown, "%s", hs.pin.pin);
            c.text(Font::big, rx, 290, shown, col::text);
            std::snprintf(buf, sizeof buf, tr(S::host_rp_pin_text), hs.pin.seconds_left / 60, hs.pin.seconds_left % 60);
            c.text_wrap(Font::small, rx, 390, 520, buf, col::muted);
        }
    }
    else if (host_tab_ == 0 && d.together_open && d.has_invite)
    {
        c.text(Font::small, rx, 210, tr(S::host_code), col::muted);
        c.text(Font::big, rx, 240, d.code_display, col::text);
        std::snprintf(buf, sizeof buf, tr(S::host_code_left), (d.invite_left_s + 59) / 60);
        c.text(Font::small, rx, 330, buf, col::muted);
        if (d.url[0])
        {
            static std::uint8_t qr[qrcodegen_BUFFER_LEN_MAX], tmp[qrcodegen_BUFFER_LEN_MAX];
            if (qrcodegen_encodeText(d.url, tmp, qr, qrcodegen_Ecc_MEDIUM, 1, 20, qrcodegen_Mask_AUTO, true))
            {
                const int size = qrcodegen_getSize(qr);
                const int module = (400 / (size + 4)) > 2 ? 400 / (size + 4) : 2;
                c.qr(qr, rx, 380, module);
                c.text(Font::small, rx, 380 + (size + 4) * module + 14,
                       tr(!std::strcmp(d.works, "anywhere") ? S::host_works_anywhere : S::host_works_wifi), col::muted);
            }
        }
        if (!d.n_guests)
            c.text_wrap(Font::small, rx, 880, 520, tr(d.room.known ? S::room_none : S::host_no_guests), col::muted);
    }
    else if (host_tab_ == 0)
        c.text_wrap(Font::small, rx, 220, 520, tr(S::room_easier), col::muted);
    // what to press for the selected row
    const Row *r = n ? &rows[sel_] : nullptr;
    if (r && r->kind == Row::ask)
    {
        bool group = false, own = false;
        for (int k = 0; k < d.n_asks; ++k)
            if (d.asks[k].id == r->id)
                own = d.asks[k].own, group = !own && d.asks[k].group_size > 1;
        const int g[] = {G_CROSS, G_SQUARE, G_TRIANGLE, G_CIRCLE};
        const S l[] = {S::hint_allow, S::hint_deny, own ? S::hint_its_me : S::hint_allow_all, S::hint_back};
        if (group || own)
            draw_hints(c, g, l, 4);
        else
        {
            const int g2[] = {G_CROSS, G_SQUARE, G_L1, G_CIRCLE};
            const S l2[] = {S::hint_allow, S::hint_deny, S::hint_tabs, S::hint_back};
            draw_hints(c, g2, l2, 4);
        }
    }
    else if (r && r->kind == Row::guest)
    {
        const int g[] = {G_TRIANGLE, G_L1, G_CIRCLE};
        const S l[] = {S::hint_menu, S::hint_tabs, S::hint_back};
        draw_hints(c, g, l, 3);
    }
    else
    {
        const int g[] = {G_CROSS, G_L1, G_CIRCLE};
        const S l[] = {r && r->kind >= Row::sw_block && r->kind != Row::close && r->kind != Row::new_code &&
                               r->kind != Row::room_ready
                           ? S::hint_change
                           : S::hint_choose,
                       S::hint_tabs, S::hint_back};
        draw_hints(c, g, l, 3);
    }
    // What PS5Upload refused just now (the last action), else the note about games.
    if (v.last_error[0] && net::now_us() - v.last_error_us < 6000000)
    {
        const char *e = v.last_error;
        if (!std::strcmp(e, "slow_down"))
            std::snprintf(buf, sizeof buf, "%s", tr(S::err_slow));
        else if (!std::strcmp(e, "nobody"))
            std::snprintf(buf, sizeof buf, "%s", tr(S::err_nobody));
        else if (!std::strcmp(e, "running"))
            std::snprintf(buf, sizeof buf, "%s", tr(S::err_ready_running));
        else if (!std::strcmp(e, "not_own"))
            std::snprintf(buf, sizeof buf, "%s", tr(S::err_not_own));
        else
            std::snprintf(buf, sizeof buf, tr(S::host_said_no), e);
        c.text(Font::small, kLeft, 940, buf, col::warn, 1700);
    }
    else
        c.text(Font::small, kLeft, 940, tr(S::host_game_note), col::muted);
}
} // namespace play
