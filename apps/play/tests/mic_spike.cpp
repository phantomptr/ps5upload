// PS5Upload Play - microphone spike (developer test, not part of the app; docs/MIC.md).
// Not built by default. To run it: copy it to src/ps5/, add -DPLAY_MIC_SPIKE to `common` in
// tools/build-in-container.sh, and in src/ps5/main.cpp call play::ps5::mic_spike_early(user_id)
// right after padio::open (before the app's own sound starts, so "no output at all" can be
// measured) and play::ps5::mic_spike_start() after net::init. Build it under a test title id.
// Everything goes to the app's log.
// SPDX-License-Identifier: GPL-3.0-or-later
#ifdef PLAY_MIC_SPIKE
#include "../app/log.hpp"
#include "../net/net.hpp"
#include "mic.hpp"
#include "sys.hpp"

#include <atomic>
#include <cstdio>
#include <cstring>
#include <ctime>
#include <pthread.h>

extern "C"
{
    int sceKernelGetModuleList(std::int32_t *handles, std::size_t count, std::size_t *actual);
    int sceKernelGetModuleInfo(std::int32_t handle, void *info);
    int sceUserServiceGetForegroundUser(std::int32_t *user);
    int sceAudioOutGetPortState(std::int32_t handle, void *state);
    int sceAudioOutClose(std::int32_t handle);
    int sceAudioInGetHandleStatusInfo(std::int32_t handle, void *info);
}

