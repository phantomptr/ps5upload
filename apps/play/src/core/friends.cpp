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
    add("# PS5Upload Play: hosts to play again with (addr|port|tailnet|seen|path|pass|name)\n");
    char line[320];
    for (const auto &f : list)
        if (f.addr[0])
        {
            std::snprintf(line, sizeof line, "friend=%s|%u|%d|%u|%s|%s|%s\n", f.addr, static_cast<unsigned>(f.port),
                          f.tailnet ? 1 : 0, f.seen, f.path, f.pass, f.name);
            add(line);
        }
    return n;
}

void Friends::parse(const char *text, std::size_t len) noexcept
{
    forget_all();
    clock = 0;
    int fi = 0;
    std::size_t i = 0;
    while (i < len && fi < kMax)
    {
        std::size_t e = i;
        while (e < len && text[e] != '\n')
            ++e;
        char line[400];
        copy(line, sizeof line, text + i, e - i);
        i = e + 1;
        if (std::strncmp(line, "friend=", 7))
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
} // namespace play
