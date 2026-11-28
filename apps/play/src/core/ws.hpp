// PS5Upload Play - WebSocket (RFC 6455) client framing, handshake and SHA-1/Base64.
// Transport-free: the caller moves the bytes (net.cpp on the console, tests on the host).
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::ws
{
void sha1(const void *data, std::size_t len, std::uint8_t out[20]) noexcept;
// Base64 of `len` bytes into out (terminated); returns its length.
std::size_t base64(const std::uint8_t *data, std::size_t len, char *out, std::size_t cap) noexcept;

enum Op : std::uint8_t
{
    kCont = 0,
    kText = 1,
    kBinary = 2,
    kClose = 8,
    kPing = 9,
    kPong = 10,
};

// The client's upgrade request for `host` (host:port as it goes in the Host header) and `path`.
// `key16`: 16 random bytes. Writes the Sec-WebSocket-Key it used into key_b64 (25+ bytes).
std::size_t handshake_request(char *out, std::size_t cap, const char *host, const char *path,
                              const std::uint8_t key16[16], char *key_b64) noexcept;

// Checks a server's answer (the bytes up to and including the blank line): 101 and the right
// Sec-WebSocket-Accept for key_b64. `status` gets the HTTP status (0 when unreadable).
bool handshake_ok(const char *response, std::size_t len, const char *key_b64, int *status) noexcept;
// Where the header block ends in buf (index after "\r\n\r\n"), or 0 when not yet whole.
std::size_t header_end(const char *buf, std::size_t len) noexcept;

// One client frame (always masked) into out: header then masked payload. Returns its size, or 0
// when it does not fit.
std::size_t frame(std::uint8_t *out, std::size_t cap, Op op, const void *payload, std::size_t len,
                  const std::uint8_t mask[4]) noexcept;
constexpr std::size_t kMaxHeader = 14;

// Incremental reader of server frames. Feed it bytes; it hands back whole messages
// (continuation frames joined) and control frames. The message buffer is the caller's.
class Reader
{
  public:
    Reader(std::uint8_t *buf, std::size_t cap) noexcept : buf_{buf}, cap_{cap}
    {
    }
    enum class Got
    {
        need_more,  // nothing whole yet: all of `in` was taken
        message,    // a text/binary message: op(), data(), size()
        control,    // a ping/pong/close: op(), data(), size()
        too_big,    // a message larger than the buffer (the connection should end)
        bad,        // a protocol error (masked server frame, bad opcode...)
    };
    // Takes bytes from in (up to len); *used says how many it took. Call again with the rest
    // after a message or control frame.
    Got feed(const std::uint8_t *in, std::size_t len, std::size_t *used) noexcept;
    Op op() const noexcept
    {
        return op_;
    }
    const std::uint8_t *data() const noexcept
    {
        return out_;
    }
    std::size_t size() const noexcept
    {
        return out_len_;
    }
    void reset() noexcept;

  private:
    std::uint8_t *buf_;
    std::size_t cap_;
    // the frame being read
    std::uint8_t hdr_[kMaxHeader];
    std::size_t hdr_have_ = 0, hdr_need_ = 2;
    bool in_payload_ = false;
    std::uint64_t pay_len_ = 0, pay_have_ = 0;
    Op frame_op_ = kCont;
    bool fin_ = false;
    // the message being joined
    std::size_t msg_len_ = 0;
    Op msg_op_ = kCont;
    bool in_message_ = false;
    bool skipping_ = false; // a message too big: its frames are read past
    // a control frame's own small buffer
    std::uint8_t ctl_[125];
    // the result
    Op op_ = kCont;
    const std::uint8_t *out_ = nullptr;
    std::size_t out_len_ = 0;
};
} // namespace play::ws