namespace play::ps5
{
namespace
{
int g_user = -1, g_fg = -1;

std::uint64_t cpu_us() noexcept
{
    timespec ts{};
    clock_gettime(14 /* CLOCK_THREAD_CPUTIME_ID */, &ts);
    return static_cast<std::uint64_t>(ts.tv_sec) * 1000000u + static_cast<std::uint64_t>(ts.tv_nsec) / 1000u;
}

double sin_approx(double x) noexcept // x in [-pi, pi]
{
    const double x2 = x * x;
    return x * (1 - x2 / 6 * (1 - x2 / 20 * (1 - x2 / 42 * (1 - x2 / 72 * (1 - x2 / 110)))));
}
double cos_approx(double x) noexcept
{
    constexpr double pi = 3.14159265358979323846;
    double y = x + pi / 2;
    if (y > pi)
        y -= 2 * pi;
    return sin_approx(y);
}

// ---- a 1 kHz test tone on our own audio-out port (48 kHz mono) -------------------------------
std::int16_t tone_table[48];
std::atomic<bool> tone_on{false}, tone_run{false};
int tone_port = -1;

void tone_init() noexcept
{
    constexpr double pi = 3.14159265358979323846;
    for (int i = 0; i < 48; ++i)
    {
        double x = 2 * pi * i / 48.0;
        if (x > pi)
            x -= 2 * pi;
        tone_table[i] = static_cast<std::int16_t>(sin_approx(x) * 0.25 * 32767); // -12 dBFS
    }
}

int tone_pos = 0;
void tone_block(std::int16_t *b) noexcept
{
    const bool on = tone_on.load();
    for (int i = 0; i < 256; ++i)
    {
        b[i] = on ? tone_table[tone_pos] : 0;
        tone_pos = (tone_pos + 1) % 48;
    }
}

void *tone_thread(void *) noexcept
{
    alignas(16) static std::int16_t b[256];
    while (tone_run.load())
    {
        tone_block(b);
        const int rc = sceAudioOutOutput(tone_port, b);
        if (rc < 0)
        {
            log("mic-spike: tone output %#x", rc);
            sceKernelUsleep(5000);
        }
    }
    return nullptr;
}

bool tone_open() noexcept
{
    if (tone_port >= 0)
        return true;
    tone_port = sceAudioOutOpen(0xff, 0, 0, 256, 48000, 0 /* S16 mono */);
    log("mic-spike: tone port sceAudioOutOpen(0xff, main, 256, 48000, mono) %#x", tone_port);
    if (tone_port < 0)
        return false;
    std::uint8_t st[64] = {};
    const int rc = sceAudioOutGetPortState(tone_port, st);
    log("mic-spike: sceAudioOutGetPortState %#x output %#x channel %u volume %d reroute %u flag %#llx", rc,
        st[0] | st[1] << 8, st[2], static_cast<std::int16_t>(st[4] | st[5] << 8), st[6] | st[7] << 8,
        static_cast<unsigned long long>(st[8] | st[9] << 8 | st[10] << 16));
    return true;
}

pthread_t tone_tid;
void tone_start() noexcept
{
    if (!tone_open())
        return;
    tone_run = true;
    pthread_create(&tone_tid, nullptr, tone_thread, nullptr);
}
void tone_stop() noexcept
{
    tone_run = false;
    pthread_join(tone_tid, nullptr);
}

// ---- measuring one second of capture ---------------------------------------------------------
struct Window
{
    unsigned reads = 0, errors = 0, zero_blocks = 0;
    std::uint64_t samples = 0;
    double sum2 = 0, gp = 0; // energy, Goertzel power at 1 kHz
    int peak = 0;
    std::uint64_t read_us = 0, worst_us = 0;
    int last_err = 0;
};

void measure(Window &w, const std::int16_t *b, unsigned n, double coeff) noexcept
{
    double s1 = 0, s2 = 0;
    bool zero = true;
    for (unsigned i = 0; i < n; ++i)
    {
        const int v = b[i];
        if (v)
            zero = false;
        const int a = v < 0 ? -v : v;
        if (a > w.peak)
            w.peak = a;
        w.sum2 += static_cast<double>(v) * v;
        const double s0 = v + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    // amplitude of the 1 kHz part of this block: 2*sqrt(power)/n
    const double p = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    w.gp += p / (static_cast<double>(n) * n) * 4; // amplitude^2
    w.samples += n;
    if (zero)
        ++w.zero_blocks;
}

void report(const char *tag, int sec, const Window &w, unsigned rate, int silent_bits, std::uint64_t cpu,
            std::uint64_t wall) noexcept
{
    const double rms = w.samples ? __builtin_sqrt(w.sum2 / static_cast<double>(w.samples)) : 0;
    const double tone = w.reads ? __builtin_sqrt(w.gp / w.reads) : 0;
    log("mic-spike: %s t=%d reads %u samples %llu (%.1f%% of real time) rms %.1f peak %d tone1k %.1f zero-blocks %u "
        "errors %u (last %#x) read avg %.2f ms worst %.2f ms silent %#x cpu %.2f%% tone %s",
        tag, sec, w.reads, static_cast<unsigned long long>(w.samples),
        wall ? 100.0 * static_cast<double>(w.samples) / (static_cast<double>(rate) * static_cast<double>(wall) / 1e6) : 0.0,
        rms, w.peak, tone, w.zero_blocks, w.errors, static_cast<unsigned>(w.last_err),
        w.reads ? static_cast<double>(w.read_us) / w.reads / 1000.0 : 0.0, static_cast<double>(w.worst_us) / 1000.0,
        static_cast<unsigned>(silent_bits), wall ? 100.0 * static_cast<double>(cpu) / static_cast<double>(wall) : 0.0,
        tone_on.load() ? "on" : "off");
}

enum class Out
{
    none,     // nothing else outputs (or whatever the app plays)
    separate, // the tone thread plays on its own pace
    lockstep, // this thread plays one 256-sample block per captured grain
};

// Captures for `seconds`, tone on during the middle third when `tone` is set.
void capture_run(const char *tag, int user, const mic::Format &f, int seconds, Out out, bool tone) noexcept
{
    const std::uint64_t o0 = net::now_us();
    const int h = mic::open(user, f);
    const std::uint64_t o1 = net::now_us();
    log("mic-spike: %s open(user %#x, type %d, %u Hz, grain %u, %s%s) %#x in %.1f ms", tag, user, f.type, f.rate,
        f.grain, f.stereo ? "stereo" : "mono", f.hq ? ", hq" : "", static_cast<unsigned>(h),
        static_cast<double>(o1 - o0) / 1000.0);
    if (h < 0)
        return;
    std::uint8_t info[256] = {};
    const int irc = sceAudioInGetHandleStatusInfo(h, info);
    char hex[200] = {};
    for (int i = 0; i < 48; ++i)
        std::snprintf(hex + i * 3, 4, "%02x ", info[i]);
    log("mic-spike: %s sceAudioInGetHandleStatusInfo %#x: %s", tag, static_cast<unsigned>(irc), hex);
    constexpr double pi = 3.14159265358979323846;
    const double coeff = 2 * cos_approx(2 * pi * 1000.0 / f.rate);
    const unsigned n = f.grain * (f.stereo ? 2 : 1);
    alignas(16) static std::int16_t buf[4096];
    alignas(16) static std::int16_t ob[256];
    if (out == Out::separate)
        tone_start();
    if (out == Out::lockstep && !tone_open())
        out = Out::none;
    const std::uint64_t t0 = net::now_us();
    std::uint64_t first_data_us = 0;
    for (int sec = 0; sec < seconds; ++sec)
    {
        if (tone)
            tone_on = sec >= seconds / 3 && sec < 2 * seconds / 3;
        Window w;
        const std::uint64_t w0 = net::now_us(), c0 = cpu_us();
        const std::uint64_t end = t0 + static_cast<std::uint64_t>(sec + 1) * 1000000u;
        while (net::now_us() < end)
        {
            if (out == Out::lockstep)
            {
                tone_block(ob);
                const int orc = sceAudioOutOutput(tone_port, ob);
                if (orc < 0)
                    w.last_err = orc;
            }
            const std::uint64_t r0 = net::now_us();
            const int rc = mic::read(h, buf);
            const std::uint64_t dt = net::now_us() - r0;
            ++w.reads;
            w.read_us += dt;
            if (dt > w.worst_us)
                w.worst_us = dt;
            if (rc < 0)
            {
                ++w.errors;
                w.last_err = rc;
                sceKernelUsleep(f.grain * 1000000u / f.rate);
                continue;
            }
            const unsigned got = rc > 0 && static_cast<unsigned>(rc) * (f.stereo ? 2u : 1u) <= n
                                     ? static_cast<unsigned>(rc) * (f.stereo ? 2u : 1u)
                                     : n;
            if (sec == 0 && w.reads <= 2)
                log("mic-spike: %s read %u returned %d after %.2f ms", tag, w.reads, rc, static_cast<double>(dt) / 1000.0);
            measure(w, buf, got, coeff);
            if (!first_data_us)
                for (unsigned i = 0; i < got; ++i)
                    if (buf[i])
                    {
                        first_data_us = net::now_us() - t0;
                        break;
                    }
        }
        report(tag, sec, w, f.rate * (f.stereo ? 2 : 1), mic::silent(h), cpu_us() - c0, net::now_us() - w0);
    }
    tone_on = false;
    if (out == Out::separate)
        tone_stop();
    log("mic-spike: %s first non-zero sample after %.1f ms; close %#x", tag, static_cast<double>(first_data_us) / 1000.0,
        static_cast<unsigned>(mic::close(h)));
}

void module_list() noexcept
{
    std::int32_t handles[256];
    std::size_t n = 0;
    const int rc = sceKernelGetModuleList(handles, 256, &n);
    char line[900] = {};
    std::size_t at = 0;
    for (std::size_t i = 0; i < n && i < 256; ++i)
    {
        alignas(8) std::uint8_t info[0x160 + 64] = {};
        *reinterpret_cast<std::uint64_t *>(info) = 0x160;
        if (sceKernelGetModuleInfo(handles[i], info) != 0)
            continue;
        const char *name = reinterpret_cast<const char *>(info + 8);
        const int w = std::snprintf(line + at, sizeof line - at, "%s ", name);
        if (w > 0 && at + static_cast<std::size_t>(w) < sizeof line - 1)
            at += static_cast<std::size_t>(w);
        if (at > 700)
        {
            log("mic-spike: modules: %s", line);
            at = 0;
            line[0] = 0;
        }
    }
    log("mic-spike: modules (%zu, rc %#x): %s", n, static_cast<unsigned>(rc), line);
}

// Which opens the system takes: one line per user, function and type.
void open_matrix() noexcept
{
    const int users[] = {g_user, g_fg, 0xff};
    const unsigned rates[] = {16000, 48000};
    const unsigned grains[] = {256, 128, 512, 480, 1024};
    for (int ui = 0; ui < 3; ++ui)
    {
        if (ui == 1 && g_fg == g_user)
            continue;
        for (int hq = 0; hq < 2; ++hq)
            for (int type = 0; type < 8; ++type)
            {
                char line[700] = {};
                std::size_t at = 0;
                for (unsigned rate : rates)
                    for (unsigned grain : grains)
                        for (int st = 0; st < 2; ++st)
                        {
                            mic::Format f;
                            f.type = type;
                            f.rate = rate;
                            f.grain = grain;
                            f.stereo = st;
                            f.hq = hq;
                            const int h = mic::open(users[ui], f);
                            int s = 0;
                            if (h >= 0)
                            {
                                s = mic::silent(h);
                                mic::close(h);
                            }
                            const int w = std::snprintf(line + at, sizeof line - at, "%u/%u%s=%x%s ", rate / 1000, grain,
                                                        st ? "s" : "m", static_cast<unsigned>(h),
                                                        h >= 0 ? (s ? "!" : "") : "");
                            if (w > 0)
                                at += static_cast<std::size_t>(w);
                        }
                log("mic-spike: matrix user %#x %s type %d: %s", users[ui], hq ? "HqOpen" : "Open", type, line);
            }
    }
}

void *late_thread(void *) noexcept
{
    sceKernelUsleep(3000000);
    mic::Format vc48;
    vc48.rate = 48000;
    // with the app's own sound thread running (menu music), separate tone thread
    capture_run("app-out+tone vc48", g_user, vc48, 15, Out::separate, true);
    // two ports at once?
    mic::Format g16;
    g16.type = mic::kGeneral;
    g16.rate = 16000;
    const int a = mic::open(g_user, vc48);
    const int b = mic::open(g_user, g16);
    log("mic-spike: two ports at once: vc48 %#x general16 %#x", static_cast<unsigned>(a), static_cast<unsigned>(b));
    if (a >= 0)
        mic::close(a);
    if (b >= 0)
        mic::close(b);
    // then keep one port open and report every 5 s (home screen / suspend tests)
    const int h = mic::open(g_user, vc48);
    log("mic-spike: monitor open %#x", static_cast<unsigned>(h));
    if (h < 0)
        return nullptr;
    alignas(16) static std::int16_t buf[256];
    constexpr double pi = 3.14159265358979323846;
    const double coeff = 2 * cos_approx(2 * pi * 1000.0 / 48000);
    tone_start();
    for (int sec = 0;; sec += 5)
    {
        tone_on = (sec / 5) % 2 == 1;
        Window w;
        const std::uint64_t w0 = net::now_us(), c0 = cpu_us();
        while (net::now_us() < w0 + 5000000)
        {
            const std::uint64_t r0 = net::now_us();
            const int rc = mic::read(h, buf);
            const std::uint64_t dt = net::now_us() - r0;
            ++w.reads;
            w.read_us += dt;
            if (dt > w.worst_us)
                w.worst_us = dt;
            if (rc < 0)
            {
                ++w.errors;
                w.last_err = rc;
                sceKernelUsleep(5000);
                continue;
            }
            measure(w, buf, 256, coeff);
        }
        report("monitor", sec, w, 48000, mic::silent(h), cpu_us() - c0, net::now_us() - w0);
    }
    return nullptr;
}
} // namespace

void mic_spike_early(int user) noexcept
{
    g_user = user;
    const int frc = sceUserServiceGetForegroundUser(&g_fg);
    log("mic-spike: initial user %#x, foreground user %#x (rc %#x)", static_cast<unsigned>(g_user),
        static_cast<unsigned>(g_fg), static_cast<unsigned>(frc));
    tone_init();
    module_list();
    log("mic-spike: sceAudioInInit %#x", static_cast<unsigned>(mic::init()));
    log("mic-spike: sceAudioOutInit %#x", static_cast<unsigned>(sceAudioOutInit()));
    open_matrix();
    // No audio output anywhere in the app yet.
    mic::Format f;
    f.rate = 16000;
    capture_run("alone vc16", g_user, f, 5, Out::none, false);
    f.rate = 48000;
    capture_run("alone vc48", g_user, f, 5, Out::none, false);
    f.type = mic::kGeneral;
    capture_run("alone gen48", g_user, f, 5, Out::none, false);
    f.type = mic::kVoiceChat;
    f.hq = true;
    capture_run("alone hq-vc48", g_user, f, 5, Out::none, false);
    f.hq = false;
    // Output on its own thread and pace, then output in step on the capture thread.
    capture_run("separate vc48", g_user, f, 9, Out::separate, true);
    capture_run("lockstep vc48", g_user, f, 9, Out::lockstep, true);
    f.rate = 16000;
    capture_run("separate vc16", g_user, f, 9, Out::separate, true);
    f.type = mic::kGeneral;
    f.rate = 48000;
    capture_run("separate gen48", g_user, f, 9, Out::separate, true);
    log("mic-spike: early part done");
}

void mic_spike_start() noexcept
{
    pthread_t t;
    pthread_create(&t, nullptr, late_thread, nullptr);
}
} // namespace play::ps5
#endif
