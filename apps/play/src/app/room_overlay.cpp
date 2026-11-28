// PS5Upload Play - the room while playing (docs/ROOMS.md, the Play Rooms design's 7.3, 7A):
// chat lines over the picture (the last 3, fading after 6 s), the unread count, who is talking,
// the mic's state, and the Room menu (hold OPTIONS + TOUCHPAD, then △): your ping and the host's
// PS5 link, quick messages, a typed line, the mic and how you talk, the game's sound while others
// talk, each person's volume and mute, the rules, and leaving. While the menu is open the
// controller is the menu's; the game gets "nothing pressed".
// SPDX-License-Identifier: GPL-3.0-or-later
#include "app.hpp"

#include "../core/notice.hpp"
#include "log.hpp"
#include "platform.hpp"

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

constexpr std::uint32_t kUp = pad::kUp, kDown = pad::kDown, kLeftB = pad::kLeft, kRight = pad::kRight;
constexpr int kQuickCells = chat::kQuick; // a 4 x 4 grid first
enum Row : int
{
    r_type = kQuickCells,
    r_mic,
    r_talk,
    r_duck,
    r_show_chat,
    r_people, // then one row per other person
};

// A straight-alpha panel on the overlay layer.
void panel(Canvas &l, int x, int y, int w, int h, std::uint32_t c, unsigned a) noexcept
{
    for (int yy = y < 0 ? 0 : y; yy < y + h && yy < ui::kH; ++yy)
        for (int xx = x < 0 ? 0 : x; xx < x + w && xx < ui::kW; ++xx)
            l.pixels()[yy * ui::kW + xx] = (c & 0xffffffu) | (a << 24);
}

// The others in the room as voice knows them: the engine's `people` (speaker ids, host = 0), else
// the room push's players.
struct Other
{
    int id = -1;
    const char *name = "";
    bool speaking = false;
};
int others(const rooms::RoomState &rs, Other *out, int cap) noexcept
{
    int n = 0;
    if (rs.n_people)
    {
        for (int i = 0; i < rs.n_people && n < cap; ++i)
            if (rs.people[i].sid >= 0 && rs.people[i].sid != rs.you_sid)
                out[n++] = Other{rs.people[i].sid, rs.people[i].name, rs.people[i].speaking};
        return n;
    }
    for (int i = 0; i < rs.n_players && n < cap; ++i)
        if (!rs.players[i].you && rs.players[i].id >= 0)
            out[n++] = Other{rs.players[i].id, rs.players[i].name, rs.players[i].speaking};
    return n;
}

const char *mic_line(const char *why) noexcept
{
    if (!*why)
        return tr(S::mic_sending);
    if (!std::strcmp(why, "hold"))
        return tr(S::mic_hold);
    if (!std::strcmp(why, "no_mic"))
        return tr(S::mic_no_mic);
    if (!std::strcmp(why, "mic_muted"))
        return tr(S::mic_muted_sys);
    if (!std::strcmp(why, "muted"))
        return tr(S::mic_host_muted);
    if (!std::strcmp(why, "room_off"))
        return tr(S::mic_room_off);
    if (!std::strcmp(why, "paused"))
        return tr(S::mic_paused);
    return tr(S::mic_off);
}
} // namespace

void App::voice_tick(const GuestView &v) noexcept
{
    const rooms::RoomState &rs = v.room;
    // Create is push-to-talk while the guest's PS & Create are kept from the game (always for
    // people from Rooms): it never reaches the game then (7A.3).
    const bool ptt_create = mic_on_ && v.block_system;
    platform::ptt_on_create(ptt_create);
    voice::Allow a;
    a.mic_on = mic_on_;
    // Open mic only with a headset (no echo cancelling on the console), else push-to-talk.
    const platform::MicState ms = fake_.on ? platform::MicState{} : platform::mic_state();
    a.mode = settings_.talk_open && ms.headset ? voice::Mode::open : voice::Mode::ptt;
    a.talk_held = ptt_create && talk_held_;
    a.room = rs.voice;
    a.host_muted = rs.muted_voice;
    a.paused = rs.paused[0] != 0;
    a.in_front = was_front_;
    a.silent = ms.silent;
    a.kbps = rs.comms_known ? rs.kbps : 24;
    a.frame_ms = rs.frame_ms;
    platform::voice_allow(a);
    if (const char *edge = ptt_.update(a.talk_held && mic_on_))
        guest_->send_mic(edge);
    platform::voice_duck(settings_.duck);
}

