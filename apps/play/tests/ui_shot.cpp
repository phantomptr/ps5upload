// PS5Upload Play - render every screen on a computer (BMP files) to check the layout:
//   ui-shot ASSETS_DIR OUT_DIR
// ASSETS_DIR holds font.bin, ui-bg.rgba and logo.rgba from the app build.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "../src/app/app.hpp"
#include "../src/app/platform.hpp"
#include "../src/app/rooms_net.hpp"
#include "../src/ui/canvas.hpp"

#include <cstdio>
#include <cstdlib>
#include <cstring>

using namespace play;

namespace
{
class NullSinks final : public GuestSinks
{
  public:
    void begin(bool) override {}
    void end() override {}
    void video_header(const std::uint8_t *, std::size_t) override {}
    void video(const std::uint8_t *, std::size_t, std::uint64_t) override {}
    void audio_header(const std::uint8_t *, std::size_t) override {}
    void audio(std::uint16_t, const std::uint8_t *, std::size_t) override {}
    void rumble(std::uint8_t, std::uint8_t) override {}
    void light(std::uint8_t, std::uint8_t, std::uint8_t) override {}
    bool wants_keyframe() override { return false; }
    void stats(char *out, std::size_t cap) override { std::snprintf(out, cap, "\"frames\":0"); }
};
NullSinks null_sinks;
std::uint8_t buf0[1 << 20], buf1[1 << 20];
std::uint32_t *presented = nullptr;
const char *assets_dir = ".";

std::uint8_t *slurp(const char *path, std::size_t *len)
{
    FILE *f = std::fopen(path, "rb");
    if (!f)
        return nullptr;
    std::fseek(f, 0, SEEK_END);
    const long n = std::ftell(f);
    std::fseek(f, 0, SEEK_SET);
    auto *d = static_cast<std::uint8_t *>(std::malloc(static_cast<std::size_t>(n)));
    *len = std::fread(d, 1, static_cast<std::size_t>(n), f);
    std::fclose(f);
    return d;
}

void write_bmp(const char *path, const std::uint32_t *px)
{
    FILE *f = std::fopen(path, "wb");
    if (!f)
        return;
    const int w = ui::kW, h = ui::kH;
    const std::uint32_t size = 54 + static_cast<std::uint32_t>(w * h * 3);
    std::uint8_t hdr[54] = {'B', 'M'};
    auto put = [&](int at, std::uint32_t v) {
        for (int k = 0; k < 4; ++k)
            hdr[at + k] = static_cast<std::uint8_t>(v >> (8 * k));
    };
    put(2, size);
    put(10, 54);
    put(14, 40);
    put(18, static_cast<std::uint32_t>(w));
    put(22, static_cast<std::uint32_t>(h));
    hdr[26] = 1;
    hdr[28] = 24;
    std::fwrite(hdr, 1, 54, f);
    static std::uint8_t row[ui::kW * 3];
    for (int y = h - 1; y >= 0; --y)
    {
        for (int x = 0; x < w; ++x)
        {
            const std::uint32_t p = px[y * w + x];
            row[x * 3] = static_cast<std::uint8_t>(p >> 16);
            row[x * 3 + 1] = static_cast<std::uint8_t>(p >> 8);
            row[x * 3 + 2] = static_cast<std::uint8_t>(p);
        }
        std::fwrite(row, 1, sizeof row, f);
    }
    std::fclose(f);
}
} // namespace

