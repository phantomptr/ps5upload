// PS5Upload Play - a JSON-over-WebSocket client for the signalling hub (protocol v1,
// engine/crates/ps5upload-hub/PROTOCOL.md): `wss://` through TLS (tls.hpp), `ws://` for tests.
// Small on purpose (docs/EASY_JOIN.md, "Interfaces"), so other work can use it: open, send one
// JSON text, wait for the next one, close. It answers the hub's pings itself. One thread at a time.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "../core/ws.hpp"
#include "tls.hpp"

#include <cstddef>
#include <cstdint>

namespace play::net
{
class HubLink
{
  public:
    HubLink() noexcept;
    ~HubLink();
    HubLink(const HubLink &) = delete;
    HubLink &operator=(const HubLink &) = delete;

    // url: "wss://host[:port]/path" or "ws://host[:port]/path".
    bool open(const char *url, unsigned timeout_ms) noexcept;
    void close() noexcept;
    bool is_open() const noexcept
    {
        return fd_ >= 0;
    }
    // One text frame (JSON). False when the link is gone.
    bool send_json(const char *text) noexcept;

    enum class Got
    {
        message, // text() / size(): one JSON text from the hub (terminated)
        timeout, // nothing within the time
        closed,  // the hub closed it, or an error (error())
    };
    Got next(unsigned timeout_ms) noexcept;
    const char *text() const noexcept
    {
        return reinterpret_cast<const char *>(msg_);
    }
    std::size_t size() const noexcept
    {
        return msg_len_;
    }
    const char *error() const noexcept
    {
        return err_;
    }
    // Bytes and messages both ways since open (the hub's load, for the log).
    std::uint64_t bytes_in = 0, bytes_out = 0;
    unsigned msgs_in = 0, msgs_out = 0;

  private:
    bool raw_write(const void *d, std::size_t n) noexcept;
    long raw_read(void *d, std::size_t cap, unsigned timeout_ms) noexcept;
    bool send_frame(ws::Op op, const void *data, std::size_t len) noexcept;

    int fd_ = -1;
    bool tls_on_ = false;
    Tls tls_;
    // The hub's frames are at most 16 KiB.
    std::uint8_t rbuf_[20 * 1024];
    ws::Reader reader_;
    std::uint8_t in_[8 * 1024];
    std::size_t in_at_ = 0, in_len_ = 0;
    std::uint8_t msg_[20 * 1024 + 1];
    std::size_t msg_len_ = 0;
    const char *err_ = "";
};

// The parts of a hub URL: scheme, host, port, path. False when it is not ws:// or wss://.
bool split_ws_url(const char *url, bool *tls, char *host, std::size_t host_cap, std::uint16_t *port, char *path,
                  std::size_t path_cap) noexcept;
} // namespace play::net
