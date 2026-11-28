// PS5Upload Play - the console app: start-up, the platform layer the app uses, the main loop.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "../app/app.hpp"
#include "../app/log.hpp"
#include "../app/platform.hpp"
#include "../net/net.hpp"
#include "../ui/canvas.hpp"
#include "display.hpp"
#include "padio.hpp"
#include "sound.hpp"
#include "sys.hpp"
#include "video.hpp"
#include "voice.hpp"

#include <cstdio>
#include <cstring>
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

#ifndef PLAY_VERSION
#define PLAY_VERSION "0.0.0"
#endif

// A pure virtual call can't happen here; the vtables still name this.
extern "C" void __cxa_pure_virtual()
{
    __builtin_trap();
}

namespace
{
using namespace play;
constexpr const char *kDir = "/data/ps5upload-play";
int user_id = -1;
char user_name_buf[64] = {};
std::uint8_t *msg_buf[2] = {};
constexpr std::size_t kMsgCap = 8u << 20;

class Sinks final : public GuestSinks
{
  public:
    void begin(bool hevc) override
    {
        ps5::video::begin(hevc);
        ps5::sound::stream_begin();
    }
    void end() override
    {
        ps5::video::end();
        ps5::sound::stream_end();
        ps5::padio::rumble(0, 0);
    }
    void video_header(const std::uint8_t *d, std::size_t n) override
    {
        ps5::video::header(d, n);
    }
    void video(const std::uint8_t *d, std::size_t n, std::uint64_t) override
    {
        ps5::video::access_unit(d, n);
    }
    void audio_header(const std::uint8_t *d, std::size_t n) override
    {
        ps5::sound::stream_header(d, n);
    }
    void audio(std::uint16_t index, const std::uint8_t *d, std::size_t n) override
    {
        ps5::sound::stream_packet(index, d, n);
    }
    void rumble(std::uint8_t l, std::uint8_t r) override
    {
        ps5::padio::rumble(l, r);
    }
    void light(std::uint8_t r, std::uint8_t g, std::uint8_t b) override
    {
        ps5::padio::light(r, g, b);
    }
    bool wants_keyframe() override
    {
        return ps5::video::wants_keyframe();
    }
    void voice(std::uint8_t speaker, std::uint16_t seq, const std::uint8_t *d, std::size_t n) override
    {
        ps5::voice_io::packet(speaker, seq, d, n);
    }
    void stats(char *out, std::size_t cap) override
    {
        const auto v = ps5::video::stats();
        const auto s = ps5::sound::stats();
        std::snprintf(out, cap,
                      "\"frames\":%u,\"dropped\":%u,\"late\":%u,\"decode_ms\":%.1f,\"key_waits\":%u,"
                      "\"decoder\":\"native\",\"codec\":\"%s\",\"snd\":\"native\",\"snd_decoded\":%u,"
                      "\"snd_concealed\":%u,\"snd_underruns\":%u,\"snd_dropped\":%u",
                      v.shown, v.dropped, v.skipped, static_cast<double>(v.decode_ms), v.key_waits,
                      v.hevc ? (v.ten_bit ? "hevc.main10" : "hevc") : "h264", s.decoded, s.concealed + s.recovered, s.underruns, s.dropped);
    }
};
Sinks sinks_instance;

// A seat of its own for another controller on this PS5: no picture or sound, its rumble and light
// to that user's controller.
class SeatSinks final : public GuestSinks
{
  public:
    int user = -1;
    void begin(bool) override {}
    void end() override
    {
        ps5::padio::extra_rumble(user, 0, 0);
    }
    void video_header(const std::uint8_t *, std::size_t) override {}
    void video(const std::uint8_t *, std::size_t, std::uint64_t) override {}
    void audio_header(const std::uint8_t *, std::size_t) override {}
    void audio(std::uint16_t, const std::uint8_t *, std::size_t) override {}
    void rumble(std::uint8_t l, std::uint8_t r) override
    {
        ps5::padio::extra_rumble(user, l, r);
    }
    void light(std::uint8_t r, std::uint8_t g, std::uint8_t b) override
    {
        ps5::padio::extra_light(user, r, g, b);
    }
    bool wants_keyframe() override
    {
        return false;
    }
    void stats(char *out, std::size_t cap) override
    {
        std::snprintf(out, cap, "\"frames\":0,\"decoder\":\"none\"");
    }
};

std::uint8_t *load_asset(const char *path, std::size_t *len) noexcept
{
    *len = 0;
    const int fd = open(path, O_RDONLY);
    if (fd < 0)
        return nullptr;
    struct stat st{};
    if (fstat(fd, &st) != 0 || st.st_size <= 0)
    {
        close(fd);
        return nullptr;
    }
    std::uint8_t *d = ps5::big_alloc(static_cast<std::size_t>(st.st_size));
    std::size_t got = 0;
    while (d && got < static_cast<std::size_t>(st.st_size))
    {
        const long r = read(fd, d + got, static_cast<std::size_t>(st.st_size) - got);
        if (r <= 0)
            break;
        got += static_cast<std::size_t>(r);
    }
    close(fd);
    *len = got;
    return d;
}
} // namespace

