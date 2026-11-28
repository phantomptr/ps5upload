// PS5Upload Play - checking one way to the engine (route_probe.hpp).
// SPDX-License-Identifier: GPL-3.0-or-later
#include "route_probe.hpp"

#include "../core/json.hpp"

#include <cstdio>
#include <cstring>

namespace play::route
{
Probe::Probe() noexcept : ws_{buf_, sizeof buf_}
{
}

bool Probe::open(const char *addr, std::uint16_t port, unsigned timeout_ms, Meter &m, bool *no_socket,
                 const char *ws_path) noexcept
{
    *no_socket = false;
    in_burst_ = false;
    // "/prefix/api/join/ws" -> "/prefix/api/join/probe"
    char path[96] = "/api/join/probe";
    const std::size_t n = ws_path ? std::strlen(ws_path) : 0;
    if (n > 3 && n < 80 && !std::strcmp(ws_path + n - 3, "/ws"))
        std::snprintf(path, sizeof path, "%.*s/probe", static_cast<int>(n - 3), ws_path);
    const std::uint64_t t0 = net::now_us();
    int status = 0;
    if (ws_.open(addr, port, path, timeout_ms, &status))
        return true;
    if (status > 0)
    {
        // An engine without the checks socket still answered: the upgrade's round trip (a
        // connect and the request: about two) is one reading.
        *no_socket = true;
        m.rtt((net::now_us() - t0) / 2, net::now_us());
        return false;
    }
    return false;
}

bool Probe::check(Meter &m, std::uint64_t now) noexcept
{
    if (!ws_.is_open())
        return false;
    const std::uint32_t id = next_id_++;
    char t[48];
    std::snprintf(t, sizeof t, "{\"p\":%u}", static_cast<unsigned>(id));
    m.sent(id, now);
    return ws_.send_text(t);
}

bool Probe::handle(Meter &m) noexcept
{
    const std::uint64_t now = net::now_us();
    m.heard(now);
    if (ws_.op() == ws::kBinary)
    {
        if (in_burst_)
        {
            if (!burst_first_us_)
                burst_first_us_ = now;
            burst_bytes_ += ws_.size();
        }
        return true;
    }
    json::Token tok[16];
    json::Doc d(tok, 16);
    if (!d.parse(reinterpret_cast<const char *>(ws_.data()), ws_.size()))
        return true;
    const int p = d.get(0, "p");
    if (p >= 0)
        m.answered(static_cast<std::uint32_t>(d.integer(p)), now);
    if (d.get(0, "burst_done") >= 0 && in_burst_)
    {
        in_burst_ = false;
        burst_done_us_ = now;
    }
    return true;
}

bool Probe::poll(Meter &m, unsigned wait_ms) noexcept
{
    // next(0) would not look at the socket at all: 1 ms is the shortest real wait
    for (unsigned w = wait_ms ? wait_ms : 1;; w = 1)
    {
        if (!ws_.is_open())
            return false;
        const auto got = ws_.next(w);
        if (got == net::WsClient::Got::closed)
            return false;
        if (got == net::WsClient::Got::timeout)
            return true;
        handle(m);
    }
}

bool Probe::burst(Meter &m, unsigned bytes, unsigned timeout_ms) noexcept
{
    if (!ws_.is_open())
        return false;
    char t[48];
    std::snprintf(t, sizeof t, "{\"burst\":%u}", bytes);
    in_burst_ = true;
    burst_bytes_ = burst_first_us_ = burst_done_us_ = 0;
    burst_sent_us_ = net::now_us();
    if (!ws_.send_text(t))
        return false;
    const std::uint64_t until = burst_sent_us_ + timeout_ms * 1000ull;
    while (in_burst_)
    {
        const std::uint64_t now = net::now_us();
        if (now >= until)
            return false;
        const auto got = ws_.next(static_cast<unsigned>((until - now) / 1000 + 1));
        if (got == net::WsClient::Got::closed)
            return false;
        if (got == net::WsClient::Got::message)
            handle(m);
    }
    // From the ask to the last byte: the round trip the ask took is in it, as slow start is.
    const Reading r = m.reading(burst_done_us_);
    const std::uint64_t rtt_us = r.rtt_ms > 0 ? static_cast<std::uint64_t>(r.rtt_ms) * 1000 : 0;
    m.burst(burst_bytes_, burst_done_us_ - burst_sent_us_, rtt_us);
    return true;
}
} // namespace play::route
