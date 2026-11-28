// PS5Upload Play - WebSocket (RFC 6455) client framing, handshake, SHA-1 (RFC 3174), Base64.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "ws.hpp"

#include <cstdio>
#include <cstring>

namespace play::ws
{
namespace
{
std::uint32_t rol(std::uint32_t v, int n) noexcept
{
    return (v << n) | (v >> (32 - n));
}

char lower(char c) noexcept
{
    return c >= 'A' && c <= 'Z' ? static_cast<char>(c + 32) : c;
}

// Case-insensitive search for a header name at a line start; returns its value (trimmed) or null.
const char *find_header(const char *r, std::size_t len, const char *name, std::size_t *vlen) noexcept
{
    const std::size_t nl = std::strlen(name);
    for (std::size_t i = 0; i + nl + 1 < len; ++i)
    {
        if (i != 0 && r[i - 1] != '\n')
            continue;
        bool same = true;
        for (std::size_t k = 0; k < nl && same; ++k)
            same = lower(r[i + k]) == lower(name[k]);
        if (!same || r[i + nl] != ':')
            continue;
        std::size_t v = i + nl + 1;
        while (v < len && (r[v] == ' ' || r[v] == '\t'))
            ++v;
        std::size_t e = v;
        while (e < len && r[e] != '\r' && r[e] != '\n')
            ++e;
        while (e > v && (r[e - 1] == ' ' || r[e - 1] == '\t'))
            --e;
        *vlen = e - v;
        return r + v;
    }
    return nullptr;
}
} // namespace

void sha1(const void *data, std::size_t len, std::uint8_t out[20]) noexcept
{
    std::uint32_t h[5] = {0x67452301u, 0xEFCDAB89u, 0x98BADCFEu, 0x10325476u, 0xC3D2E1F0u};
    const auto block = [&h](const std::uint8_t *b) {
        std::uint32_t w[80];
        for (int t = 0; t < 16; ++t)
            w[t] = static_cast<std::uint32_t>(b[t * 4]) << 24 | static_cast<std::uint32_t>(b[t * 4 + 1]) << 16 |
                   static_cast<std::uint32_t>(b[t * 4 + 2]) << 8 | b[t * 4 + 3];
        for (int t = 16; t < 80; ++t)
            w[t] = rol(w[t - 3] ^ w[t - 8] ^ w[t - 14] ^ w[t - 16], 1);
        std::uint32_t a = h[0], bb = h[1], c = h[2], d = h[3], e = h[4];
        for (int t = 0; t < 80; ++t)
        {
            std::uint32_t f, k;
            if (t < 20)
                f = (bb & c) | (~bb & d), k = 0x5A827999u;
            else if (t < 40)
                f = bb ^ c ^ d, k = 0x6ED9EBA1u;
            else if (t < 60)
                f = (bb & c) | (bb & d) | (c & d), k = 0x8F1BBCDCu;
            else
                f = bb ^ c ^ d, k = 0xCA62C1D6u;
            const std::uint32_t tmp = rol(a, 5) + f + e + k + w[t];
            e = d;
            d = c;
            c = rol(bb, 30);
            bb = a;
            a = tmp;
        }
        h[0] += a, h[1] += bb, h[2] += c, h[3] += d, h[4] += e;
    };
    const auto *p = static_cast<const std::uint8_t *>(data);
    std::size_t done = 0;
    for (; len - done >= 64; done += 64)
        block(p + done);
    // the tail, 0x80, zeros and the length in bits: one block or two
    std::uint8_t last[128] = {};
    const std::size_t rest = len - done;
    std::memcpy(last, p + done, rest);
    last[rest] = 0x80;
    const std::size_t total = rest < 56 ? 64 : 128;
    const std::uint64_t bits = static_cast<std::uint64_t>(len) * 8;
    for (int k = 0; k < 8; ++k)
        last[total - 1 - k] = static_cast<std::uint8_t>(bits >> (8 * k));
    block(last);
    if (total == 128)
        block(last + 64);
    for (int i = 0; i < 5; ++i)
        for (int k = 0; k < 4; ++k)
            out[i * 4 + k] = static_cast<std::uint8_t>(h[i] >> (24 - 8 * k));
}

std::size_t base64(const std::uint8_t *d, std::size_t len, char *out, std::size_t cap) noexcept
{
    static const char t[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    std::size_t o = 0;
    for (std::size_t i = 0; i < len; i += 3)
    {
        const std::uint32_t v = static_cast<std::uint32_t>(d[i]) << 16 |
                                (i + 1 < len ? d[i + 1] << 8 : 0) | (i + 2 < len ? d[i + 2] : 0);
        if (o + 5 > cap)
            break;
        out[o++] = t[v >> 18 & 63];
        out[o++] = t[v >> 12 & 63];
        out[o++] = i + 1 < len ? t[v >> 6 & 63] : '=';
        out[o++] = i + 2 < len ? t[v & 63] : '=';
    }
    if (o < cap)
        out[o] = 0;
    return o;
}

std::size_t handshake_request(char *out, std::size_t cap, const char *host, const char *path,
                              const std::uint8_t key16[16], char *key_b64) noexcept
{
    base64(key16, 16, key_b64, 32);
    const int n = std::snprintf(out, cap,
                                "GET %s HTTP/1.1\r\n"
                                "Host: %s\r\n"
                                "Upgrade: websocket\r\n"
                                "Connection: Upgrade\r\n"
                                "Sec-WebSocket-Key: %s\r\n"
                                "Sec-WebSocket-Version: 13\r\n"
                                "User-Agent: PS5UploadPlay\r\n"
                                "\r\n",
                                path, host, key_b64);
    return n > 0 && static_cast<std::size_t>(n) < cap ? static_cast<std::size_t>(n) : 0;
}

std::size_t header_end(const char *buf, std::size_t len) noexcept
{
    for (std::size_t i = 3; i < len; ++i)
        if (buf[i - 3] == '\r' && buf[i - 2] == '\n' && buf[i - 1] == '\r' && buf[i] == '\n')
            return i + 1;
    return 0;
}

bool handshake_ok(const char *r, std::size_t len, const char *key_b64, int *status) noexcept
{
    *status = 0;
    if (len < 12 || std::memcmp(r, "HTTP/1.1 ", 9) != 0)
        return false;
    *status = (r[9] - '0') * 100 + (r[10] - '0') * 10 + (r[11] - '0');
    if (*status != 101)
        return false;
    char concat[96];
    const int n = std::snprintf(concat, sizeof concat, "%s258EAFA5-E914-47DA-95CA-C5AB0DC85B11", key_b64);
    std::uint8_t digest[20];
    sha1(concat, static_cast<std::size_t>(n), digest);
    char want[32];
    const std::size_t wl = base64(digest, 20, want, sizeof want);
    std::size_t vl = 0;
    const char *v = find_header(r, len, "Sec-WebSocket-Accept", &vl);
    return v && vl == wl && std::memcmp(v, want, wl) == 0;
}

std::size_t frame(std::uint8_t *out, std::size_t cap, Op op, const void *payload, std::size_t len,
                  const std::uint8_t mask[4]) noexcept
{
    std::size_t h = 2;
    if (len >= 126)
        h += len > 0xFFFF ? 8 : 2;
    h += 4;
    if (h + len > cap)
        return 0;
    out[0] = static_cast<std::uint8_t>(0x80 | op);
    if (len < 126)
        out[1] = static_cast<std::uint8_t>(0x80 | len);
    else if (len <= 0xFFFF)
    {
        out[1] = 0x80 | 126;
        out[2] = static_cast<std::uint8_t>(len >> 8);
        out[3] = static_cast<std::uint8_t>(len);
    }
    else
    {
        out[1] = 0x80 | 127;
        for (int k = 0; k < 8; ++k)
            out[2 + k] = static_cast<std::uint8_t>(static_cast<std::uint64_t>(len) >> (56 - 8 * k));
    }
    std::memcpy(out + h - 4, mask, 4);
    const auto *p = static_cast<const std::uint8_t *>(payload);
    for (std::size_t i = 0; i < len; ++i)
        out[h + i] = p[i] ^ mask[i & 3];
    return h + len;
}

void Reader::reset() noexcept
{
    hdr_have_ = 0;
    hdr_need_ = 2;
    in_payload_ = false;
    pay_len_ = pay_have_ = 0;
    msg_len_ = 0;
    in_message_ = false;
    skipping_ = false;
    out_ = nullptr;
    out_len_ = 0;
}

Reader::Got Reader::feed(const std::uint8_t *in, std::size_t len, std::size_t *used) noexcept
{
    std::size_t i = 0;
    *used = 0;
    while (i < len)
    {
        if (!in_payload_)
        {
            while (hdr_have_ < hdr_need_ && i < len)
                hdr_[hdr_have_++] = in[i++];
            if (hdr_have_ < hdr_need_)
                break;
            if (hdr_need_ == 2)
            {
                if (hdr_[1] & 0x80)
                {
                    *used = i;
                    return Got::bad; // a server never masks
                }
                const std::uint8_t l = hdr_[1] & 0x7F;
                hdr_need_ = 2 + (l == 126 ? 2 : l == 127 ? 8 : 0);
                if (hdr_have_ < hdr_need_)
                    continue;
            }
            const std::uint8_t l = hdr_[1] & 0x7F;
            if (l < 126)
                pay_len_ = l;
            else if (l == 126)
                pay_len_ = static_cast<std::uint64_t>(hdr_[2]) << 8 | hdr_[3];
            else
            {
                pay_len_ = 0;
                for (int k = 0; k < 8; ++k)
                    pay_len_ = pay_len_ << 8 | hdr_[2 + k];
            }
            fin_ = hdr_[0] & 0x80;
            frame_op_ = static_cast<Op>(hdr_[0] & 0x0F);
            pay_have_ = 0;
            in_payload_ = true;
            hdr_have_ = 0;
            hdr_need_ = 2;
            const bool control = frame_op_ >= 8;
            if (control)
            {
                if (pay_len_ > 125 || !fin_ || (frame_op_ != kClose && frame_op_ != kPing && frame_op_ != kPong))
                {
                    *used = i;
                    return Got::bad;
                }
            }
            else if (frame_op_ == kCont)
            {
                if (!in_message_)
                {
                    *used = i;
                    return Got::bad;
                }
            }
            else if (frame_op_ == kText || frame_op_ == kBinary)
            {
                if (in_message_)
                {
                    *used = i;
                    return Got::bad;
                }
                in_message_ = true;
                msg_op_ = frame_op_;
                msg_len_ = 0;
                skipping_ = false;
            }
            else
            {
                *used = i;
                return Got::bad;
            }
            if (!control && msg_len_ + pay_len_ > cap_)
                skipping_ = true;
        }
        // payload bytes
        const bool control = frame_op_ >= 8;
        const std::uint64_t want = pay_len_ - pay_have_;
        const std::size_t take = want < len - i ? static_cast<std::size_t>(want) : len - i;
        if (control)
            std::memcpy(ctl_ + pay_have_, in + i, take);
        else if (!skipping_)
            std::memcpy(buf_ + msg_len_ + pay_have_, in + i, take);
        pay_have_ += take;
        i += take;
        if (pay_have_ < pay_len_)
            break;
        // frame complete
        in_payload_ = false;
        if (control)
        {
            op_ = frame_op_;
            out_ = ctl_;
            out_len_ = static_cast<std::size_t>(pay_len_);
            *used = i;
            return Got::control;
        }
        if (!skipping_)
            msg_len_ += static_cast<std::size_t>(pay_len_);
        if (fin_)
        {
            in_message_ = false;
            if (skipping_)
            {
                skipping_ = false;
                msg_len_ = 0;
                *used = i;
                return Got::too_big;
            }
            op_ = msg_op_;
            out_ = buf_;
            out_len_ = msg_len_;
            msg_len_ = 0;
            *used = i;
            return Got::message;
        }
    }
    *used = i;
    return Got::need_more;
}
} // namespace play::ws
