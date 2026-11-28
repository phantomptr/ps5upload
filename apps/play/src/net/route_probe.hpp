// PS5Upload Play - checking one way to the engine over its checks socket (`/api/join/probe`,
// engine join_route.rs): small timed checks answered at once, and one short burst at connect.
// An engine without the socket (older than 13.0.0, or Play Together not open: HTTP 404/409) is
// timed by how long its answer to the upgrade takes instead.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "net.hpp"
#include "route.hpp"

#include <cstdint>

namespace play::route
{
class Probe
{
  public:
    Probe() noexcept;
    // Opens the checks socket. False when the way does not answer at all. `*no_socket`: the
    // engine answered but has no checks socket (one round trip was still measured into m).
    // ws_path: the guest socket's path when the engine is under a prefix (Candidate::path); the
    // checks socket is beside it. Null or "": /api/join/probe.
    bool open(const char *addr, std::uint16_t port, unsigned timeout_ms, Meter &m, bool *no_socket,
              const char *ws_path = nullptr) noexcept;
    bool is_open() const noexcept
    {
        return ws_.is_open();
    }
    void close() noexcept
    {
        ws_.close();
    }
    // One check out (into m as sent).
    bool check(Meter &m, std::uint64_t now) noexcept;
    // Reads answers for up to wait_ms (0: only what has come). False when the socket is gone.
    bool poll(Meter &m, unsigned wait_ms) noexcept;
    // The burst at connect: `bytes` asked, timed into m. False when it did not finish.
    bool burst(Meter &m, unsigned bytes, unsigned timeout_ms) noexcept;

  private:
    bool handle(Meter &m) noexcept;
    std::uint8_t buf_[20 * 1024];
    net::WsClient ws_;
    std::uint32_t next_id_ = 1;
    // a burst being read
    bool in_burst_ = false;
    std::uint64_t burst_bytes_ = 0, burst_first_us_ = 0, burst_done_us_ = 0;
    std::uint64_t burst_sent_us_ = 0;
};
} // namespace play::route