void App::overlay_frame(std::uint64_t now) noexcept
{
    (void)now;
    const GuestView v = guest_view();
    if (v.chat_rev != ov_chat_rev_)
    {
        ov_chat_rev_ = v.chat_rev;
        guest_->chat_copy(ov_chat_);
        dirty_ = true;
    }
    // The notice accepted on this PS5 reaches the engine once per join (API.md: nobody sends or
    // gets chat or voice before that).
    const rooms::RoomState &rs = v.room;
    if (rs.comms_known && !rs.accepted && rs.notice_ver > 0 && settings_.notice >= rs.notice_ver && !notice_sent_)
        notice_sent_ = guest_->send_notice(settings_.notice);
    if (rs.refused_rev != refused_seen_)
    {
        refused_seen_ = rs.refused_rev;
        const char *w = rs.refused_why;
        if (!std::strcmp(w, "notice"))
        {
            notice_sent_ = false;
            if (settings_.notice < rs.notice_ver)
            {
                ov_open_ = false;
                pending_ = Pending::none;
                go(Screen::notice);
            }
        }
        else
        {
            const bool voice_side = !std::strcmp(rs.refused_what, "voice") || !std::strcmp(rs.refused_what, "mic");
            S s = S::chat_off;
            if (!std::strcmp(w, "off"))
                s = voice_side ? S::mic_room_off : S::chat_off;
            else if (!std::strcmp(w, "quick_only"))
                s = S::chat_quick_only;
            else if (!std::strcmp(w, "muted"))
                s = voice_side ? S::mic_host_muted : S::chat_muted;
            else if (!std::strcmp(w, "slow_down"))
                s = S::chat_slow;
            else if (!std::strcmp(w, "too_long") || !std::strcmp(w, "empty"))
                s = S::chat_too_long;
            else if (!std::strcmp(w, "ptt_only"))
                s = S::voice_ptt_only;
            else if (!std::strcmp(w, "mic_off"))
                s = S::mic_off;
            else if (!std::strcmp(w, "poor"))
                s = S::mic_paused;
            else
                s = voice_side ? S::mic_off : S::chat_off;
            std::snprintf(note_, sizeof note_, "%s", tr(s));
            note_until_ = net::now_us() + 3000000;
            ov_note_until_ = note_until_;
        }
    }
    voice_tick(v);
}