namespace play::platform
{
Input input() noexcept
{
    return ps5::padio::input();
}
void stream_pad(Guest *guest) noexcept
{
    ps5::padio::stream_to(guest);
    ps5::voice_io::set_guest(guest);
}
Chords chords() noexcept
{
    return ps5::padio::chords();
}
Chords chords_peek() noexcept
{
    return ps5::padio::chords_peek();
}
void pad_to_menu(bool on) noexcept
{
    ps5::padio::to_menu(on);
}
void ptt_on_create(bool on) noexcept
{
    ps5::padio::ptt_on_create(on);
}
void voice_allow(const voice::Allow &a) noexcept
{
    ps5::voice_io::allow(a);
}
MicState mic_state() noexcept
{
    return ps5::voice_io::state();
}
void voice_volume(int speaker, int percent, bool muted) noexcept
{
    ps5::voice_io::volume(speaker, percent, muted);
}
void voice_duck(int level) noexcept
{
    ps5::voice_io::duck(level);
}
std::uint32_t voice_speaking() noexcept
{
    return ps5::voice_io::speaking();
}
int local_pads(LocalPad *out) noexcept
{
    ps5::padio::Extra e[ps5::padio::kExtra];
    const int n = ps5::padio::extras(e);
    for (int i = 0; i < kLocalPads && i < ps5::padio::kExtra; ++i)
    {
        out[i] = LocalPad{};
        out[i].user = e[i].user;
        std::snprintf(out[i].name, sizeof out[i].name, "%s", e[i].name);
        out[i].connected = e[i].connected;
        out[i].cross = e[i].cross;
        out[i].in_front = e[i].in_front;
    }
    return n;
}
void attach_pad(int user, Guest *g) noexcept
{
    ps5::padio::attach(user, g);
}
bool pad_leave(int user) noexcept
{
    return ps5::padio::extra_leave(user);
}
GuestSinks &seat_sinks(int place, int user) noexcept
{
    static SeatSinks s[kLocalPads];
    SeatSinks &x = s[place < 0 ? 0 : place % kLocalPads];
    x.user = user;
    return x;
}
std::uint8_t *seat_buffer(int place, std::size_t *cap) noexcept
{
    static std::uint8_t *b[kLocalPads] = {};
    const int i = place < 0 ? 0 : place % kLocalPads;
    if (!b[i])
        b[i] = ps5::big_alloc(256 * 1024);
    *cap = b[i] ? 256 * 1024 : 0;
    return b[i];
}
bool video_showing() noexcept
{
    // Short stalls keep the last picture up; after this long the app says it is waiting.
    return ps5::display::since_video_us() < 1500000;
}
void present(const std::uint32_t *ui) noexcept
{
    ps5::display::present_ui(ui);
}
void overlay(const std::uint32_t *layer, int top, int bottom) noexcept
{
    ps5::display::set_overlay(layer, top, bottom);
}
void blip(Blip b) noexcept
{
    ps5::sound::blip(static_cast<ps5::sound::Blip>(b));
}
void music(bool on, int volume) noexcept
{
    ps5::sound::music(on, volume);
}
void sounds(bool on) noexcept
{
    ps5::sound::sounds(on);
}
void power_tick() noexcept
{
    sceSystemServicePowerTick();
}
const char *user_name() noexcept
{
    return user_name_buf;
}
long read_file(const char *path, void *buf, std::size_t cap) noexcept
{
    const int fd = open(path, O_RDONLY);
    if (fd < 0)
        return -1;
    const long n = read(fd, buf, cap);
    close(fd);
    return n;
}
bool write_file(const char *path, const void *data, std::size_t len) noexcept
{
    char tmp[200];
    std::snprintf(tmp, sizeof tmp, "%s.new", path);
    const int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0)
        return false;
    const bool ok = write(fd, data, len) == static_cast<long>(len);
    fsync(fd);
    close(fd);
    return ok && rename(tmp, path) == 0;
}
const char *data_dir() noexcept
{
    return kDir;
}
const std::uint8_t *asset(const char *name, std::size_t *len) noexcept
{
    char path[160];
    std::snprintf(path, sizeof path, "/app0/assets/%s", name);
    return load_asset(path, len);
}
int system_language() noexcept
{
    int v = -1;
    // SCE_SYSTEM_SERVICE_PARAM_ID_LANG
    return sceSystemServiceParamGetInt(1, &v) == 0 ? v : -1;
}
void exit_app() noexcept
{
    log("app: exit");
    ps5::padio::stream_to(nullptr);
    sceKernelUsleep(50000);
    const int rc = sceSystemServiceLoadExec("exit", nullptr);
    log("app: sceSystemServiceLoadExec returned %#x", rc);
    for (;;)
        sceKernelUsleep(1000000);
}
GuestSinks &sinks() noexcept
{
    return sinks_instance;
}
std::uint8_t *message_buffer(int which, std::size_t *cap) noexcept
{
    *cap = kMsgCap;
    return msg_buf[which ? 1 : 0];
}
MediaStats media_stats() noexcept
{
    const auto v = ps5::video::stats();
    const auto s = ps5::sound::stats();
    MediaStats m;
    m.shown = v.shown;
    m.decoded = v.decoded;
    m.dropped = v.dropped;
    m.decode_errors = v.errors;
    m.decode_ms = v.decode_ms;
    m.snd_played = s.decoded;
    m.snd_concealed = s.concealed + s.recovered;
    m.snd_underruns = s.underruns;
    return m;
}
} // namespace play::platform

