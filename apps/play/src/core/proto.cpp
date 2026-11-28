// PS5Upload Play - what the app says to the PS5Upload engine and how it reads the answers.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "proto.hpp"

#include "json.hpp"

#include <cstdio>
#include <cstring>

namespace play::proto
{
namespace
{
// libScePad bit, the apps' bit (engine vpad.rs APP_TO_PAD).
constexpr std::uint32_t kMap[16][2] = {
    {pad::kCross, 1},       {pad::kCircle, 2},     {pad::kSquare, 4},   {pad::kTriangle, 8},
    {pad::kLeft, 16},       {pad::kRight, 32},     {pad::kUp, 64},      {pad::kDown, 128},
    {pad::kL1, 256},        {pad::kR1, 512},       {pad::kL3, 1024},    {pad::kR3, 2048},
    {pad::kOptions, 4096},  {pad::kCreate, 8192},  {pad::kTouchpad, 16384}, {pad::kPs, 32768},
};

void put32(std::uint8_t *p, std::uint32_t v) noexcept
{
    p[0] = static_cast<std::uint8_t>(v);
    p[1] = static_cast<std::uint8_t>(v >> 8);
    p[2] = static_cast<std::uint8_t>(v >> 16);
    p[3] = static_cast<std::uint8_t>(v >> 24);
}

void copy(char *dst, std::size_t cap, const json::Doc &d, int i) noexcept
{
    if (!d.str(i, dst, cap))
        dst[0] = 0;
}
} // namespace

std::uint32_t raw_to_app(std::uint32_t raw) noexcept
{
    std::uint32_t a = 0;
    for (const auto &m : kMap)
        if (raw & m[0])
            a |= m[1];
    return a;
}

bool same_input(const PadState &a, const PadState &b) noexcept
{
    if (a.buttons != b.buttons || a.lx != b.lx || a.ly != b.ly || a.rx != b.rx || a.ry != b.ry ||
        a.l2 != b.l2 || a.r2 != b.r2 || a.touches != b.touches)
        return false;
    for (int i = 0; i < a.touches; ++i)
        if (a.touch[i].id != b.touch[i].id || a.touch[i].x != b.touch[i].x || a.touch[i].y != b.touch[i].y)
            return false;
    return true;
}

std::size_t pad_frame(std::uint8_t *out, std::size_t cap, std::uint32_t seq, const PadState &s,
                      bool raw) noexcept
{
    const int n = s.touches > 2 ? 2 : s.touches;
    const std::size_t size = 16 + static_cast<std::size_t>(n) * 5 + (s.has_motion ? 25 : 0);
    if (size > cap)
        return 0;
    out[0] = 0x01;
    put32(out + 1, seq);
    // The system's own bits never go: only what the engine knows.
    put32(out + 5, raw ? (s.buttons & ~pad::kIntercepted) : raw_to_app(s.buttons));
    out[9] = s.lx;
    out[10] = s.ly;
    out[11] = s.rx;
    out[12] = s.ry;
    out[13] = s.l2;
    out[14] = s.r2;
    out[15] = static_cast<std::uint8_t>(n);
    std::size_t o = 16;
    for (int i = 0; i < n; ++i)
    {
        out[o] = s.touch[i].id;
        out[o + 1] = static_cast<std::uint8_t>(s.touch[i].x);
        out[o + 2] = static_cast<std::uint8_t>(s.touch[i].x >> 8);
        out[o + 3] = static_cast<std::uint8_t>(s.touch[i].y);
        out[o + 4] = static_cast<std::uint8_t>(s.touch[i].y >> 8);
        o += 5;
    }
    if (s.has_motion)
    {
        out[o++] = 0x02;
        const float v[6] = {s.gyro[0], s.gyro[1], s.gyro[2], s.accel[0], s.accel[1], s.accel[2]};
        for (float f : v)
        {
            std::uint32_t bits;
            std::memcpy(&bits, &f, 4);
            put32(out + o, bits);
            o += 4;
        }
    }
    return o;
}

std::size_t hello(char *out, std::size_t cap, const char *code, const char *resume, const char *name,
                  const ClientCaps &caps) noexcept
{
    return hello_ex(out, cap, code, resume, name, caps, HelloExtras{});
}

std::size_t hello_ex(char *out, std::size_t cap, const char *code, const char *resume, const char *name,
                     const ClientCaps &caps, const HelloExtras &x) noexcept
{
    json::Writer w(out, cap);
    w.raw("{\"hello\":{");
    bool first = true;
    auto field = [&](const char *k, const char *v) {
        if (!v || !*v)
            return;
        if (!first)
            w.raw(",");
        first = false;
        w.str(k).raw(":").str(v);
    };
    field("invite", x.invite);
    field("code", code);
    field("resume", resume);
    // A pass goes only on a new ask: coming back is the resume token's.
    if (!resume || !*resume)
        field("again", x.again);
    field("name", name);
    if (!first)
        w.raw(",");
    if (x.remember)
        w.raw("\"remember\":true,");
    if (x.inbox_room && *x.inbox_room && x.inbox_hub && *x.inbox_hub)
        w.raw("\"inbox\":{\"room\":").str(x.inbox_room).raw(",\"hub\":").str(x.inbox_hub).raw("},");
    if (x.device && *x.device)
        w.raw("\"device\":").str(x.device).raw(",");
    if (x.knock_via && *x.knock_via && (!resume || !*resume))
        w.raw("\"knock\":{\"via\":").str(x.knock_via).raw("},");
    if (x.no_media)
        w.raw("\"media\":false,");
    if (x.group && *x.group)
        w.raw("\"group\":").str(x.group).raw(",");
    if (x.rooms)
        w.raw("\"proto\":2,\"caps\":[").raw(kRoomFeatures).raw("],");
    w.raw("\"client\":{\"kind\":\"ps5\",\"app\":\"play\",\"v\":").str(caps.version);
    w.raw(",\"caps\":{\"hevc\":").boolean(caps.hevc);
    w.raw(",\"main10\":").boolean(caps.main10);
    w.raw(",\"raw_pad\":").boolean(caps.raw_pad);
    w.raw(",\"timing\":").boolean(caps.timing);
    w.raw("}}}}");
    return w.ok() ? w.len() : 0;
}

bool parse_guest(const char *text, std::size_t len, GuestEvent &ev) noexcept
{
    ev = GuestEvent{};
    json::Token toks[320];
    json::Doc d(toks, 320);
    if (!d.parse(text, len) || d.type(0) != json::Type::object)
        return false;
    int v;
    if ((v = d.get(0, "stage")) >= 0)
    {
        if (d.str_is(v, "asking"))
            ev.type = GuestEvent::asking;
        else if (d.str_is(v, "waiting"))
            ev.type = GuestEvent::waiting;
        else if (d.str_is(v, "joined"))
            ev.type = GuestEvent::joined;
        else if (d.str_is(v, "refused"))
            ev.type = GuestEvent::refused;
        else
            return false;
        copy(ev.why, sizeof ev.why, d, d.get(0, "why"));
        copy(ev.token, sizeof ev.token, d, d.get(0, "token"));
        copy(ev.friend_pass, sizeof ev.friend_pass, d, d.get(0, "friend"));
        ev.player = static_cast<int>(d.integer(d.get(0, "player")));
        ev.hevc = d.boolean(d.get(0, "hevc"));
        copy(ev.lobby, sizeof ev.lobby, d, d.get(0, "lobby"));
        if (ev.type == GuestEvent::refused)
        {
            // The engine says `tries_left` (a stand-in once said `left`).
            int left = d.get(0, "tries_left");
            if (left < 0)
                left = d.get(0, "left");
            ev.tries_left = d.type(left) == json::Type::number ? static_cast<int>(d.integer(left)) : -1;
            // A name already in the room: {"stage": "refused", "why": "name_taken", "suggest"}.
            if (!std::strcmp(ev.why, "name_taken"))
            {
                ev.type = GuestEvent::name_taken;
                int s = d.get(0, "suggest");
                if (s < 0)
                    s = d.get(d.get(0, "name_taken"), "suggest");
                copy(ev.suggest, sizeof ev.suggest, d, s);
            }
        }
        const int cw = d.get(0, "codec_warning");
        ev.caps_known = cw >= 0;
        copy(ev.codec_warning, sizeof ev.codec_warning, d, cw);
        return true;
    }
    if ((v = d.get(0, "status")) >= 0)
    {
        ev.type = GuestEvent::status;
        ev.player = static_cast<int>(d.integer(d.get(v, "player")));
        const int r = d.get(v, "rtt_ms");
        ev.rtt_ms = d.type(r) == json::Type::number ? static_cast<int>(d.integer(r)) : -1;
        copy(ev.host, sizeof ev.host, d, d.get(v, "host"));
        copy(ev.state, sizeof ev.state, d, d.get(v, "state"));
        copy(ev.plays_as, sizeof ev.plays_as, d, d.get(v, "plays_as"));
        copy(ev.host_user, sizeof ev.host_user, d, d.get(v, "host_user"));
        copy(ev.codec_warning, sizeof ev.codec_warning, d, d.get(v, "codec_warning"));
        ev.block_system = d.boolean(d.get(v, "block_system"));
        ev.stream_seat = d.boolean(d.get(v, "stream_seat"));
        ev.move_first = d.boolean(d.get(v, "move_first"));
        ev.took_host = d.boolean(d.get(v, "took_host"));
        return true;
    }
    const int ru = d.get(0, "rumble"), li = d.get(0, "light");
    if (ru >= 0 || li >= 0)
    {
        ev.type = GuestEvent::rumble_light;
        if (d.type(ru) == json::Type::array && d.count(ru) >= 2)
        {
            ev.has_rumble = true;
            for (int k = 0; k < 2; ++k)
            {
                const long long x = d.integer(d.item(ru, static_cast<std::uint32_t>(k)));
                ev.rumble_lr[k] = static_cast<std::uint8_t>(x < 0 ? 0 : x > 255 ? 255 : x);
            }
        }
        if (d.type(li) == json::Type::array && d.count(li) >= 3)
        {
            ev.has_light = true;
            for (int k = 0; k < 3; ++k)
            {
                const long long x = d.integer(d.item(li, static_cast<std::uint32_t>(k)));
                ev.light_rgb[k] = static_cast<std::uint8_t>(x < 0 ? 0 : x > 255 ? 255 : x);
            }
        }
        return ev.has_rumble || ev.has_light;
    }
    if ((v = d.get(0, "bye")) >= 0)
    {
        ev.type = GuestEvent::bye;
        copy(ev.why, sizeof ev.why, d, v);
        return true;
    }
    if ((v = d.get(0, "knock")) >= 0)
    {
        // {"knock": "challenge", "c", "pk"} | {"knock": "wait"} | {"knock": "ok"} |
        // {"knock": "no", "why", "left"}
        ev.type = GuestEvent::knock;
        copy(ev.knock_state, sizeof ev.knock_state, d, v);
        copy(ev.challenge, sizeof ev.challenge, d, d.get(0, "c"));
        copy(ev.pk, sizeof ev.pk, d, d.get(0, "pk"));
        copy(ev.why, sizeof ev.why, d, d.get(0, "why"));
        int left = d.get(0, "tries_left");
        if (left < 0)
            left = d.get(0, "left");
        ev.tries_left = d.type(left) == json::Type::number ? static_cast<int>(d.integer(left)) : -1;
        return ev.knock_state[0] != 0;
    }
    if ((v = d.get(0, "name_taken")) >= 0)
    {
        ev.type = GuestEvent::name_taken;
        copy(ev.suggest, sizeof ev.suggest, d, d.get(v, "suggest"));
        return true;
    }
    if (d.get(0, "watch") >= 0 || d.get(0, "picture") >= 0)
        return true; // the watch page's notes: nothing for the app (type none)
    if ((v = d.get(0, "pong")) >= 0)
    {
        ev.type = GuestEvent::pong;
        ev.pong_t = d.num(v);
        const int e = d.get(0, "engine_us");
        ev.has_engine_us = d.type(e) == json::Type::number;
        ev.engine_us = d.num(e);
        return true;
    }
    return false;
}

bool normalize_code(const char *in, char out[kCodeLen + 1]) noexcept
{
    int n = 0;
    for (const char *p = in; *p; ++p)
    {
        char c = *p;
        if (c == '-' || c == ' ')
            continue;
        if (c >= 'a' && c <= 'z')
            c = static_cast<char>(c - 32);
        if (!std::strchr(kCodeAlphabet, c) || c == 0 || n >= kCodeLen)
            return false;
        out[n++] = c;
    }
    out[n] = 0;
    return n == kCodeLen;
}

void code_display(const char *code, char out[kCodeLen + 2]) noexcept
{
    const std::size_t l = std::strlen(code);
    std::size_t o = 0;
    for (std::size_t i = 0; i < l && o < kCodeLen + 1; ++i)
    {
        if (i == 3)
            out[o++] = '-';
        out[o++] = code[i];
    }
    out[o] = 0;
}

void probe(std::uint8_t out[kProbeSize]) noexcept
{
    std::memset(out, ' ', kProbeSize);
    std::memcpy(out, kProbe, sizeof kProbe - 1);
}

bool parse_found(const char *text, std::size_t len, Found &f) noexcept
{
    f = Found{};
    json::Token toks[32];
    json::Doc d(toks, 32);
    if (!d.parse(text, len) || d.type(0) != json::Type::object)
        return false;
    const double port = d.num(d.get(0, "port"), -1);
    if (port < 1 || port > 65535)
        return false;
    f.port = static_cast<std::uint16_t>(port);
    copy(f.name, sizeof f.name, d, d.get(0, "name"));
    copy(f.version, sizeof f.version, d, d.get(0, "version"));
    copy(f.id, sizeof f.id, d, d.get(0, "id"));
    const double t = d.num(d.get(0, "together"), 0);
    f.together = t > 0 && t < 100 ? static_cast<int>(t) : 0;
    const int you = d.get(0, "you");
    if (d.type(you) == json::Type::object)
    {
        f.you_known = d.boolean(d.get(you, "known"));
        f.you_streaming = d.boolean(d.get(you, "streaming"));
        f.you_paired = d.boolean(d.get(you, "paired"));
        copy(f.you_name, sizeof f.you_name, d, d.get(you, "name"));
    }
    return true;
}

bool is_keyframe(const std::uint8_t *au, std::size_t len, bool hevc) noexcept
{
    for (std::size_t i = 0; i + 3 < len; ++i)
    {
        if (au[i] != 0 || au[i + 1] != 0)
            continue;
        std::size_t at;
        if (au[i + 2] == 1)
            at = i + 3;
        else if (au[i + 2] == 0 && au[i + 3] == 1)
            at = i + 4;
        else
            continue;
        if (at >= len)
            break;
        if (hevc)
        {
            const int t = (au[at] >> 1) & 0x3F;
            if (t >= 16 && t <= 21)
                return true;
        }
        else if ((au[at] & 0x1F) == 5)
            return true;
        i = at;
    }
    return false;
}

std::uint32_t version_number(const char *v) noexcept
{
    // (no sscanf: the console app keeps to the calls known to work there)
    unsigned part[3] = {};
    int n = 0;
    bool digit = false;
    for (const char *p = v; p && *p && n < 3; ++p)
    {
        if (*p >= '0' && *p <= '9')
            part[n] = part[n] * 10 + static_cast<unsigned>(*p - '0'), digit = true;
        else if (*p == '.' && digit)
            ++n, digit = false;
        else
            break;
    }
    if (digit)
        ++n;
    if (n < 2)
        return 0;
    return part[0] * 1000000u + part[1] * 1000u + part[2];
}

const char *refusal_key(const char *why) noexcept
{
    return why;
}
} // namespace play::proto
