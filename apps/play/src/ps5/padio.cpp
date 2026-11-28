// PS5Upload Play - the controller: one thread reads it every 4 ms. It hands button presses to
// the menus, and while joined sends the pad to the host (with the held chords: the host's PS
// button and leaving), and applies the host's rumble and light bar.
// Phase 0: samples come with bit 0x80000000 set and everything neutral while the system has the
// pad (home screen, control center): nothing leaks to the app then.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "padio.hpp"

#include "../app/log.hpp"
#include "../core/chord.hpp"
#include "../net/net.hpp"
#include "sys.hpp"

#include <atomic>
#include <cstring>
#include <pthread.h>

namespace play::ps5::padio
{
namespace
{
namespace pb = proto::pad;
int handle = -1;
unsigned touch_w = 1920, touch_h = 1080;
pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
std::uint32_t held = 0, pressed = 0;
bool in_front = true;
std::atomic<Guest *> guest{nullptr};
// rumble / light from the host, applied on the pad thread
std::atomic<std::uint32_t> want_rumble{0}, want_light{0};
std::atomic<unsigned> rumble_serial{0}, light_serial{0};
platform::Chords chord_view;
std::atomic<unsigned> frames_sent{0};

std::uint32_t sticks_as_dpad(const PadSample &s) noexcept
{
    std::uint32_t b = 0;
    if (s.lx < 64)
        b |= pb::kLeft;
    if (s.lx > 192)
        b |= pb::kRight;
    if (s.ly < 64)
        b |= pb::kUp;
    if (s.ly > 192)
        b |= pb::kDown;
    return b;
}

proto::PadState to_state(const PadSample &s, std::uint32_t buttons) noexcept
{
    proto::PadState p;
    p.buttons = buttons;
    p.lx = s.lx, p.ly = s.ly, p.rx = s.rx, p.ry = s.ry, p.l2 = s.l2, p.r2 = s.r2;
    const int n = s.touch_count > 2 ? 2 : s.touch_count;
    p.touches = static_cast<std::uint8_t>(n);
    for (int i = 0; i < n; ++i)
    {
        // the engine takes 1920 x 1080 whatever the touchpad's own resolution
        const unsigned x = static_cast<unsigned>(s.touch[i].x) * 1920u / touch_w;
        const unsigned y = static_cast<unsigned>(s.touch[i].y) * 1080u / touch_h;
        p.touch[i] = {s.touch[i].id, static_cast<std::uint16_t>(x > 1919 ? 1919 : x),
                      static_cast<std::uint16_t>(y > 1079 ? 1079 : y)};
    }
    p.has_motion = true;
    for (int k = 0; k < 3; ++k)
    {
        p.gyro[k] = s.gyro[k];
        p.accel[k] = s.accel[k];
    }
    return p;
}

void *thread_main(void *) noexcept
{
    Chords chords;
    proto::PadState last_sent;
    std::uint64_t last_send = 0, last_motion = 0;
    bool sent_neutral = false;
    unsigned seen_rumble = 0, seen_light = 0;
    PadSample s[16];
    PadSample cur{};
    cur.lx = cur.ly = cur.rx = cur.ry = 128;
    for (;;)
    {
        const int n = handle >= 0 ? scePadRead(handle, s, 16) : 0;
        const std::uint64_t now = net::now_us();
        std::uint32_t went_down = 0;
        for (int i = 0; i < n; ++i)
        {
            const std::uint32_t was = (cur.buttons & ~pb::kIntercepted) | sticks_as_dpad(cur);
            cur = s[i];
            const std::uint32_t is = (cur.buttons & ~pb::kIntercepted) | sticks_as_dpad(cur);
            if (!(cur.buttons & pb::kIntercepted))
                went_down |= is & ~was;
        }
        const bool front = !(cur.buttons & pb::kIntercepted);
        pthread_mutex_lock(&lock);
        held = front ? (cur.buttons | sticks_as_dpad(cur)) & ~pb::kIntercepted : 0;
        pressed |= went_down;
        in_front = front;
        pthread_mutex_unlock(&lock);

        // Rumble and light from the host.
        if (handle >= 0)
        {
            const unsigned rs = rumble_serial.load();
            if (rs != seen_rumble)
            {
                seen_rumble = rs;
                const std::uint32_t v = want_rumble.load();
                const std::uint8_t m[2] = {static_cast<std::uint8_t>(v >> 8), static_cast<std::uint8_t>(v)};
                scePadSetVibration(handle, m);
            }
            const unsigned ls = light_serial.load();
            if (ls != seen_light)
            {
                seen_light = ls;
                const std::uint32_t v = want_light.load();
                if (v & 0x1000000u)
                {
                    const std::uint8_t c[4] = {static_cast<std::uint8_t>(v >> 16), static_cast<std::uint8_t>(v >> 8),
                                               static_cast<std::uint8_t>(v), 0};
                    scePadSetLightBar(handle, c);
                }
                else
                    scePadResetLightBar(handle);
            }
        }

        // While joined: the pad to the host.
        Guest *g = guest.load();
        if (g)
        {
            if (!front)
            {
                // The system has the pad: the host sees "nothing pressed" once.
                if (!sent_neutral && g->send_pad(proto::PadState{}))
                    sent_neutral = true;
                last_sent = proto::PadState{};
                pthread_mutex_lock(&lock);
                chord_view.ps = chord_view.leave = 0;
                pthread_mutex_unlock(&lock);
            }
            else
            {
                sent_neutral = false;
                const auto out = chords.update(cur.buttons & ~pb::kIntercepted, now);
                pthread_mutex_lock(&lock);
                chord_view.ps = out.ps_progress;
                chord_view.leave = out.leave_progress;
                chord_view.ps_sent = chord_view.ps_sent || out.ps_sent;
                chord_view.leave_now = chord_view.leave_now || out.leave;
                pthread_mutex_unlock(&lock);
                const proto::PadState st = to_state(cur, out.buttons);
                const bool changed = !proto::same_input(st, last_sent);
                // Buttons, sticks and touch at once; motion alone 125 times a second; and a
                // heartbeat every 100 ms.
                if (changed || now - last_motion >= 8000 || now - last_send >= 100000)
                {
                    if (g->send_pad(st))
                        frames_sent.fetch_add(1);
                    last_sent = st;
                    last_send = last_motion = now;
                }
            }
        }
        sceKernelUsleep(4000);
    }
    return nullptr;
}
} // namespace

bool open(int user) noexcept
{
    const int rc = scePadInit();
    handle = scePadOpen(user, 0, 0, nullptr);
    log("pad: init %#x, open %#x", rc, handle);
    if (handle < 0)
        return false;
    scePadSetVibrationMode(handle, 2);
    log("pad: motion sensors %#x", scePadSetMotionSensorState(handle, true));
    std::uint8_t info[64] = {};
    if (scePadGetControllerInformation(handle, info) == 0)
    {
        const unsigned w = static_cast<unsigned>(info[4] | info[5] << 8), h = static_cast<unsigned>(info[6] | info[7] << 8);
        if (w >= 100 && w <= 4096 && h >= 100 && h <= 4096)
            touch_w = w, touch_h = h;
    }
    log("pad: touchpad %ux%u", touch_w, touch_h);
    pthread_t t;
    return pthread_create(&t, nullptr, thread_main, nullptr) == 0;
}

platform::Input input() noexcept
{
    platform::Input in;
    pthread_mutex_lock(&lock);
    in.held = held;
    in.pressed = pressed;
    in.in_front = in_front;
    pressed = 0;
    pthread_mutex_unlock(&lock);
    return in;
}

void stream_to(Guest *g) noexcept
{
    guest.store(g);
    if (!g)
    {
        rumble(0, 0);
        light_reset();
    }
}

platform::Chords chords() noexcept
{
    pthread_mutex_lock(&lock);
    platform::Chords c = chord_view;
    chord_view.ps_sent = false;
    chord_view.leave_now = false;
    pthread_mutex_unlock(&lock);
    return c;
}

void rumble(std::uint8_t left, std::uint8_t right) noexcept
{
    want_rumble.store(static_cast<std::uint32_t>(left) << 8 | right);
    rumble_serial.fetch_add(1);
}

void light(std::uint8_t r, std::uint8_t g, std::uint8_t b) noexcept
{
    want_light.store(0x1000000u | static_cast<std::uint32_t>(r) << 16 | static_cast<std::uint32_t>(g) << 8 | b);
    light_serial.fetch_add(1);
}

void light_reset() noexcept
{
    want_light.store(0);
    light_serial.fetch_add(1);
}

unsigned sent() noexcept
{
    return frames_sent.load();
}
} // namespace play::ps5::padio
