// PS5Upload Play - the ways to one engine (route_pick.hpp).
// SPDX-License-Identifier: GPL-3.0-or-later
#include "route_pick.hpp"

#include <cstdio>
#include <cstring>

namespace play
{
namespace
{
bool same_engine(const net::Answer &a, const char *name, const char *id) noexcept
{
    if (id && id[0] && a.info.id[0])
        return !std::strcmp(a.info.id, id);
    return name && name[0] && !std::strcmp(a.info.name, name);
}

bool listed(const route::Candidate *out, int n, const char *addr, std::uint16_t port) noexcept
{
    for (int i = 0; i < n; ++i)
        if (out[i].port == port && !std::strcmp(out[i].addr, addr))
            return true;
    return false;
}
} // namespace

int route_candidates(const net::Answer *found, int n_found, const char *picked_name, const char *picked_id,
                     const char *picked_addr, std::uint16_t picked_port, route::Candidate *out, int cap) noexcept
{
    int n = 0;
    for (int i = 0; i < n_found && n < cap; ++i)
    {
        if (!same_engine(found[i], picked_name, picked_id))
            continue;
        route::Candidate c;
        net::ip_text(found[i].ip_be, c.addr);
        c.port = found[i].info.port;
        if (found[i].tailnet)
        {
            c.kind = route::Kind::tailscale;
            c.way = route::Way::helper;
        }
        else
        {
            c.kind = route::kind_of_addr(c.addr);
            c.way = route::Way::direct;
        }
        if (!listed(out, n, c.addr, c.port))
            out[n++] = c;
    }
    const bool forward = picked_addr && !std::strcmp(picked_addr, "127.0.0.1") && n > 0;
    if (picked_addr && picked_addr[0] && !forward && n < cap && !listed(out, n, picked_addr, picked_port))
    {
        route::Candidate c;
        std::snprintf(c.addr, sizeof c.addr, "%s", picked_addr);
        c.port = picked_port;
        c.kind = route::kind_of_addr(picked_addr);
        out[n++] = c;
    }
    return n;
}

bool route_bridge(const char *bridge_addr, std::uint16_t bridge_port, const char *room, const char *fp,
                  route::Candidate &out) noexcept
{
    if (!bridge_addr || !bridge_addr[0] || !room || std::strlen(room) != 22 || !fp || !fp[0] ||
        std::strlen(fp) >= sizeof out.fp)
        return false;
    out = route::Candidate{};
    out.kind = route::Kind::internet;
    out.way = route::Way::bridge;
    std::snprintf(out.addr, sizeof out.addr, "%s", bridge_addr);
    out.port = bridge_port;
    std::snprintf(out.room, sizeof out.room, "%s", room);
    std::snprintf(out.fp, sizeof out.fp, "%s", fp);
    return true;
}
} // namespace play
