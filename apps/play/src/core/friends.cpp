// PS5Upload Play - "Play again": remembered hosts and their friend passes.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "friends.hpp"

#include <cstdio>
#include <cstdlib>
#include <cstring>

namespace play
{
namespace
{
// One line each, fields split by '|': none of them may hold one (or a line break).
bool safe(const char *s) noexcept
{
    for (; *s; ++s)
        if (*s == '\n' || *s == '\r' || *s == '|' || static_cast<unsigned char>(*s) < 0x20)
            return false;
    return true;
}

void copy(char *dst, std::size_t cap, const char *src, std::size_t len) noexcept
{
    const std::size_t n = len < cap - 1 ? len : cap - 1;
    std::memcpy(dst, src, n);
    dst[n] = 0;
}
} // namespace

Friend *Friends::find(const char *addr, std::uint16_t port) noexcept
{
    for (auto &f : list)
        if (f.addr[0] && f.port == port && !std::strcmp(f.addr, addr))
            return &f;
    return nullptr;
}

bool Friends::set_link(Friend &f, const char *room, const char *fp, const char *key) noexcept
{
    if (!room || !fp || !key || !safe(room) || !safe(fp) || !safe(key) || std::strlen(room) >= sizeof f.room ||
        std::strlen(fp) >= sizeof f.fp || std::strlen(key) >= sizeof f.key)
        return false;
    std::snprintf(f.room, sizeof f.room, "%s", room);
    std::snprintf(f.fp, sizeof f.fp, "%s", fp);
    std::snprintf(f.key, sizeof f.key, "%s", key);
    return true;
}

Friend *Friends::remember(const char *addr, std::uint16_t port, bool tailnet, const char *path, const char *name,
                          const char *pass) noexcept
{
    if (!addr || !*addr || !pass || !*pass || !safe(addr) || !safe(pass) || std::strlen(pass) >= sizeof list[0].pass ||
        std::strlen(addr) >= sizeof list[0].addr || (path && !safe(path)))
        return nullptr;
    Friend *f = find(addr, port);
    if (!f)
    {
        f = &list[0];
        for (auto &c : list)
        {
            if (!c.addr[0])
            {
                f = &c;
                break;
            }
            if (c.seen < f->seen)
                f = &c;
        }
        *f = Friend{};
        std::snprintf(f->addr, sizeof f->addr, "%s", addr);
        f->port = port;
    }
    f->tailnet = tailnet;
    std::snprintf(f->path, sizeof f->path, "%s", path ? path : "");
    std::snprintf(f->pass, sizeof f->pass, "%s", pass);
    if (name && *name && safe(name))
        std::snprintf(f->name, sizeof f->name, "%s", name);
    f->seen = ++clock;
    return f;
}

void Friends::forget(const Friend *f) noexcept
{
    for (auto &c : list)
        if (&c == f)
            c = Friend{};
}

void Friends::forget_all() noexcept
{
    for (auto &c : list)
        c = Friend{};
}

int Friends::recent(const Friend **out, int cap) const noexcept
{
    int n = 0;
    for (const auto &f : list)
        if (f.addr[0] && n < cap)
            out[n++] = &f;
    for (int i = 1; i < n; ++i)
        for (int k = i; k > 0 && out[k]->seen > out[k - 1]->seen; --k)
        {
            const Friend *t = out[k];
            out[k] = out[k - 1];
            out[k - 1] = t;
        }
    return n;
}

std::size_t Friends::write(char *out, std::size_t cap) const noexcept
{
    std::size_t n = 0;
    auto add = [&](const char *s) {
        const std::size_t l = std::strlen(s);
        if (n + l + 1 > cap)
            return;
        std::memcpy(out + n, s, l);
        n += l;
        out[n] = 0;
    };
    add("# PS5Upload Play: hosts to play again with (addr|port|tailnet|seen|path|pass|name), the last\n"
        "# internet invite played with each (link=addr|port|room|fp|key), and rooms (room=addr|port|lobby|id|rtts)\n");
    char line[400];
    for (const auto &f : list)
        if (f.addr[0])
        {
            std::snprintf(line, sizeof line, "friend=%s|%u|%d|%u|%s|%s|%s\n", f.addr, static_cast<unsigned>(f.port),
                          f.tailnet ? 1 : 0, f.seen, f.path, f.pass, f.name);
            add(line);
            if (f.room[0] && f.fp[0] && f.key[0])
            {
                std::snprintf(line, sizeof line, "link=%s|%u|%s|%s|%s\n", f.addr, static_cast<unsigned>(f.port), f.room,
                              f.fp, f.key);
                add(line);
            }
            if (f.lobby[0] || f.id[0] || f.rtts[0] >= 0)
            {
                std::snprintf(line, sizeof line, "room=%s|%u|%s|%s|%d,%d,%d\n", f.addr, static_cast<unsigned>(f.port),
                              f.lobby, f.id, f.rtts[0], f.rtts[1], f.rtts[2]);
                add(line);
            }
        }
    return n;
}

void Friends::parse(const char *text, std::size_t len) noexcept
{
    forget_all();
    clock = 0;
    int fi = 0;
    std::size_t i = 0;
    while (i < len)
    {
        std::size_t e = i;
        while (e < len && text[e] != '\n')
            ++e;
        char line[400];
        copy(line, sizeof line, text + i, e - i);
        i = e + 1;
        if (!std::strncmp(line, "room=", 5))
        {
            // addr|port|lobby|id|rtt,rtt,rtt for a friend read above
            char a[64] = {}, lob[24] = {}, id[72] = {};
            unsigned port = 0;
            int r0 = -1, r1 = -1, r2 = -1;
            const int got = std::sscanf(line + 5, "%63[^|]|%u|%23[^|]|%71[^|]|%d,%d,%d", a, &port, lob, id, &r0, &r1, &r2);
            if (got < 2)
            {
                // empty lobby or id fields: read them one by one
                continue;
            }
            Friend *f = port > 0 && port < 65536 ? find(a, static_cast<std::uint16_t>(port)) : nullptr;
            if (!f)
                continue;
            // the fields may be empty: split by hand
            const char *p = line + 5;
            const char *fld[5] = {};
            std::size_t fl[5] = {};
            int k = 0;
            for (; k < 5 && p; ++k)
            {
                fld[k] = p;
                const char *bar = k < 4 ? std::strchr(p, '|') : nullptr;
                fl[k] = bar ? static_cast<std::size_t>(bar - p) : std::strlen(p);
                p = bar ? bar + 1 : nullptr;
            }
            if (k < 5 || !fld[4])
                continue;
            copy(f->lobby, sizeof f->lobby, fld[2], fl[2]);
            copy(f->id, sizeof f->id, fld[3], fl[3]);
            f->rtts[0] = f->rtts[1] = f->rtts[2] = -1;
            std::sscanf(fld[4], "%d,%d,%d", &f->rtts[0], &f->rtts[1], &f->rtts[2]);
            continue;
        }
        if (!std::strncmp(line, "link=", 5))
        {
            // addr|port|room|fp|key, for a friend read above
            char *fld[5] = {};
            char *p = line + 5;
            int k = 0;
            for (; k < 5 && p; ++k)
            {
                fld[k] = p;
                char *bar = k < 4 ? std::strchr(p, '|') : nullptr;
                if (bar)
                    *bar = 0;
                p = bar ? bar + 1 : nullptr;
            }
            if (k == 5 && fld[4])
            {
                const long port = std::strtol(fld[1], nullptr, 10);
                if (Friend *f = port > 0 && port < 65536 ? find(fld[0], static_cast<std::uint16_t>(port)) : nullptr)
                    set_link(*f, fld[2], fld[3], fld[4]);
            }
            continue;
        }
        if (std::strncmp(line, "friend=", 7) || fi >= kMax)
            continue;
        // addr|port|tailnet|seen|path|pass|name (the name last: the only free text)
        const char *f[7] = {};
        std::size_t fl[7] = {};
        const char *p = line + 7;
        bool ok = true;
        for (int k = 0; k < 7; ++k)
        {
            f[k] = p;
            const char *bar = k < 6 ? std::strchr(p, '|') : nullptr;
            if (k < 6 && !bar)
            {
                ok = false;
                break;
            }
            fl[k] = bar ? static_cast<std::size_t>(bar - p) : std::strlen(p);
            p = bar ? bar + 1 : p + fl[k];
        }
        if (!ok || !fl[0] || !fl[5])
            continue;
        Friend &x = list[fi++];
        x = Friend{};
        copy(x.addr, sizeof x.addr, f[0], fl[0]);
        char num[16];
        copy(num, sizeof num, f[1], fl[1]);
        const long port = std::strtol(num, nullptr, 10);
        x.port = static_cast<std::uint16_t>(port > 0 && port < 65536 ? port : 19113);
        x.tailnet = fl[2] == 1 && f[2][0] == '1';
        copy(num, sizeof num, f[3], fl[3]);
        x.seen = static_cast<std::uint32_t>(std::strtoul(num, nullptr, 10));
        copy(x.path, sizeof x.path, f[4], fl[4]);
        copy(x.pass, sizeof x.pass, f[5], fl[5]);
        copy(x.name, sizeof x.name, f[6], fl[6]);
        if (x.seen > clock)
            clock = x.seen;
    }
}
Friend *Friends::find_id(const char *id) noexcept
{
    if (!id || !*id)
        return nullptr;
    for (auto &f : list)
        if (f.addr[0] && !std::strcmp(f.id, id))
            return &f;
    return nullptr;
}

int Friend::last_rtt() const noexcept
{
    int v[3], n = 0;
    for (int r : rtts)
        if (r >= 0)
            v[n++] = r;
    if (!n)
        return -1;
    // the median of up to three
    for (int i = 1; i < n; ++i)
        for (int k = i; k > 0 && v[k] < v[k - 1]; --k)
        {
            const int t = v[k];
            v[k] = v[k - 1];
            v[k - 1] = t;
        }
    return v[n / 2];
}

void Friend::add_rtt(int ms) noexcept
{
    if (ms < 0)
        return;
    rtts[2] = rtts[1];
    rtts[1] = rtts[0];
    rtts[0] = ms;
}
} // namespace play
