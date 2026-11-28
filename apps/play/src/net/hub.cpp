// PS5Upload Play - the hub client: WebSocket (core/ws) over TCP, or over TLS for wss://.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "hub.hpp"

#include "net.hpp"

#include <arpa/inet.h>
#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <sys/socket.h>
#include <unistd.h>

#ifndef PLAY_PS5
#include <fcntl.h>
#endif

namespace play::net
{
namespace
{
#ifdef MSG_NOSIGNAL
constexpr int kSendFlags = MSG_NOSIGNAL;
#else
constexpr int kSendFlags = 0;
#endif

void nonblocking(int fd, bool on) noexcept
{
#ifdef PLAY_PS5
    int v = on ? 1 : 0;
    setsockopt(fd, SOL_SOCKET, 0x1200 /* SO_NBIO */, &v, sizeof v);
#else
    const int fl = fcntl(fd, F_GETFL, 0);
    fcntl(fd, F_SETFL, on ? (fl | O_NONBLOCK) : (fl & ~O_NONBLOCK));
#endif
}

int dial(std::uint32_t ip_be, std::uint16_t port, unsigned timeout_ms, const char **err) noexcept
{
    const int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0)
    {
        *err = "socket";
        return -1;
    }
#ifdef SO_NOSIGPIPE
    int one_ = 1;
    setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one_, sizeof one_);
#endif
    int one = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
    sockaddr_in a{};
    a.sin_family = AF_INET;
    a.sin_port = htons(port);
    a.sin_addr.s_addr = ip_be;
    nonblocking(fd, true);
    int rc = connect(fd, reinterpret_cast<sockaddr *>(&a), sizeof a);
    if (rc != 0 && errno != EINPROGRESS && errno != EWOULDBLOCK && errno != EALREADY)
    {
        *err = "connect";
        ::close(fd);
        return -1;
    }
    if (rc != 0)
    {
        pollfd p{fd, POLLOUT, 0};
        rc = poll(&p, 1, static_cast<int>(timeout_ms));
        int soerr = 0;
        socklen_t sl = sizeof soerr;
        if (rc <= 0 || getsockopt(fd, SOL_SOCKET, SO_ERROR, &soerr, &sl) != 0 || soerr != 0)
        {
            *err = rc == 0 ? "timeout" : "connect";
            ::close(fd);
            return -1;
        }
    }
    nonblocking(fd, false);
    return fd;
}
} // namespace