namespace play::platform
{
Input input() noexcept { return {}; }
void stream_pad(Guest *) noexcept {}
Chords chords() noexcept { return {}; }
Chords chords_peek() noexcept { return {}; }
void pad_to_menu(bool) noexcept {}
void ptt_on_create(bool) noexcept {}
void voice_allow(const voice::Allow &) noexcept {}
MicState mic_state() noexcept
{
    MicState m;
    m.open = true;
    m.silent = 0;
    return m;
}
void voice_volume(int, int, bool) noexcept {}
void voice_duck(int) noexcept {}
std::uint32_t voice_speaking() noexcept { return 0; }
int local_pads(LocalPad *out) noexcept
{
    for (int i = 0; i < kLocalPads; ++i)
        out[i] = LocalPad{};
    return 0;
}
void attach_pad(int, Guest *) noexcept {}
bool pad_leave(int) noexcept { return false; }
GuestSinks &seat_sinks(int, int) noexcept { return null_sinks; }
std::uint8_t *seat_buffer(int, std::size_t *cap) noexcept
{
    static std::uint8_t b[3][64 * 1024];
    static int at = 0;
    *cap = sizeof b[0];
    return b[at++ % 3];
}
bool video_showing() noexcept { return false; }
void present(const std::uint32_t *ui) noexcept { presented = const_cast<std::uint32_t *>(ui); }
void overlay(const std::uint32_t *, int, int) noexcept {}
void blip(Blip) noexcept {}
void music(bool, int) noexcept {}
void sounds(bool) noexcept {}
void power_tick() noexcept {}
const char *user_name() noexcept { return "Tester"; }
long read_file(const char *, void *, std::size_t) noexcept { return -1; }
bool write_file(const char *, const void *, std::size_t) noexcept { return true; }
const char *data_dir() noexcept { return "/tmp"; }
const std::uint8_t *asset(const char *name, std::size_t *len) noexcept
{
    char p[512];
    std::snprintf(p, sizeof p, "%s/%s", assets_dir, name);
    return slurp(p, len);
}
int system_language() noexcept { return -1; }
void exit_app() noexcept { std::exit(0); }
GuestSinks &sinks() noexcept { return null_sinks; }
std::uint8_t *message_buffer(int which, std::size_t *cap) noexcept
{
    *cap = sizeof buf0;
    return which ? buf1 : buf0;
}
MediaStats media_stats() noexcept { return {}; }
} // namespace play::platform