void App::input_overlay(std::uint32_t p) noexcept
{
    const GuestView v = guest_view();
    const rooms::RoomState &rs = v.room;
    Other people[8];
    const int np = others(rs, people, 8);
    const int n = r_people + np + 2; // + Rules, Leave
    const int r_rules = r_people + np, r_leave = r_rules + 1;
    if (p & (pad::kCircle | pad::kOptions))
    {
        ov_open_ = false;
        platform::blip(platform::Blip::back);
        guest_->chat_read();
        return;
    }
    // the quick grid: 4 x 4, then the rows below
    if (ov_sel_ < kQuickCells)
    {
        if (p & kLeftB)
            ov_sel_ = ov_sel_ % 4 ? ov_sel_ - 1 : ov_sel_ + 3;
        if (p & kRight)
            ov_sel_ = ov_sel_ % 4 == 3 ? ov_sel_ - 3 : ov_sel_ + 1;
        if (p & kUp)
            ov_sel_ = ov_sel_ < 4 ? r_leave : ov_sel_ - 4;
        else if (p & kDown)
            ov_sel_ = ov_sel_ + 4 >= kQuickCells ? r_type : ov_sel_ + 4;
    }
    else
    {
        if (p & kUp)
            ov_sel_ = ov_sel_ == r_type ? kQuickCells - 4 : ov_sel_ - 1;
        else if (p & kDown)
            ov_sel_ = ov_sel_ + 1 >= n ? 0 : ov_sel_ + 1;
    }
    if (p & (kUp | kDown | kLeftB | kRight))
        platform::blip(platform::Blip::move);
    const bool cross = p & pad::kCross, left = p & kLeftB, right = p & kRight;
    if (ov_sel_ < kQuickCells)
    {
        if (!cross)
            return;
        if (!std::strcmp(rs.chat, "off") || rs.muted_chat)
        {
            platform::blip(platform::Blip::bad);
            return;
        }
        if (!need_notice(static_cast<int>(notice::Need::chat), Pending::none))
            return;
        const bool sent = guest_->send_quick(ov_sel_ + 1);
        platform::blip(sent ? platform::Blip::choose : platform::Blip::bad);
        return;
    }
    switch (ov_sel_)
    {
    case r_type:
        if (cross)
        {
            if (strcmp(rs.chat, "text") != 0 && strcmp(rs.chat, "on") != 0)
            {
                std::snprintf(note_, sizeof note_, "%s", tr(S::chat_quick_only));
                note_until_ = net::now_us() + 3000000;
                platform::blip(platform::Blip::bad);
                break;
            }
            if (!need_notice(static_cast<int>(notice::Need::chat), Pending::chat))
                break;
            ov_open_ = false;
            open_keyboard(Kb::chat, tr(S::kb_title_chat), "", chat::kTextMax, false);
        }
        break;
    case r_mic:
        if (cross || left || right)
        {
            if (!mic_on_)
            {
                if (!need_notice(static_cast<int>(notice::Need::voice), Pending::mic))
                    break;
                mic_on_ = true;
                guest_->send_mic("on");
            }
            else
            {
                mic_on_ = false;
                guest_->send_mic("off");
            }
            platform::blip(platform::Blip::choose);
        }
        break;
    case r_talk:
        if (cross || left || right)
        {
            settings_.talk_open = !settings_.talk_open;
            if (settings_.talk_open && !platform::mic_state().headset)
            {
                std::snprintf(note_, sizeof note_, "%s", tr(S::mic_headset));
                note_until_ = net::now_us() + 4000000;
            }
            save();
            platform::blip(platform::Blip::choose);
        }
        break;
    case r_duck:
        if (cross || left || right)
        {
            settings_.duck = (settings_.duck + (left ? 2 : 1)) % 3;
            save();
            platform::blip(platform::Blip::choose);
        }
        break;
    case r_show_chat:
        if (cross || left || right)
        {
            settings_.show_chat = !settings_.show_chat;
            save();
            platform::blip(platform::Blip::choose);
        }
        break;
    default:
        if (ov_sel_ == r_rules && cross)
        {
            ov_open_ = false;
            pending_ = Pending::none;
            go(Screen::notice);
        }
        else if (ov_sel_ == r_leave && cross)
        {
            ov_open_ = false;
            log("app: leave from the Room menu");
            leave_game();
        }
        else if (ov_sel_ >= r_people && ov_sel_ < r_rules)
        {
            // a person: ←/→ volume (10 % steps, 0-200), □ mute (for you only), △ tell the host
            const int id = people[ov_sel_ - r_people].id;
            if (p & pad::kTriangle)
            {
                const bool sent = guest_->send_flag(id);
                if (sent)
                {
                    std::snprintf(note_, sizeof note_, tr(S::flag_sent), people[ov_sel_ - r_people].name);
                    note_until_ = net::now_us() + 3000000;
                }
                platform::blip(sent ? platform::Blip::choose : platform::Blip::bad);
            }
            if (left || right)
            {
                mixer_.set_volume(id, mixer_.volume(id) + (left ? -10 : 10));
                platform::voice_volume(id, mixer_.volume(id), mixer_.muted(id));
                platform::blip(platform::Blip::move);
            }
            if (p & pad::kSquare)
            {
                mixer_.set_muted(id, !mixer_.muted(id));
                platform::voice_volume(id, mixer_.volume(id), mixer_.muted(id));
                platform::blip(platform::Blip::choose);
            }
        }
        break;
    }
}

