// PS5Upload Play - TLS 1.2 client over a connected socket (BearSSL, third_party/bearssl): the
// certificate is checked against the ISRG (Let's Encrypt) roots and the server's name, with the
// console's clock. Used for the hub (wss://), docs/EASY_JOIN.md.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::net
{
class Tls
{
  public:
    Tls() noexcept;
    ~Tls();
    Tls(const Tls &) = delete;
    Tls &operator=(const Tls &) = delete;
    // Starts the handshake on `fd` (blocking socket) for `server_name`; finishes it within
    // timeout_ms. False with error() set.
    bool start(int fd, const char *server_name, unsigned timeout_ms) noexcept;
    // Writes all of it (and flushes) within timeout_ms.
    bool write(const void *data, std::size_t len, unsigned timeout_ms) noexcept;
    // Reads what is there (waits up to timeout_ms for some): >0 bytes, 0 nothing yet, -1 closed.
    long read(void *out, std::size_t cap, unsigned timeout_ms) noexcept;
    const char *error() const noexcept
    {
        return err_;
    }
    // BearSSL's last error code (0 none), for the log.
    int code() const noexcept;
    void reset() noexcept;

  private:
    // Moves records until the engine can take app data (`want_read` false) or has some for us
    // (true): 1, 0 on time out, -1 on an error.
    int pump(bool want_read, std::uint64_t until_us) noexcept;
    struct State;
    State *st_;
    int fd_ = -1;
    const char *err_ = "";
};

// Fills out with random bytes from the console's generator (sceRandom on the PS5, the system's
// on a computer). False when none could be had (the caller must not go on with weak ones).
bool secure_random(std::uint8_t *out, std::size_t n) noexcept;
// A name to an IPv4 address (dotted quads too): the console's resolver on the PS5.
bool resolve_name(const char *host, std::uint32_t *ip_be) noexcept;
// Why the last resolve_name failed ("dns" and the console's codes), for the log.
const char *dns_error() noexcept;
} // namespace play::net
