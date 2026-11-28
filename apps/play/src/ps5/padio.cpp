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
#include <cstdio>
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
std::atomic<bool> menu_has_pad{false}, ptt_create{false};
int primary_user = -1;

proto::PadState to_state(const PadSample &s, std::uint32_t buttons) noexcept;

// The other signed-in users' controllers (one place each), read on the pad thread.
struct ExtraPad
{
    int user = -1;
    int handle = -1;
    char name[40] = {};
    bool connected = false, cross = false, front = true;
    Chords chords;
    proto::PadState last;
    std::uint64_t last_send = 0;
    bool neutral_sent = false, leave = false;
    std::atomic<Guest *> guest{nullptr};
    std::atomic<std::uint32_t> want_rumble{0}, want_light{0};
    std::atomic<unsigned> rumble_serial{0}, light_serial{0};
    unsigned seen_rumble = 0, seen_light = 0;
};
ExtraPad extra[kExtra];
std::uint64_t next_users = 0;

// Signed-in users (sceUserServiceGetLoginUserIdList): a pad opened for each one not yet open.
void refresh_users() noexcept
{
    // SceUserServiceLoginUserIdList: 4 ids on the PS4; room for more in case the PS5's is larger
    // (a short buffer would be written past).
    struct
    {
        std::int32_t id[64];
    } list{};
    for (auto &x : list.id)
        x = -1;
    if (sceUserServiceGetLoginUserIdList(&list) != 0)
        return;
    for (int k = 0; k < 4; ++k)
    {
        const std::int32_t u = list.id[k];
        if (u < 0 || u == primary_user)
            continue;
        bool have = false;
        for (const auto &e : extra)
            have = have || e.user == u;
        if (have)
            continue;
        for (auto &e : extra)
        {
            if (e.user >= 0)
                continue;
            const int h = scePadOpen(u, 0, 0, nullptr);
            log("pad: another user %#x signed in: open %#x", static_cast<unsigned>(u), h);
            if (h < 0)
                break;
            e.user = u;
            e.handle = h;
            if (sceUserServiceGetUserName(u, e.name, sizeof e.name) != 0)
                e.name[0] = 0;
            scePadSetVibrationMode(h, 2);
            break;
        }
    }
    // a user who signed out: the place is let go (the app sees it gone)
    for (auto &e : extra)
    {
        if (e.user < 0)
            continue;
        bool still = false;
        for (int k = 0; k < 4; ++k)
            still = still || list.id[k] == e.user;
        if (!still)
        {
            log("pad: user %#x signed out", static_cast<unsigned>(e.user));
            if (e.handle >= 0)
                scePadClose(e.handle);
            pthread_mutex_lock(&lock);
            e.user = -1;
            e.handle = -1;
            e.connected = e.cross = false;
            pthread_mutex_unlock(&lock);
        }
    }
}

void read_extra(ExtraPad &e, std::uint64_t now) noexcept
{
    PadSample s[16];
    const int n = e.handle >= 0 ? scePadRead(e.handle, s, 16) : 0;
    if (n <= 0)
        return;
    const PadSample &cur = s[n - 1];
    const bool front = !(cur.buttons & pb::kIntercepted);
    pthread_mutex_lock(&lock);
    e.connected = cur.connected != 0;
    e.front = front;
    e.cross = front && (cur.buttons & pb::kCross);
    pthread_mutex_unlock(&lock);
    // its rumble and light from the host
    const unsigned rs = e.rumble_serial.load();
    if (rs != e.seen_rumble)
    {
        e.seen_rumble = rs;
        const std::uint32_t v = e.want_rumble.load();
        const std::uint8_t m[2] = {static_cast<std::uint8_t>(v >> 8), static_cast<std::uint8_t>(v)};
        scePadSetVibration(e.handle, m);
    }
    const unsigned ls = e.light_serial.load();
    if (ls != e.seen_light)
    {
        e.seen_light = ls;
        const std::uint32_t v = e.want_light.load();
        if (v & 0x1000000u)
        {
            const std::uint8_t c[4] = {static_cast<std::uint8_t>(v >> 16), static_cast<std::uint8_t>(v >> 8),
                                       static_cast<std::uint8_t>(v), 0};
            scePadSetLightBar(e.handle, c);
        }
        else
            scePadResetLightBar(e.handle);
    }
    Guest *g = e.guest.load();
    if (!g)
        return;
    if (!front || !e.connected || menu_has_pad.load())
    {
        if (!e.neutral_sent && g->send_pad(proto::PadState{}))
            e.neutral_sent = true;
        e.last = proto::PadState{};
        return;
    }
    e.neutral_sent = false;
    const auto out = e.chords.update(cur.buttons & ~pb::kIntercepted, now);
    if (out.leave)
    {
        pthread_mutex_lock(&lock);
        e.leave = true;
        pthread_mutex_unlock(&lock);
    }
    const proto::PadState st = to_state(cur, out.buttons);
    if (!proto::same_input(st, e.last) || now - e.last_send >= 100000)
    {
        g->send_pad(st);
        e.last = st;
        e.last_send = now;
    }
}
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