int App::draw_chat_lines(Canvas &l, const GuestView &v, std::uint64_t now) noexcept
{
    // Bottom-left: the newest lines (at most 3), each fading after 6 s; nothing while hidden.
    if (!settings_.show_chat || ov_open_)
        return 0;
    const chat::Line *lines[3];
    const chat::Log &log_ = fake_.on ? fake_.chat : ov_chat_;
    const int k = log_.recent(lines, 3, fake_.on ? log_.at(log_.size() - 1).at_us + 1000 : now);
    if (!k)
        return 0;
    const bool hide = settings_.hide_rude == 1 || (settings_.hide_rude == -1 && !std::strcmp(v.room.chat, "quick"));
    const int lh = 52, h = k * lh + 24;
    const int y0 = ui::kH - h - 40;
    for (int i = 0; i < k; ++i)
    {
        char text[700], shown[700];
        const chat::Line &ln = *lines[i];
        if (ln.quick)
            std::snprintf(text, sizeof text, "%s", quick_text(ln.quick));
        else
            chat::for_display(ln.text, text, sizeof text, hide);
        std::snprintf(shown, sizeof shown, tr(S::chat_line), ln.from[0] ? ln.from : tr(S::host_friend), text);
        if (ln.times > 1)
        {
            const std::size_t o = std::strlen(shown);
            std::snprintf(shown + o, sizeof shown - o, " ×%d", ln.times);
        }
        const int w = Canvas::text_width(Font::body, shown) + 40;
        const int y = y0 + 12 + i * lh;
        panel(l, 40, y, w > 1100 ? 1100 : w, lh - 6, col::panel, 200);
        l.text(Font::body, 60, y + (lh - 6 - ui::font_height(Font::body)) / 2, shown, ln.mine ? col::muted : col::text, 1060);
    }
    return h + 40;
}

