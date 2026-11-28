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

// ---- short GETs (the rooms list, a friend's room, a preview) ----------------------------------

namespace
{
// "content-length: 12" style header value (case-insensitive name) in head; -1 when absent.
long header_num(const char *head, const char *name) noexcept
{
    const std::size_t nl = std::strlen(name);
    for (const char *p = head; *p; ++p)
    {
        if (p != head && p[-1] != '\n')
            continue;
        bool same = true;
        for (std::size_t i = 0; i < nl && same; ++i)
        {
            char c = p[i];
            if (c >= 'A' && c <= 'Z')
                c = static_cast<char>(c - 'A' + 'a');
            same = c == name[i];
        }
        if (same && p[nl] == ':')
            return std::strtol(p + nl + 1, nullptr, 10);
    }
    return -1;
}

bool header_has(const char *head, const char *needle) noexcept
{
    const std::size_t nl = std::strlen(needle);
    for (const char *p = head; *p; ++p)
    {
        std::size_t i = 0;
        while (i < nl && p[i])
        {
            char c = p[i];
            if (c >= 'A' && c <= 'Z')
                c = static_cast<char>(c - 'A' + 'a');
            if (c != needle[i])
                break;
            ++i;
        }
        if (i == nl)
            return true;
    }
    return false;
}
} // namespace

int HubLink::get(const char *url, char *out, std::size_t cap, unsigned timeout_ms, int *retry_after_s) noexcept
{
    close();
    if (retry_after_s)
        *retry_after_s = 0;
    if (cap < 64)
        return -1;
    out[0] = 0;
    // https:// and http:// as wss:// and ws:// for the parts
    char wsurl[320];
    if (!std::strncmp(url, "https://", 8))
        std::snprintf(wsurl, sizeof wsurl, "wss://%s", url + 8);
    else if (!std::strncmp(url, "http://", 7))
        std::snprintf(wsurl, sizeof wsurl, "ws://%s", url + 7);
    else
    {
        err_ = "url";
        return -1;
    }
    char host[96], path[256];
    std::uint16_t port = 0;
    if (!split_ws_url(wsurl, &tls_on_, host, sizeof host, &port, path, sizeof path))
    {
        err_ = "url";
        return -1;
    }
    std::uint32_t ip = 0;
    if (!resolve_name(host, &ip))
    {
        err_ = dns_error();
        return -1;
    }
    const int fd = dial(ip, port, timeout_ms, &err_);
    if (fd < 0)
        return -1;
    fd_ = fd;
    if (tls_on_ && !tls_.start(fd_, host, timeout_ms))
    {
        err_ = tls_.error();
        close();
        return -1;
    }
    char req[512];
    const bool default_port = (tls_on_ && port == 443) || (!tls_on_ && port == 80);
    char hosthdr[112];
    if (default_port)
        std::snprintf(hosthdr, sizeof hosthdr, "%s", host);
    else
        std::snprintf(hosthdr, sizeof hosthdr, "%s:%u", host, static_cast<unsigned>(port));
    const int rl = std::snprintf(req, sizeof req,
                                 "GET %s HTTP/1.1\r\nHost: %s\r\nUser-Agent: PS5UploadPlay\r\n"
                                 "Accept: application/json\r\nConnection: close\r\n\r\n",
                                 path, hosthdr);
    if (rl <= 0 || static_cast<std::size_t>(rl) >= sizeof req || !raw_write(req, static_cast<std::size_t>(rl)))
    {
        err_ = "send";
        close();
        return -1;
    }
    bytes_out += static_cast<std::uint64_t>(rl);
    // the whole answer (head and body) into out
    std::size_t have = 0;
    long body_at = -1, want = -1;
    bool chunked = false;
    int status = -1;
    const std::uint64_t until = now_us() + timeout_ms * 1000ull;
    for (;;)
    {
        if (body_at >= 0 && want >= 0 && have >= static_cast<std::size_t>(body_at + want))
            break;
        const std::uint64_t now = now_us();
        if (now >= until)
        {
            err_ = "timeout";
            close();
            return -1;
        }
        if (have + 1 >= cap)
        {
            err_ = "too_big";
            close();
            return -1;
        }
        const long n = raw_read(out + have, cap - 1 - have, static_cast<unsigned>((until - now) / 1000 + 1));
        if (n < 0)
            break; // closed: the answer ends here (Connection: close)
        have += static_cast<std::size_t>(n);
        bytes_in += static_cast<std::uint64_t>(n);
        out[have] = 0;
        if (body_at < 0)
        {
            const char *e = std::strstr(out, "\r\n\r\n");
            if (!e)
                continue;
            body_at = e + 4 - out;
            out[body_at - 2] = 0; // the head as its own string for the header lookups
            if (std::sscanf(out, "HTTP/1.%*d %d", &status) != 1)
                status = -1;
            want = header_num(out, "content-length");
            chunked = header_has(out, "transfer-encoding: chunked");
            if (retry_after_s)
            {
                const long ra = header_num(out, "retry-after");
                *retry_after_s = ra > 0 ? static_cast<int>(ra > 3600 ? 3600 : ra) : 0;
            }
            out[body_at - 2] = '\r';
        }
    }
    close();
    if (body_at < 0 || status < 0)
    {
        err_ = "answer";
        return -1;
    }
    std::size_t blen = have - static_cast<std::size_t>(body_at);
    std::memmove(out, out + body_at, blen);
    out[blen] = 0;
    if (chunked)
    {
        // de-chunk in place
        std::size_t r = 0, w = 0;
        for (;;)
        {
            char *end = nullptr;
            const unsigned long sz = std::strtoul(out + r, &end, 16);
            const char *nl = std::strstr(out + r, "\r\n");
            if (!nl || sz == 0)
                break;
            r = static_cast<std::size_t>(nl - out) + 2;
            if (r + sz > blen)
                break;
            std::memmove(out + w, out + r, sz);
            w += sz;
            r += sz + 2;
        }
        blen = w;
        out[blen] = 0;
    }
    return status;
}

bool http_base_of(const char *hub_ws_url, char *out, std::size_t cap) noexcept
{
    bool tls = false;
    char host[96], path[96];
    std::uint16_t port = 0;
    if (!split_ws_url(hub_ws_url, &tls, host, sizeof host, &port, path, sizeof path))
        return false;
    const bool def = (tls && port == 443) || (!tls && port == 80);
    const int n = def ? std::snprintf(out, cap, "%s://%s", tls ? "https" : "http", host)
                      : std::snprintf(out, cap, "%s://%s:%u", tls ? "https" : "http", host, static_cast<unsigned>(port));
    return n > 0 && static_cast<std::size_t>(n) < cap;
}
} // namespace play::net
