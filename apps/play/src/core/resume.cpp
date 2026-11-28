// PS5Upload Play - resume.txt (see resume.hpp).
// SPDX-License-Identifier: GPL-3.0-or-later
#include "resume.hpp"

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <initializer_list>

namespace play::resume
{
namespace
{
bool clean(const char *s) noexcept
{
    return !std::strpbrk(s, "|\r\n");
}

void copy(char *dst, std::size_t cap, const char *src) noexcept
{
    std::snprintf(dst, cap, "%s", src ? src : "");
}
} // namespace

std::size_t write(const Record &r, char *out, std::size_t cap) noexcept
{
    const route::Candidate &w = r.way;
    for (const char *s : {r.addr, r.token, r.path, r.name, w.addr, w.path, w.room, w.fp})
        if (!clean(s))
            return 0;
    int n = std::snprintf(out, cap, "%s|%u|%s|%s|%s|%llu", r.addr, static_cast<unsigned>(r.port), r.token, r.path,
                          r.name, static_cast<unsigned long long>(r.at));
    if (n > 0 && r.has_way && static_cast<std::size_t>(n) < cap)
        n += std::snprintf(out + n, cap - static_cast<std::size_t>(n), "|%u|%u|%s|%u|%s|%s|%s|%u",
                           static_cast<unsigned>(w.kind), static_cast<unsigned>(w.way), w.addr,
                           static_cast<unsigned>(w.port), w.path, w.room, w.fp, w.lobby ? 1u : 0u);
    if (n <= 0 || static_cast<std::size_t>(n) + 1 >= cap)
        return 0;
    out[n++] = '\n';
    out[n] = 0;
    return static_cast<std::size_t>(n);
}

bool parse(const char *text, Record &out) noexcept
{
    out = Record{};
    char buf[512];
    copy(buf, sizeof buf, text);
    if (char *nl = std::strpbrk(buf, "\r\n"))
        *nl = 0;
    // fields may be empty: split by hand
    constexpr int kMax = 14;
    char *f[kMax] = {};
    int nf = 0;
    for (char *p = buf; p && nf < kMax;)
    {
        f[nf++] = p;
        char *bar = std::strchr(p, '|');
        if (bar)
            *bar = 0;
        p = bar ? bar + 1 : nullptr;
    }
    if (nf < 6 || !f[0][0] || !f[2][0])
        return false;
    copy(out.addr, sizeof out.addr, f[0]);
    out.port = static_cast<std::uint16_t>(std::strtoul(f[1], nullptr, 10));
    copy(out.token, sizeof out.token, f[2]);
    copy(out.path, sizeof out.path, f[3]);
    copy(out.name, sizeof out.name, f[4]);
    out.at = std::strtoull(f[5], nullptr, 10);
    if (nf < 14)
        return true; // written before the way was kept
    const unsigned kind = static_cast<unsigned>(std::strtoul(f[6], nullptr, 10));
    const unsigned way = static_cast<unsigned>(std::strtoul(f[7], nullptr, 10));
    const unsigned port = static_cast<unsigned>(std::strtoul(f[9], nullptr, 10));
    if (kind > static_cast<unsigned>(route::Kind::internet) || way > static_cast<unsigned>(route::Way::bridge) ||
        !f[8][0] || !port || port > 65535)
        return true; // the game, without a way that makes sense: start()'s address as before
    route::Candidate &w = out.way;
    w.kind = static_cast<route::Kind>(kind);
    w.way = static_cast<route::Way>(way);
    copy(w.addr, sizeof w.addr, f[8]);
    w.port = static_cast<std::uint16_t>(port);
    copy(w.path, sizeof w.path, f[10]);
    copy(w.room, sizeof w.room, f[11]);
    copy(w.fp, sizeof w.fp, f[12]);
    w.lobby = f[13][0] == '1';
    // a way through this home's PS5Upload needs the room it joins
    out.has_way = w.way != route::Way::bridge || w.room[0];
    return true;
}
} // namespace play::resume
