// PS5Upload Play - the ways to one engine (route_pick.hpp).
// SPDX-License-Identifier: GPL-3.0-or-later
#include "route_pick.hpp"

#include "../core/proto.hpp"

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

// The bridge is in engines from 13.0.0 (/api/join/bridge).
constexpr std::uint32_t kBridgeVersion = 13000000;
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

bool own_engine(const net::Answer *found, int n_found, char *addr, std::size_t cap, std::uint16_t *port) noexcept
{
    int best = -1;
    std::uint32_t best_v = 0;
    for (int i = 0; i < n_found; ++i)
    {
        if (found[i].tailnet || !found[i].info.port)
            continue;
        char ip[16];
        net::ip_text(found[i].ip_be, ip);
#ifndef PLAY_TEST_LOOPBACK // play-cli's end-to-end test: the stand-in engine is on this computer
        if (!std::strncmp(ip, "127.", 4))
            continue; // the helper's forward is a way to someone else's engine
#endif
        const std::uint32_t v = proto::version_number(found[i].info.version);
        if (best < 0 || (v >= kBridgeVersion && best_v < kBridgeVersion))
            best = i, best_v = v;
    }
    if (best < 0)
        return false;
    char ip[16];
    net::ip_text(found[best].ip_be, ip);
    std::snprintf(addr, cap, "%s", ip);
    *port = found[best].info.port;
    return true;
}

int handed_ways(const char *addr, std::uint16_t port, bool tailnet, const char *path, const char *room,
                const char *fp, const char *own_addr, std::uint16_t own_port, route::Candidate *out, int n,
                int cap) noexcept
{
    if (addr && addr[0] && n < cap && !listed(out, n, addr, port))
    {
        route::Candidate c;
        std::snprintf(c.addr, sizeof c.addr, "%s", addr);
        c.port = port;
        c.kind = tailnet ? route::Kind::tailscale : route::kind_of_addr(addr);
        c.way = tailnet ? route::Way::helper : route::Way::direct;
        std::snprintf(c.path, sizeof c.path, "%s", path ? path : "");
        out[n++] = c;
    }
    route::Candidate b;
    if (n < cap && room && room[0] && route_bridge(own_addr, own_port, room, fp, b))
        out[n++] = b;
    return n;
}

int invite_ways(const invite::Invite &inv, const char *own_addr, std::uint16_t own_port, route::Candidate *out,
                int cap) noexcept
{
    int n = 0;
    if (inv.n_routes)
    {
        const invite::Route &r = inv.routes[0];
        n = handed_ways(r.addr, r.port, r.tailnet, r.path, nullptr, nullptr, nullptr, 0, out, n, cap);
    }
    return handed_ways(nullptr, 0, false, nullptr, inv.room, inv.fp, own_addr, own_port, out, n, cap);
}

int again_ways(const Friend &f, bool direct, const char *own_addr, std::uint16_t own_port, route::Candidate *out,
               int cap) noexcept
{
    const int n = direct ? handed_ways(f.addr, f.port, f.tailnet, f.path, nullptr, nullptr, nullptr, 0, out, 0, cap) : 0;
    return f.key[0] ? handed_ways(nullptr, 0, false, nullptr, f.room, f.fp, own_addr, own_port, out, n, cap) : n;
}
} // namespace play