int main()
{
    // Open to the PS5Upload helper (another user): it removes this folder when the app is
    // uninstalled with its data. With 0755 it could not (Permission denied on the Pro); a secret
    // in it stays the app's own by its file mode (room.txt is 0600).
    mkdir(kDir, 0777);
    chmod(kDir, 0777);
    char path[160];
    std::snprintf(path, sizeof path, "%s/log.txt", kDir);
    log_open(path);
    log("==== PS5Upload Play %s (pid %d)", PLAY_VERSION, getpid());
    // The decoder library and its compute queue first, as phase 0 found works.
    const bool video_ok = ps5::video::init();
    std::size_t flex = 0;
    sceKernelAvailableFlexibleMemorySize(&flex);
    log("memory: direct %zu MiB, flexible free %zu MiB", sceKernelGetDirectMemorySize() >> 20, flex >> 20);
    sceUserServiceInitialize(nullptr);
    sceUserServiceGetInitialUser(&user_id);
    if (sceUserServiceGetUserName(user_id, user_name_buf, sizeof user_name_buf) != 0)
        user_name_buf[0] = 0;
    ps5::padio::open(user_id);
    ps5::voice_io::init(user_id); // the mic opens only when switched on in a room
    msg_buf[0] = ps5::big_alloc(kMsgCap);
    msg_buf[1] = ps5::big_alloc(kMsgCap);
    auto *ui_px = reinterpret_cast<std::uint32_t *>(ps5::big_alloc(ui::kW * ui::kH * 4));
    auto *layer = reinterpret_cast<std::uint32_t *>(ps5::big_alloc(ui::kW * ui::kH * 4));
    if (!ps5::display::open() || !ui_px || !layer || !msg_buf[0] || !msg_buf[1])
    {
        log("start-up failed (display or memory)");
        sceSystemServiceLoadExec("exit", nullptr);
        return 1;
    }
    std::size_t n = 0;
    std::uint8_t *font = load_asset("/app0/assets/font.bin", &n);
    if (!font || !ui::font_load(font, n))
        log("font: missing or unreadable");
    ui::Image bg, logo;
    if (std::uint8_t *d = load_asset("/app0/assets/ui-bg.rgba", &n))
        ui::image_from_rgba(d, n, bg);
    if (std::uint8_t *d = load_asset("/app0/assets/logo.rgba", &n))
        ui::image_from_rgba(d, n, logo);
    std::size_t music_len = 0;
    std::uint8_t *music = load_asset("/app0/assets/music.pcm", &music_len);
    ps5::sound::init(music, music_len);
    net::init();
    if (!video_ok)
        log("video: the decoder did not start; joining shows no picture");

    static App app(PLAY_VERSION, bg, logo);
    app.load_settings();
    // First picture, then the splash can go.
    app.frame(net::now_us(), ui_px, layer);
    log("splash: hide %#x", sceSystemServiceHideSplashScreen());

    struct stat st{};
    char dev[160], cmd[160];
    std::snprintf(dev, sizeof dev, "%s/dev", kDir);
    std::snprintf(cmd, sizeof cmd, "%s/cmd", kDir);
    const bool dev_mode = stat(dev, &st) == 0;
    if (dev_mode)
        log("dev: commands are read from %s", cmd);
    std::uint64_t next_cmd = 0, next_report = 0;
    for (;;)
    {
        const std::uint64_t now = net::now_us();
        app.frame(now, ui_px, layer);
        if (dev_mode && now >= next_cmd)
        {
            next_cmd = now + 500000;
            char line[160] = {};
            const int fd = open(cmd, O_RDONLY);
            if (fd >= 0)
            {
                const long r = read(fd, line, sizeof line - 1);
                close(fd);
                unlink(cmd);
                if (r > 0)
                {
                    for (long i = 0; i < r; ++i)
                        if (line[i] == '\n' || line[i] == '\r')
                            line[i] = 0;
                    app.command(line);
                }
            }
        }
        if (now >= next_report)
        {
            next_report = now + 10000000;
            const auto v = ps5::video::stats();
            const auto s = ps5::sound::stats();
            const auto d = ps5::display::stats();
            const auto r = ps5::padio::rumbles();
            if (v.decoded || s.decoded)
                log("stats: pictures decoded %u shown %u skipped %u dropped %u errors %u key waits %u, decode %.2f ms, "
                    "convert %.2f ms%s, flips %u (waits %u), gaps over 25 ms %u, worst %.1f ms; sound decoded %u "
                    "concealed %u recovered %u underruns %u dropped %u late %u; pad frames %u; rumble set %u times (%u on, last %#x)",
                    v.decoded, v.shown, v.skipped, v.dropped, v.errors, v.key_waits, static_cast<double>(v.decode_ms),
                    static_cast<double>(d.convert_ms), v.ten_bit ? " (10-bit HDR to SDR)" : "", d.flips, d.waits,
                    d.slow, static_cast<double>(d.worst_ms), s.decoded, s.concealed, s.recovered, s.underruns,
                    s.dropped, s.late, ps5::padio::sent(), r.calls, r.nonzero, static_cast<unsigned>(r.last_rc));
        }
        sceKernelUsleep(8000);
    }
}