bool split_ws_url(const char *url, bool *tls, char *host, std::size_t host_cap, std::uint16_t *port, char *path,
                  std::size_t path_cap) noexcept
{
    const char *s;
    if (!std::strncmp(url, "wss://", 6))
        s = url + 6, *tls = true, *port = 443;
    else if (!std::strncmp(url, "ws://", 5))
        s = url + 5, *tls = false, *port = 80;
    else
        return false;
    std::size_t h = 0;
    while (s[h] && s[h] != ':' && s[h] != '/')
        ++h;
    if (!h || h >= host_cap)
        return false;
    for (std::size_t i = 0; i < h; ++i)
    {
        const char c = s[i];
        if (!((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '.' || c == '-'))
            return false;
    }
    std::memcpy(host, s, h);
    host[h] = 0;
    s += h;
    if (*s == ':')
    {
        char *end = nullptr;
        const long p = std::strtol(s + 1, &end, 10);
        if (p < 1 || p > 65535)
            return false;
        *port = static_cast<std::uint16_t>(p);
        s = end;
    }
    const int n = std::snprintf(path, path_cap, "%s", *s ? s : "/");
    return n > 0 && static_cast<std::size_t>(n) < path_cap && path[0] == '/';
}

HubLink::HubLink() noexcept : reader_{rbuf_, sizeof rbuf_} {}

HubLink::~HubLink()
{
    close();
}

void HubLink::close() noexcept
{
    if (fd_ >= 0)
    {
        shutdown(fd_, SHUT_RDWR);
        ::close(fd_);
        fd_ = -1;
    }
    tls_.reset();
}

bool HubLink::raw_write(const void *d, std::size_t n) noexcept
{
    if (tls_on_)
        return tls_.write(d, n, 5000);
    const auto *p = static_cast<const std::uint8_t *>(d);
    while (n)
    {
        pollfd pf{fd_, POLLOUT, 0};
        if (poll(&pf, 1, 5000) <= 0)
            return false;
        const long w = ::send(fd_, p, n, kSendFlags);
        if (w <= 0)
        {
            if (w < 0 && (errno == EINTR || errno == EAGAIN))
                continue;
            return false;
        }
        p += w;
        n -= static_cast<std::size_t>(w);
    }
    return true;
}

long HubLink::raw_read(void *d, std::size_t cap, unsigned timeout_ms) noexcept
{
    if (tls_on_)
        return tls_.read(d, cap, timeout_ms);
    pollfd pf{fd_, POLLIN, 0};
    const int r = poll(&pf, 1, static_cast<int>(timeout_ms));
    if (r == 0)
        return 0;
    if (r < 0)
        return errno == EINTR ? 0 : -1;
    const long n = ::recv(fd_, d, cap, 0);
    return n > 0 ? n : -1;
}

bool HubLink::open(const char *url, unsigned timeout_ms) noexcept
{
    close();
    bytes_in = bytes_out = 0;
    msgs_in = msgs_out = 0;
    char host[96], path[96];
    std::uint16_t port = 0;
    if (!split_ws_url(url, &tls_on_, host, sizeof host, &port, path, sizeof path))
    {
        err_ = "url";
        return false;
    }
    std::uint32_t ip = 0;
    if (!resolve_name(host, &ip))
    {
        err_ = dns_error();
        return false;
    }
    const int fd = dial(ip, port, timeout_ms, &err_);
    if (fd < 0)
        return false;
    fd_ = fd;
    if (tls_on_ && !tls_.start(fd_, host, timeout_ms))
    {
        err_ = tls_.error();
        close();
        return false;
    }
    char hosthdr[112], key[32];
    if ((tls_on_ && port == 443) || (!tls_on_ && port == 80))
        std::snprintf(hosthdr, sizeof hosthdr, "%s", host);
    else
        std::snprintf(hosthdr, sizeof hosthdr, "%s:%u", host, static_cast<unsigned>(port));
    std::uint8_t k16[16];
    if (!secure_random(k16, sizeof k16))
        random_bytes(k16, sizeof k16); // the key is not a secret, only a check
    char req[512];
    const std::size_t rl = ws::handshake_request(req, sizeof req, hosthdr, path, k16, key);
    if (!rl || !raw_write(req, rl))
    {
        err_ = "send";
        close();
        return false;
    }
    char resp[2048];
    std::size_t have = 0, end = 0;
    const std::uint64_t until = now_us() + timeout_ms * 1000ull;
    while (!end)
    {
        const std::uint64_t now = now_us();
        if (now >= until)
        {
            err_ = "timeout";
            close();
            return false;
        }
        const long n = raw_read(resp + have, sizeof resp - 1 - have, static_cast<unsigned>((until - now) / 1000 + 1));
        if (n < 0)
        {
            err_ = "closed";
            close();
            return false;
        }
        have += static_cast<std::size_t>(n);
        end = ws::header_end(resp, have);
        if (!end && have >= sizeof resp - 1)
        {
            err_ = "header";
            close();
            return false;
        }
    }
    int st = 0;
    if (!ws::handshake_ok(resp, end, key, &st))
    {
        err_ = st == 429 ? "busy" : st == 503 ? "full" : "upgrade";
        close();
        return false;
    }
    reader_.reset();
    in_len_ = have - end;
    in_at_ = 0;
    std::memcpy(in_, resp + end, in_len_);
    bytes_in += have;
    bytes_out += rl;
    err_ = "";
    return true;
}

bool HubLink::send_frame(ws::Op op, const void *data, std::size_t len) noexcept
{
    if (fd_ < 0)
        return false;
    static_assert(sizeof(msg_) > 16 * 1024, "room for a frame");
    std::uint8_t f[16 * 1024 + ws::kMaxHeader];
    std::uint8_t mask[4];
    random_bytes(mask, sizeof mask);
    const std::size_t n = ws::frame(f, sizeof f, op, data, len, mask);
    if (!n || !raw_write(f, n))
    {
        err_ = n ? "send" : "too_big";
        if (n)
            close();
        return false;
    }
    bytes_out += n;
    return true;
}

bool HubLink::send_json(const char *text) noexcept
{
    const bool ok = send_frame(ws::kText, text, std::strlen(text));
    msgs_out += ok;
    return ok;
}

HubLink::Got HubLink::next(unsigned timeout_ms) noexcept
{
    const std::uint64_t until = now_us() + timeout_ms * 1000ull;
    for (;;)
    {
        if (fd_ < 0)
            return Got::closed;
        while (in_at_ < in_len_)
        {
            std::size_t used = 0;
            const auto g = reader_.feed(in_ + in_at_, in_len_ - in_at_, &used);
            in_at_ += used;
            if (g == ws::Reader::Got::need_more)
                continue;
            if (g == ws::Reader::Got::too_big || g == ws::Reader::Got::bad)
            {
                err_ = "frame";
                close();
                return Got::closed;
            }
            if (g == ws::Reader::Got::control)
            {
                if (reader_.op() == ws::kPing)
                    send_frame(ws::kPong, reader_.data(), reader_.size());
                else if (reader_.op() == ws::kClose)
                {
                    err_ = "closed";
                    close();
                    return Got::closed;
                }
                continue;
            }
            if (reader_.op() != ws::kText)
                continue; // the hub only sends text
            msg_len_ = reader_.size();
            std::memcpy(msg_, reader_.data(), msg_len_);
            msg_[msg_len_] = 0;
            ++msgs_in;
            return Got::message;
        }
        const std::uint64_t now = now_us();
        if (now >= until)
            return Got::timeout;
        const long n = raw_read(in_, sizeof in_, static_cast<unsigned>((until - now) / 1000 + 1));
        if (n < 0)
        {
            err_ = tls_on_ ? tls_.error() : "closed";
            close();
            return Got::closed;
        }
        in_at_ = 0;
        in_len_ = static_cast<std::size_t>(n);
        bytes_in += in_len_;
    }
}
} // namespace play::net
