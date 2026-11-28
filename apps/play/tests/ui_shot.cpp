// PS5Upload Play - render every screen on a computer (BMP files) to check the layout:
//   ui-shot ASSETS_DIR OUT_DIR
// ASSETS_DIR holds font.bin, ui-bg.rgba and logo.rgba from the app build.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "../src/app/app.hpp"
#include "../src/app/platform.hpp"
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
    return 0;
}