std::atomic<unsigned> rumble_calls{0}, rumble_nonzero{0};
std::atomic<int> rumble_rc{0};

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
        beat(Beat::pad, 0);
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
                const int rc = scePadSetVibration(handle, m);
                rumble_calls.fetch_add(1);
                if (m[0] || m[1])
                    rumble_nonzero.fetch_add(1);
                rumble_rc.store(rc);
                static int logged;
                if (logged++ < 5)
                    log("pad: rumble %u %u: %#x", m[0], m[1], rc);
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
                    const int rc = scePadSetLightBar(handle, c);
                    static int logged;
                    if (logged++ < 5)
                        log("pad: light bar %u %u %u: %#x", c[0], c[1], c[2], rc);
                }
                else
                    scePadResetLightBar(handle);
            }
        }

        // While joined: the pad to the host.
        Guest *g = guest.load();
        if (g)
        {
            chords.set_ptt_on_create(ptt_create.load());
            if (!front || menu_has_pad.load())
            {
                // The system has the pad: the host sees "nothing pressed" once.
                if (!sent_neutral && g->send_pad(proto::PadState{}))
                    sent_neutral = true;
                last_sent = proto::PadState{};
                // the chords still work in the Room menu (△ closes it from the chord too)
                const auto out = front ? chords.update(cur.buttons & ~pb::kIntercepted, now) : Chords::Out{};
                pthread_mutex_lock(&lock);
                chord_view.ps = chord_view.leave = 0;
                chord_view.room_menu = chord_view.room_menu || out.room_menu;
                chord_view.leave_now = chord_view.leave_now || out.leave;
                chord_view.talk = false;
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
                chord_view.room_menu = chord_view.room_menu || out.room_menu;
                chord_view.talk = out.talk;
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
        // the other signed-in users' controllers (many controllers on one PS5)
        if (now >= next_users)
        {
            next_users = now + 1000000;
            beat(Beat::pad, 1);
            refresh_users();
            beat(Beat::pad, 2);
        }
        for (auto &e : extra)
            if (e.user >= 0)
                read_extra(e, now);
        beat(Beat::pad, 3);
        sceKernelUsleep(4000);
    }
    return nullptr;
}
} // namespace

bool open(int user) noexcept
{
    primary_user = user;
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
    chord_view.room_menu = false;
    pthread_mutex_unlock(&lock);
    return c;
}

platform::Chords chords_peek() noexcept
{
    pthread_mutex_lock(&lock);
    platform::Chords c = chord_view;
    pthread_mutex_unlock(&lock);
    return c;
}

void to_menu(bool on) noexcept
{
    menu_has_pad.store(on);
}

void ptt_on_create(bool on) noexcept
{
    ptt_create.store(on);
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

Rumbles rumbles() noexcept
{
    return {rumble_calls.load(), rumble_nonzero.load(), rumble_rc.load()};
}

int extras(Extra *out) noexcept
{
    int n = 0;
    pthread_mutex_lock(&lock);
    for (int i = 0; i < kExtra; ++i)
    {
        out[i] = Extra{};
        out[i].user = extra[i].user;
        std::snprintf(out[i].name, sizeof out[i].name, "%s", extra[i].name);
        out[i].connected = extra[i].connected;
        out[i].cross = extra[i].cross;
        out[i].in_front = extra[i].front;
        n += extra[i].user >= 0 ? 1 : 0;
    }
    pthread_mutex_unlock(&lock);
    return n;
}

void attach(int user, Guest *g) noexcept
{
    for (auto &e : extra)
        if (e.user == user)
        {
            e.guest.store(g);
            if (!g)
            {
                e.want_rumble.store(0);
                e.rumble_serial.fetch_add(1);
                e.want_light.store(0);
                e.light_serial.fetch_add(1);
            }
        }
}

bool extra_leave(int user) noexcept
{
    bool l = false;
    pthread_mutex_lock(&lock);
    for (auto &e : extra)
        if (e.user == user && e.leave)
            l = true, e.leave = false;
    pthread_mutex_unlock(&lock);
    return l;
}

void extra_rumble(int user, std::uint8_t left, std::uint8_t right) noexcept
{
    for (auto &e : extra)
        if (e.user == user)
        {
            e.want_rumble.store(static_cast<std::uint32_t>(left) << 8 | right);
            e.rumble_serial.fetch_add(1);
        }
}

void extra_light(int user, std::uint8_t r, std::uint8_t g, std::uint8_t b) noexcept
{
    for (auto &e : extra)
        if (e.user == user)
        {
            e.want_light.store(0x1000000u | static_cast<std::uint32_t>(r) << 16 | static_cast<std::uint32_t>(g) << 8 | b);
            e.light_serial.fetch_add(1);
        }
}
} // namespace play::ps5::padio