int App::draw_overlay(Canvas &l, const GuestView &v, std::uint64_t now) noexcept
{
    (void)now;
    const rooms::RoomState &rs = v.room;
    // Top left: who is talking; top right (under the way chip): unread and the mic.
    int used = 0;
    {
        int x = 40;
        Other who[8];
        const int nw = others(rs, who, 8);
        const std::uint32_t heard = fake_.on ? 0u : platform::voice_speaking();
        for (int i = 0; i < nw; ++i)
        {
            const bool talking = who[i].speaking || (who[i].id < 32 && (heard >> who[i].id & 1u));
            if (!talking)
                continue;
            const int w = Canvas::text_width(Font::small, who[i].name) + 64;
            panel(l, x, 34, w, 48, col::panel, 215);
            // a speaking ring
            for (int yy = 46; yy < 70; ++yy)
                for (int xx = x + 14; xx < x + 38; ++xx)
                {
                    const int dx = xx - (x + 26), dy = yy - 58;
                    if (dx * dx + dy * dy <= 121 && dx * dx + dy * dy >= 49)
                        l.pixels()[yy * ui::kW + xx] = (col::good & 0xffffffu) | (255u << 24);
                }
            l.text(Font::small, x + 46, 34 + (48 - ui::font_height(Font::small)) / 2, who[i].name, col::text);
            x += w + 12;
            used = 100;
        }
        char line[160] = {};
        if (v.chat_unread && !ov_open_)
            std::snprintf(line, sizeof line, tr(S::chat_new), v.chat_unread);
        if (seat_toast_until_ && net::now_us() < seat_toast_until_)
        {
            const std::size_t o = std::strlen(line);
            std::snprintf(line + o, sizeof line - o, "%s%s", o ? " · " : "", seat_toast_);
        }
        if (mic_on_)
        {
            const platform::MicState ms = fake_.on ? platform::MicState{} : platform::mic_state();
            voice::Allow a;
            a.mic_on = true;
            a.mode = settings_.talk_open && ms.headset ? voice::Mode::open : voice::Mode::ptt;
            a.talk_held = ptt_.held();
            a.room = rs.voice;
            a.host_muted = rs.muted_voice;
            a.paused = rs.paused[0] != 0;
            a.silent = fake_.on ? 0 : ms.silent;
            const std::size_t o = std::strlen(line);
            std::snprintf(line + o, sizeof line - o, "%s%s", o ? " · " : "", mic_line(voice::why_not(a)));
        }
        if (line[0])
        {
            const int w = Canvas::text_width(Font::small, line) + 48;
            panel(l, ui::kW - w - 40, 96, w, 48, col::panel, 215);
            l.text(Font::small, ui::kW - w - 16, 96 + (48 - ui::font_height(Font::small)) / 2, line, col::text);
            used = 160;
        }
    }
    if (!ov_open_)
        return used;
    // The Room menu: a panel on the right half.
    const int x = 900, y = 160, w = 960, h = 890;
    panel(l, x, y, w, h, col::bg, 232);
    char b[200];
    l.text(Font::title, x + 40, y + 24, rs.name[0] ? rs.name : tr(S::ov_title), col::text, w - 80);
    // your ping and the host's PS5 link
    int yy = y + 96;
    {
        int xx = x + 40;
        if (rs.you_rtt >= 0)
        {
            xx += draw_bars(l, xx, yy, rooms::bars_of(rs.you_tier), false) + 12;
            std::snprintf(b, sizeof b, "%s %d ms", tr(S::ov_you), rs.you_rtt);
            xx += l.text(Font::small, xx, yy + 4, b, col::text) + 40;
        }
        if (rs.link_rtt >= 0)
        {
            xx += draw_bars(l, xx, yy, rooms::bars_of(rs.link_tier), false) + 12;
            std::snprintf(b, sizeof b, "%s %d ms", tr(S::ov_host_link), rs.link_rtt);
            l.text(Font::small, xx, yy + 4, b, col::text);
        }
        yy += 54;
    }
    // quick messages: 4 x 4
    const bool chat_ok = std::strcmp(rs.chat, "off") != 0 && !rs.muted_chat && rs.chat[0];
    const int cw = 214, chh = 48;
    for (int i = 0; i < kQuickCells; ++i)
    {
        const int cx = x + 40 + (i % 4) * (cw + 8), cy = yy + (i / 4) * (chh + 8);
        const bool s = ov_sel_ == i;
        panel(l, cx, cy, cw, chh, s ? col::accent : col::panel, 240);
        l.text(Font::small, cx + 14, cy + (chh - ui::font_height(Font::small)) / 2, quick_text(i + 1),
               s ? col::on_accent : chat_ok ? col::text : col::muted, cw - 20);
    }
    yy += 4 * (chh + 8) + 14;
    if (!chat_ok)
    {
        l.text(Font::small, x + 40, yy, tr(!rs.chat[0] ? S::mic_needs_host : rs.muted_chat ? S::chat_muted : S::chat_off), col::warn, w - 80);
        yy += 40;
    }
    // the rows
    Other people[8];
    const int np = others(rs, people, 8);
    const int rows_n = r_people + np + 2;
    for (int r = r_type; r < rows_n && yy < y + h - 90; ++r)
    {
        const bool s = ov_sel_ == r;
        const char *title = "", *value = nullptr;
        char vb[64];
        if (r == r_type)
            title = tr(S::ov_type);
        else if (r == r_mic)
            title = tr(S::ov_mic), value = tr(mic_on_ ? S::on : S::off);
        else if (r == r_talk)
            title = tr(S::ov_how_talk), value = tr(settings_.talk_open ? S::talk_open : S::talk_ptt);
        else if (r == r_duck)
            title = tr(S::ov_duck), value = tr(settings_.duck == 0 ? S::duck_off : settings_.duck == 1 ? S::duck_little : S::duck_more);
        else if (r == r_show_chat)
            title = tr(S::ov_show_chat), value = tr(settings_.show_chat ? S::on : S::off);
        else if (r < r_people + np)
        {
            const Other &pl = people[r - r_people];
            title = pl.name;
            if (mixer_.muted(pl.id))
                value = tr(S::ov_muted);
            else
            {
                std::snprintf(vb, sizeof vb, tr(S::ov_volume), mixer_.volume(pl.id));
                value = vb;
            }
        }
        else if (r == r_people + np)
            title = tr(S::ov_rules);
        else
            title = tr(S::ov_leave);
        panel(l, x + 30, yy, w - 60, 42, s ? col::panel_hi : col::panel, 235);
        if (s)
            panel(l, x + 30, yy, 6, 42, col::accent, 255);
        l.text(Font::small, x + 52, yy + (42 - ui::font_height(Font::small)) / 2, title, col::text, w - 340);
        if (value)
            l.text(Font::small, x + w - 300, yy + (42 - ui::font_height(Font::small)) / 2, value, s ? col::accent : col::muted, 260);
        yy += 47;
    }
    // what works here
    char hint[200];
    std::snprintf(hint, sizeof hint, "✕ %s   ← → %s   □ %s   △ %s   ○ %s", tr(S::hint_choose), tr(S::hint_volume),
                  tr(S::hint_mute), tr(S::hint_flag), tr(S::hint_close));
    l.text(Font::small, x + 40, y + h - 50, hint, col::muted, w - 80);
    return ui::kH; // the whole layer is drawn on
}
} // namespace play
