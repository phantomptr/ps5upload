// PS5Upload Play - what the app keeps between runs.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "settings.hpp"

#include <cstdarg>
#include <cstdio>
#include <cstdlib>
#include <cstring>

namespace play
{
namespace
{
void copy(char *dst, std::size_t cap, const char *src, std::size_t len) noexcept
{
    if (!cap)
        return;
    const std::size_t n = len < cap - 1 ? len : cap - 1;
    std::memcpy(dst, src, n);
    dst[n] = 0;
}

bool safe(const char *s) noexcept
{
    // Values are written one per line, fields split by '|': neither may appear inside one.
    for (; *s; ++s)
        if (*s == '\n' || *s == '\r' || *s == '|')
            return false;
    return true;
}
} // namespace

Host *Settings::find(const char *addr, std::uint16_t port) noexcept
{
    for (auto &h : hosts)
        if (h.addr[0] && h.port == port && std::strcmp(h.addr, addr) == 0)
            return &h;
    return nullptr;
}

Host *Settings::remember(const char *addr, std::uint16_t port, const char *name) noexcept
{
    if (!addr || !*addr || !safe(addr))
        return nullptr;
    Host *h = find(addr, port);
    if (!h)
    {
        h = &hosts[0];
        for (auto &c : hosts)
        {
            if (!c.addr[0])
            {
                h = &c;
                break;
            }
            if (c.seen < h->seen)
                h = &c;
        }
        *h = Host{};
        copy(h->addr, sizeof h->addr, addr, std::strlen(addr));
        h->port = port;
    }
    if (name && *name && safe(name))
        copy(h->name, sizeof h->name, name, std::strlen(name));
    h->seen = ++clock;
    return h;
}

void Settings::forget(const Host *h) noexcept
{
    for (auto &c : hosts)
        if (&c == h)
            c = Host{};
}

int Settings::recent(const Host **out, int cap) const noexcept
{
    int n = 0;
    for (const auto &h : hosts)
        if (h.addr[0] && n < cap)
            out[n++] = &h;
    // newest first (a handful: insertion sort)
    for (int i = 1; i < n; ++i)
        for (int k = i; k > 0 && out[k]->seen > out[k - 1]->seen; --k)
        {
            const Host *t = out[k];
            out[k] = out[k - 1];
            out[k - 1] = t;
        }
    return n;
}

std::size_t Settings::write(char *out, std::size_t cap) const noexcept
{
    std::size_t n = 0;
    const auto add = [&](const char *fmt, ...) __attribute__((format(printf, 2, 3))) {
        if (n + 1 >= cap)
            return;
        va_list ap;
        va_start(ap, fmt);
        const int w = std::vsnprintf(out + n, cap - n, fmt, ap);
        va_end(ap);
        if (w > 0)
            n = n + static_cast<std::size_t>(w) < cap ? n + static_cast<std::size_t>(w) : cap - 1;
    };
    add("# PS5Upload Play settings\n");
    add("music=%d\n", music ? 1 : 0);
    add("music_volume=%d\n", music_volume);
    add("sounds=%d\n", sounds ? 1 : 0);
    add("hints=%d\n", hints ? 1 : 0);
    add("clock=%u\n", clock);
    for (const auto &h : hosts)
        if (h.addr[0])
            add("host=%s|%u|%u|%s|%s\n", h.addr, static_cast<unsigned>(h.port), h.seen, h.token, h.name);
    return n;
}

void Settings::parse(const char *text, std::size_t len) noexcept
{
    int hi = 0;
    std::size_t i = 0;
    while (i < len)
    {
        std::size_t e = i;
        while (e < len && text[e] != '\n')
            ++e;
        char line[320];
        copy(line, sizeof line, text + i, e - i);
        i = e + 1;
        char *eq = std::strchr(line, '=');
        if (line[0] == '#' || !eq)
            continue;
        *eq = 0;
        const char *k = line, *v = eq + 1;
        if (!std::strcmp(k, "music"))
            music = std::atoi(v) != 0;
        else if (!std::strcmp(k, "music_volume"))
        {
            const int x = std::atoi(v);
            music_volume = x < 0 ? 0 : x > 100 ? 100 : x;
        }
        else if (!std::strcmp(k, "sounds"))
            sounds = std::atoi(v) != 0;
        else if (!std::strcmp(k, "hints"))
            hints = std::atoi(v) != 0;
        else if (!std::strcmp(k, "clock"))
            clock = static_cast<std::uint32_t>(std::strtoul(v, nullptr, 10));
        else if (!std::strcmp(k, "host") && hi < kHosts)
        {
            // addr|port|seen|token|name (the name last: it is the only free text)
            const char *f[5] = {};
            std::size_t fl[5] = {};
            const char *p = v;
            for (int n = 0; n < 5; ++n)
            {
                f[n] = p;
                const char *bar = n < 4 ? std::strchr(p, '|') : nullptr;
                fl[n] = bar ? static_cast<std::size_t>(bar - p) : std::strlen(p);
                if (!bar && n < 4)
                {
                    f[n] = nullptr;
                    break;
                }
                p = bar ? bar + 1 : p + fl[n];
            }
            if (!f[4] || fl[0] == 0)
                continue;
            Host &h = hosts[hi++];
            h = Host{};
            copy(h.addr, sizeof h.addr, f[0], fl[0]);
            char num[16];
            copy(num, sizeof num, f[1], fl[1]);
            const long port = std::strtol(num, nullptr, 10);
            h.port = static_cast<std::uint16_t>(port > 0 && port < 65536 ? port : 19113);
            copy(num, sizeof num, f[2], fl[2]);
            h.seen = static_cast<std::uint32_t>(std::strtoul(num, nullptr, 10));
            copy(h.token, sizeof h.token, f[3], fl[3]);
            copy(h.name, sizeof h.name, f[4], fl[4]);
        }
    }
    for (const auto &h : hosts)
        if (h.seen > clock)
            clock = h.seen;
}

bool split_address(const char *in, char *addr, std::size_t cap, std::uint16_t *port,
                   std::uint16_t default_port) noexcept
{
    while (*in == ' ')
        ++in;
    const char *colon = std::strrchr(in, ':');
    std::size_t al = colon ? static_cast<std::size_t>(colon - in) : std::strlen(in);
    while (al && in[al - 1] == ' ')
        --al;
    if (al == 0 || al >= cap)
        return false;
    for (std::size_t i = 0; i < al; ++i)
    {
        const char c = in[i];
        if (!((c >= '0' && c <= '9') || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c == '.' || c == '-'))
            return false;
    }
    copy(addr, cap, in, al);
    *port = default_port;
    if (colon)
    {
        char *end = nullptr;
        const long p = std::strtol(colon + 1, &end, 10);
        if (p < 1 || p > 65535 || (end && *end && *end != ' '))
            return false;
        *port = static_cast<std::uint16_t>(p);
    }
    return true;
}
} // namespace play