int main(int argc, char **argv)
{
    if (argc < 3)
        return 2;
    assets_dir = argv[1];
    char p[512];
    std::size_t n = 0;
    std::snprintf(p, sizeof p, "%s/font.bin", argv[1]);
    std::uint8_t *font = slurp(p, &n);
    if (!font || !ui::font_load(font, n))
    {
        std::fprintf(stderr, "no font at %s\n", p);
        return 1;
    }
    ui::Image bg, logo;
    std::snprintf(p, sizeof p, "%s/ui-bg.rgba", argv[1]);
    if (std::uint8_t *d = slurp(p, &n))
        ui::image_from_rgba(d, n, bg);
    std::snprintf(p, sizeof p, "%s/logo.rgba", argv[1]);
    if (std::uint8_t *d = slurp(p, &n))
        ui::image_from_rgba(d, n, logo);
    static App app("13.0.0", bg, logo);
    // ui-shot ASSETS OUT [LANG]: every screen in that language
    if (argc > 3)
        app.set_language(argv[3]);
    static std::uint32_t ui_px[ui::kW * ui::kH], layer[ui::kW * ui::kH], out[ui::kW * ui::kH];
    auto shot = [&](const char *name, App::Screen s, const App::Fake &f) {
        app.fake(s, f);
        bool show = true;
        int top = 0, bottom = 0;
        app.draw(ui_px, layer, 1000000, &show, &top, &bottom);
        if (show)
            std::memcpy(out, ui_px, sizeof out);
        else
        {
            // a stand-in picture with the overlay bands blended on it, as the console does
            for (int y = 0; y < ui::kH; ++y)
                for (int x = 0; x < ui::kW; ++x)
                {
                    std::uint32_t v = ui::rgb(static_cast<unsigned>(x * 255 / ui::kW), 90, static_cast<unsigned>(y * 255 / ui::kH));
                    if (y < top || y >= ui::kH - bottom)
                    {
                        const std::uint32_t o = layer[y * ui::kW + x];
                        const unsigned a = o >> 24;
                        auto ch = [&](int sh) {
                            return (((o >> sh) & 0xff) * a + ((v >> sh) & 0xff) * (255 - a)) / 255;
                        };
                        v = ui::rgb(ch(0), ch(8), ch(16));
                    }
                    out[y * ui::kW + x] = v;
                }
        }
        char path[512];
        std::snprintf(path, sizeof path, "%s/%s.bmp", argv[2], name);
        write_bmp(path, out);
        std::printf("%s\n", path);
    };
    App::Fake f;
    f.on = true;
    shot("home", App::Screen::home, f);
    // Easy join (docs/EASY_JOIN.md): a game open nearby, and the phone code.
    {
        App::Fake e = f;
        e.found = 1;
        e.answers[0].ip_be = 0x1456a8c0;
        std::snprintf(e.answers[0].info.name, sizeof e.answers[0].info.name, "Living room PC");
        e.answers[0].info.port = 19113;
        e.answers[0].info.together = 1;
        e.room.state = WaitingRoom::View::ready;
        std::snprintf(e.room.qr, sizeof e.room.qr, "%s",
                      "https://ps5upload.phantomptr.com/join/#ps5=QmC7v41CxdU7YX73vWnXjh&t=F3wc41E7TkEaYdwc&hub=hub.phantomptr.com");
        shot("home-easy", App::Screen::home, e);
        e.room = WaitingRoom::View{};
        e.room.expired = true;
        shot("home-easy-off", App::Screen::home, e);
    }
    f.found = 1;
    f.answers[0].ip_be = 0x1456a8c0; // 192.168.86.20
    std::snprintf(f.answers[0].info.name, sizeof f.answers[0].info.name, "Living room PC");
    f.answers[0].info.port = 19113;
    shot("pick", App::Screen::pick, f);
    shot("way", App::Screen::way, f);
    shot("code", App::Screen::code, f);
    shot("address", App::Screen::address, f);
    f.guest.phase = GuestView::asking;
    std::snprintf(f.guest.engine, sizeof f.guest.engine, "192.168.86.20:19113");
    shot("asking", App::Screen::joining, f);
    f.guest.phase = GuestView::refused;
    std::snprintf(f.guest.why, sizeof f.guest.why, "not_playing");
    shot("refused", App::Screen::joining, f);
    // An internet invite through this home's PS5Upload that is too old for the bridge.
    f.guest.phase = GuestView::failed;
    std::snprintf(f.guest.why, sizeof f.guest.why, "bridge_old");
    std::snprintf(f.guest.bridge_at, sizeof f.guest.bridge_at, "192.168.86.31:19113");
    shot("failed-bridge", App::Screen::joining, f);
    f.guest.bridge_at[0] = 0;
    f.guest.phase = GuestView::joined;
    f.guest.player = 2;
    std::snprintf(f.guest.host, sizeof f.guest.host, "Sam's PC");
    shot("playing-wait", App::Screen::playing, f);
    f.picture = true;
    f.ps = 0.6f;
    shot("playing-hold", App::Screen::playing, f);
    f.ps = 0;
    f.guest.move_first = true;
    std::snprintf(f.guest.host_user, sizeof f.guest.host_user, "Sam");
    shot("playing-move", App::Screen::playing, f);
    // the way to the host (docs/ROUTES.md): just changed, then Poor
    f.guest.move_first = false;
    f.guest.route = 1;
    f.guest.tier = 3;
    f.guest.route_rtt_ms = 23;
    f.guest.route_changed_us = 900000;
    shot("playing-way", App::Screen::playing, f);
    f.guest.route = 0;
    f.guest.tier = 1;
    f.guest.route_rtt_ms = 120;
    f.guest.route_changed_us = 0;
    shot("playing-poor", App::Screen::playing, f);
    f.guest.route = -1;
    f.host.phase = CompanionView::pairing;
    std::snprintf(f.host.pair_display, sizeof f.host.pair_display, "PQR-STU");
    f.host.pair_left_s = 251;
    shot("host-pair", App::Screen::host, f);
    f.host.phase = CompanionView::paired;
    CompanionDoc &d = f.host.doc;
    d.paired = d.playing = d.together_open = d.together_playing = d.has_invite = d.rp_paired = true;
    std::snprintf(d.engine_name, sizeof d.engine_name, "Living room PC");
    std::snprintf(d.codec, sizeof d.codec, "h265");
    d.bitrate_kbps = 15000;
    std::snprintf(d.code_display, sizeof d.code_display, "ABC-DEF");
    std::snprintf(d.url, sizeof d.url, "http://192.168.86.20:19113/join#i=0123456789abcdef0123456789abcdef");
    std::snprintf(d.works, sizeof d.works, "wifi");
    d.invite_left_s = 1700;
    d.n_asks = 1;
    std::snprintf(d.asks[0].name, sizeof d.asks[0].name, "Alex");
    std::snprintf(d.asks[0].client_kind, sizeof d.asks[0].client_kind, "ps5");
    d.n_guests = 1;
    d.guests[0].player = 2;
    d.guests[0].rtt_ms = 6;
    std::snprintf(d.guests[0].name, sizeof d.guests[0].name, "Jo");
    std::snprintf(d.guests[0].route, sizeof d.guests[0].route, "tailscale");
    std::snprintf(d.guests[0].tier, sizeof d.guests[0].tier, "good");
    std::snprintf(d.invites, sizeof d.invites, "anywhere");
    std::snprintf(d.keep_awake, sizeof d.keep_awake, "transfers");
    d.helper_present = 1;
    shot("host", App::Screen::host, f);
    f.helper_present = 1;
    f.rp.service_on = false;
    std::snprintf(f.pin.state, sizeof f.pin.state, "waiting");
    std::snprintf(f.pin.pin, sizeof f.pin.pin, "12345678");
    f.pin.seconds_left = 271;
    shot("host-helper", App::Screen::host, f);
    shot("settings", App::Screen::settings, f);

    // ---- Play Rooms (docs/ROOMS.md) ----
    App::Fake r;
    r.on = true;
    r.notice_accepted = 1;
    {
        // the Join list: a friend's room, a game nearby, public rooms
        RoomsNet::View &v = r.rooms;
        v.state = RoomsNet::State::ok;
        v.n_friends = 1;
        rooms::Card &fr = v.friends[0];
        std::snprintf(fr.room, sizeof fr.room, "lobbyF");
        std::snprintf(fr.name, sizeof fr.name, "Alex's room");
        std::snprintf(fr.host, sizeof fr.host, "Alex");
        std::snprintf(fr.tag, sizeof fr.tag, "77B0");
        std::snprintf(fr.game_title, sizeof fr.game_title, "ASTRO BOT");
        fr.in = 2, fr.max = 4, fr.proto = 2, fr.est_ms = 28, fr.cap_ps5app = true;
        const char *names[] = {"Couch chaos", "Sunday racers", "Late night co-op"};
        const char *hosts[] = {"Sam", "Kai", "Mo"};
        const char *games[] = {"It Takes Two", "Gran Turismo 7", "Split Fiction"};
        const int ins[] = {1, 4, 2}, maxes[] = {2, 4, 4}, ests[] = {70, 30, 110};
        for (int i = 0; i < 3; ++i)
        {
            rooms::Card &k = v.list.cards[v.list.n++];
            std::snprintf(k.room, sizeof k.room, "lobby%d", i);
            std::snprintf(k.name, sizeof k.name, "%s", names[i]);
            std::snprintf(k.host, sizeof k.host, "%s", hosts[i]);
            std::snprintf(k.tag, sizeof k.tag, "%s", i == 0 ? "4F2A" : i == 1 ? "9C11" : "1E3D");
            std::snprintf(k.game_title, sizeof k.game_title, "%s", games[i]);
            std::snprintf(k.lang, sizeof k.lang, "%s", i == 1 ? "de" : "en");
            k.in = ins[i], k.max = maxes[i], k.est_ms = ests[i], k.proto = 2, k.cap_ps5app = true, k.cap_preview = true;
            k.pw = i == 0;
            k.group = rooms::Group::pub;
        }
        r.found = 1;
        r.answers[0].ip_be = 0x1456a8c0;
        std::snprintf(r.answers[0].info.name, sizeof r.answers[0].info.name, "Dad's PC");
        r.answers[0].info.port = 19113;
        r.answers[0].info.together = 1;
        // the focused card's details
        v.detail = RoomsNet::Detail::ok;
        std::snprintf(v.detail_key, sizeof v.detail_key, "lobbyF");
        rooms::Preview &p = v.preview;
        p.ok = true;
        std::snprintf(p.host, sizeof p.host, "Alex");
        std::snprintf(p.tag, sizeof p.tag, "77B0");
        std::snprintf(p.room_name, sizeof p.room_name, "Alex's room");
        std::snprintf(p.game_title, sizeof p.game_title, "ASTRO BOT");
        std::snprintf(p.res, sizeof p.res, "1080p");
        std::snprintf(p.codec, sizeof p.codec, "hevc");
        p.fps = 60, p.kbps = 15000, p.playing_min = 42, p.n_players = 2;
        std::snprintf(p.players[0].name, sizeof p.players[0].name, "Alex");
        p.players[0].seat = 1;
        std::snprintf(p.players[0].tier, sizeof p.players[0].tier, "excellent");
        std::snprintf(p.players[1].name, sizeof p.players[1].name, "Mia");
        p.players[1].seat = 2;
        std::snprintf(p.players[1].tier, sizeof p.players[1].tier, "good");
        v.measured_ms = 28;
    }
    shot("rooms", App::Screen::rooms, r);
    shot("rooms-details", App::Screen::details, r);
    shot("rooms-filter", App::Screen::filter, r);
    {
        App::Fake e = r;
        e.rooms = RoomsNet::View{};
        e.rooms.state = RoomsNet::State::unreachable;
        e.found = 0;
        shot("rooms-offline", App::Screen::rooms, e);
    }
    app.fake_keyboard("Password for Couch chaos", "hunt", true);
    shot("keyboard", App::Screen::keyboard, r);
    r.notice_accepted = 0;
    shot("notice", App::Screen::notice, r);
    r.notice_accepted = 1;
    shot("thanks", App::Screen::thanks, r);
    app.fake_page(1, 0);
    shot("welcome", App::Screen::welcome, r);
    shot("help", App::Screen::help, r);
    // joining a room: a name already there, a wrong password, the real round trip while asking
    r.guest = GuestView{};
    r.guest.phase = GuestView::name_taken;
    std::snprintf(r.guest.suggest, sizeof r.guest.suggest, "Tester (2)");
    shot("name-taken", App::Screen::joining, r);
    r.guest.phase = GuestView::refused;
    std::snprintf(r.guest.why, sizeof r.guest.why, "password");
    r.guest.tries_left = 4;
    shot("wrong-password", App::Screen::joining, r);
    r.guest.phase = GuestView::asking;
    r.guest.route_rtt_ms = 31;
    r.guest.tier = 3;
    shot("asking-ping", App::Screen::joining, r);
    // many controllers on this PS5: one asking, one playing, one still to hold ✕
    app.fake_seat(1001, "Mia", 2, 3);
    app.fake_seat(1002, "Dad", 0, 0);
    app.fake_seat(1003, "Kai", 3, 4);
    shot("asking-seats", App::Screen::joining, r);
    // in game: chat lines over the picture, the Room menu
    r.guest = GuestView{};
    r.guest.phase = GuestView::joined;
    r.picture = true;
    rooms::RoomState &rs = r.guest.room;
    rs.known = true;
    std::snprintf(rs.name, sizeof rs.name, "Couch chaos");
    std::snprintf(rs.chat, sizeof rs.chat, "text");
    std::snprintf(rs.voice, sizeof rs.voice, "ptt");
    rs.you_rtt = 31, rs.link_rtt = 2;
    std::snprintf(rs.you_tier, sizeof rs.you_tier, "good");
    std::snprintf(rs.link_tier, sizeof rs.link_tier, "excellent");
    rs.n_players = 3;
    std::snprintf(rs.players[0].name, sizeof rs.players[0].name, "Sam");
    rs.players[0].id = 1, rs.players[0].speaking = true;
    std::snprintf(rs.players[1].name, sizeof rs.players[1].name, "Tester");
    rs.players[1].id = 2, rs.players[1].you = true;
    std::snprintf(rs.players[2].name, sizeof rs.players[2].name, "Mia");
    rs.players[2].id = 3;
    r.guest.chat_unread = 2;
    r.chat.add("Mia", 2, "", 990000, false);
    r.chat.add("Sam", 0, "nice jump! see you at example.com", 995000, false);
    r.mic_on = true;
    shot("playing-chat", App::Screen::playing, r);
    r.overlay = true;
    shot("playing-room-menu", App::Screen::playing, r);
    r.overlay = false;
    // the host's ready check: the banner over the game, then "I'm ready" first in the Room menu
    rs.ready_left_s = 24, rs.ready_asked = true, rs.ready_rev = 1;
    shot("playing-ready", App::Screen::playing, r);
    r.overlay = true;
    shot("playing-ready-menu", App::Screen::playing, r);
    r.overlay = false;
    rs.ready_left_s = -1, rs.ready_asked = false;
    r.mic_on = false;
    // the host's My room: a public room, an ask from Rooms with the password right, two players
    App::Fake h = f;
    h.helper_present = 1;
    h.pin = helper::Pin{};
    CompanionDoc &hd = h.host.doc;
    hd.room.known = true;
    std::snprintf(hd.room.name, sizeof hd.room.name, "Sam's room");
    std::snprintf(hd.room.vis, sizeof hd.room.vis, "public");
    std::snprintf(hd.room.chat, sizeof hd.room.chat, "quick");
    std::snprintf(hd.room.voice, sizeof hd.room.voice, "ptt");
    std::snprintf(hd.room.listed, sizeof hd.room.listed, "listed");
    std::snprintf(hd.room.game, sizeof hd.room.game, "ASTRO BOT");
    std::snprintf(hd.room.link_tier, sizeof hd.room.link_tier, "excellent");
    hd.room.link_rtt = 2;
    hd.room.size = 4;
    hd.room.password = true;
    std::snprintf(hd.asks[0].name, sizeof hd.asks[0].name, "Jo");
    std::snprintf(hd.asks[0].via, sizeof hd.asks[0].via, "rooms");
    std::snprintf(hd.asks[0].tag, sizeof hd.asks[0].tag, "9C11");
    hd.asks[0].password_ok = 1;
    hd.asks[0].rtt_hint = 70;
    hd.n_guests = 2;
    hd.guests[0].id = 3;
    hd.guests[1] = hd.guests[0];
    hd.guests[1].id = 4;
    hd.guests[1].player = 3;
    hd.guests[1].rtt_ms = 74;
    std::snprintf(hd.guests[1].name, sizeof hd.guests[1].name, "Lee");
    std::snprintf(hd.guests[1].tag, sizeof hd.guests[1].tag, "4F2A");
    std::snprintf(hd.guests[1].tier, sizeof hd.guests[1].tier, "playable");
    std::snprintf(hd.guests[1].route, sizeof hd.guests[1].route, "internet");
    hd.guests[1].from_rooms = true;
    shot("host-room", App::Screen::host, h);
    // an ask from the host's own device ("It's me") and a ready check running
    {
        App::Fake m = h;
        CompanionDoc &md = m.host.doc;
        md.n_asks = 1;
        md.asks[0] = CompanionDoc::Ask{};
        md.asks[0].id = 9;
        std::snprintf(md.asks[0].name, sizeof md.asks[0].name, "Sam");
        md.asks[0].own = true;
        md.room.ready_left_s = 18, md.room.ready_asked = 2, md.room.ready_n = 1;
        md.guests[1].ready = true;
        shot("host-room-me-ready", App::Screen::host, m);
        // Lee's menu: mutes, Give Player 1 (a friend on the stream's controller is on), Player 4
        m.host.doc.stream_seat = true;
        m.host.doc.guests[1].from_rooms = false;
        app.fake(App::Screen::menu, m);
        app.fake_player_menu(4);
        shot("host-player-menu", App::Screen::menu, m);
    }
    app.fake_page(0, 2);
    shot("host-this-ps5", App::Screen::host, h);
    app.fake_page(0, 0);
    app.fake(App::Screen::host, h);
    app.fake_confirm_close();
    shot("host-close", App::Screen::confirm, h);
    return 0;
}
