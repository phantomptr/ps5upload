// PS5Upload Play - sockets: a WebSocket client over TCP and the UDP discovery probe.
// Plain BSD sockets: the same code runs on the console and on a computer (tests, play-cli).
// The app only ever dials out; nothing here listens.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "../core/proto.hpp"
#include "../core/ws.hpp"

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <pthread.h>

namespace play::net
{
void init() noexcept;
std::uint64_t now_us() noexcept;
// The console's clock (seconds since 1970), for what is kept across runs.
std::uint64_t wall_seconds() noexcept;
void sleep_ms(unsigned ms) noexcept;
void random_bytes(std::uint8_t *out, std::size_t n) noexcept;

// IPv4 only (the console's home network). addr: dotted quad or a name the resolver knows.
bool resolve(const char *addr, std::uint32_t *ip_be) noexcept;
void ip_text(std::uint32_t ip_be, char out[16]) noexcept;

class WsClient
{
  public:
    // buf: where whole messages are put together (big enough for a keyframe).
    WsClient(std::uint8_t *buf, std::size_t cap) noexcept;
    ~WsClient();
    // Connects and upgrades (timeout for each step). status: the HTTP status when it was not 101.
    bool open(const char *addr, std::uint16_t port, const char *path, unsigned timeout_ms,
              int *status = nullptr) noexcept;
    void close() noexcept;
    bool is_open() const noexcept
    {
        return fd_ >= 0;
    }
    // Thread-safe (one send at a time). False when the socket is gone.
    bool send(ws::Op op, const void *data, std::size_t len) noexcept;
    bool send_text(const char *s) noexcept;
    // Takes over `other`'s open connection, with what it has read but not handed out yet (call
    // right after other.next() gave a whole message). This client's own connection is closed.
    // Changing way mid-game (docs/ROUTES.md): the new socket was opened and joined beside this one.
    void adopt(WsClient &other) noexcept;

    enum class Got
    {
        message, // text or binary in op()/data()/size()
        timeout, // nothing within the time given
        closed,  // the other side closed, or an error
    };
    // Waits up to timeout_ms for the next message; answers pings itself. One reader thread.
    Got next(unsigned timeout_ms) noexcept;
    ws::Op op() const noexcept
    {
        return reader_.op();
    }
    const std::uint8_t *data() const noexcept
    {
        return reader_.data();
    }
    std::size_t size() const noexcept
    {
        return reader_.size();
    }
    const char *error() const noexcept
    {
        return err_;
    }
    std::uint64_t bytes_in() const noexcept
    {
        return bytes_in_;
    }

  private:
    int fd_ = -1;
    ws::Reader reader_;
    pthread_mutex_t send_lock_;
    std::uint8_t in_[64 * 1024];
    std::size_t in_at_ = 0, in_len_ = 0;
    std::uint8_t out_[16 * 1024];
    const char *err_ = "";
    std::uint64_t bytes_in_ = 0;
};

// Discovery: one probe to the broadcast address and to each of `unicast`, answers for
// timeout_ms. Calls found(ip, port, info) for each engine that answered (once per address).
struct Answer
{
    std::uint32_t ip_be;
    proto::Found info;
    bool tailnet = false; // from the helper's mesh: a Tailscale address (reached through the helper)
};
int discover(const std::uint32_t *unicast, int n_unicast, unsigned timeout_ms, Answer *out,
             int cap) noexcept;

// A plain HTTP request (no keep-alive): the answer's body into out (terminated; out also holds the
// headers while reading, so give it room); returns the status or -1. body: JSON, or null.
int http_request(const char *method, const char *addr, std::uint16_t port, const char *path, const char *body,
                 char *out, std::size_t cap, unsigned timeout_ms) noexcept;
// GET.
int http_get(const char *addr, std::uint16_t port, const char *path, char *out, std::size_t cap,
             unsigned timeout_ms) noexcept;
} // namespace play::net
