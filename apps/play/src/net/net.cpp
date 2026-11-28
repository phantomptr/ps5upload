// PS5Upload Play - sockets: a WebSocket client over TCP and the UDP discovery probe.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "net.hpp"

#include <arpa/inet.h>
#include <cerrno>
#include <cstdio>
#include <cstring>
#include <fcntl.h>
#include <ifaddrs.h>
#include <net/if.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#ifdef PLAY_PS5
extern "C" int sceNetInit();
extern "C" int sceKernelUsleep(unsigned us);
#endif

namespace play::net
{
namespace
{
// The console's sockets take non-blocking mode as a socket option (SO_NBIO); FIONBIO and
// fcntl are not what its network stack reads.
#ifndef SO_NBIO
#define PLAY_SO_NBIO 0x1200
#else
#define PLAY_SO_NBIO SO_NBIO
#endif
constexpr int kRcvBuf = 512 * 1024;

void set_nonblocking(int fd, bool on) noexcept
{
#ifdef PLAY_PS5
    int v = on ? 1 : 0;
    setsockopt(fd, SOL_SOCKET, PLAY_SO_NBIO, &v, sizeof v);
#else
    const int fl = fcntl(fd, F_GETFL, 0);
    fcntl(fd, F_SETFL, on ? (fl | O_NONBLOCK) : (fl & ~O_NONBLOCK));
#endif
}

void no_sigpipe(int fd) noexcept
{
#ifdef SO_NOSIGPIPE
    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof one);
#else
    (void)fd;
#endif
}

#ifdef MSG_NOSIGNAL
constexpr int kSendFlags = MSG_NOSIGNAL;
#else
constexpr int kSendFlags = 0;
#endif

std::uint64_t rng_state = 0;

std::uint64_t rng() noexcept
{
    if (!rng_state)
        rng_state = now_us() * 0x9E3779B97F4A7C15ull ^ reinterpret_cast<std::uintptr_t>(&rng_state);
    rng_state ^= rng_state << 13;
    rng_state ^= rng_state >> 7;
    rng_state ^= rng_state << 17;
    return rng_state;
}

// Connects with a time limit; returns the socket or -1.
int dial(std::uint32_t ip_be, std::uint16_t port, unsigned timeout_ms, const char **err) noexcept
{
    const int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0)
    {
        *err = "socket";
        return -1;
    }
    no_sigpipe(fd);
    int one = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
    int rcv = kRcvBuf;
    setsockopt(fd, SOL_SOCKET, SO_RCVBUF, &rcv, sizeof rcv);
    sockaddr_in a{};
    a.sin_family = AF_INET;
    a.sin_port = htons(port);
    a.sin_addr.s_addr = ip_be;
    set_nonblocking(fd, true);
    int rc = connect(fd, reinterpret_cast<sockaddr *>(&a), sizeof a);
    if (rc != 0 && errno != EINPROGRESS && errno != EWOULDBLOCK && errno != EALREADY)
    {
        *err = errno == ECONNREFUSED ? "refused" : "connect";
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
            *err = rc == 0 ? "timeout" : soerr == ECONNREFUSED ? "refused" : "connect";
            ::close(fd);
            return -1;
        }
    }
    set_nonblocking(fd, false);
    return fd;
}

bool send_all(int fd, const void *data, std::size_t len, unsigned timeout_ms) noexcept
{
    const auto *p = static_cast<const std::uint8_t *>(data);
    const std::uint64_t until = now_us() + timeout_ms * 1000ull;
    while (len)
    {
        pollfd pf{fd, POLLOUT, 0};
        const std::uint64_t now = now_us();
        if (now >= until || poll(&pf, 1, static_cast<int>((until - now) / 1000 + 1)) <= 0)
            return false;
        const long n = ::send(fd, p, len, kSendFlags);
        if (n < 0)
        {
            if (errno == EINTR || errno == EAGAIN)
                continue;
            return false;
        }
        p += n;
        len -= static_cast<std::size_t>(n);
    }
    return true;
}
} // namespace

void init() noexcept
{
#ifdef PLAY_PS5
    sceNetInit();
#endif
}

std::uint64_t now_us() noexcept
{
    timespec ts{};
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return static_cast<std::uint64_t>(ts.tv_sec) * 1000000u + static_cast<std::uint64_t>(ts.tv_nsec) / 1000;
}

void sleep_ms(unsigned ms) noexcept
{
#ifdef PLAY_PS5
    sceKernelUsleep(ms * 1000);
#else
    usleep(ms * 1000);
#endif
}

void random_bytes(std::uint8_t *out, std::size_t n) noexcept
{
    for (std::size_t i = 0; i < n; ++i)
        out[i] = static_cast<std::uint8_t>(rng() >> 24);
}

bool resolve(const char *addr, std::uint32_t *ip_be) noexcept
{
    // Dotted quad by hand (inet_pton is not among the calls this app relies on).
    unsigned part[4] = {};
    int n = 0, digits = 0;
    const char *p = addr;
    for (; *p; ++p)
    {
        if (*p >= '0' && *p <= '9' && digits < 3)
            part[n] = part[n] * 10 + static_cast<unsigned>(*p - '0'), ++digits;
        else if (*p == '.' && digits && n < 3)
            ++n, digits = 0;
        else
            break;
    }
    if (!*p && n == 3 && digits && part[0] < 256 && part[1] < 256 && part[2] < 256 && part[3] < 256)
    {
        const std::uint8_t b[4] = {static_cast<std::uint8_t>(part[0]), static_cast<std::uint8_t>(part[1]),
                                   static_cast<std::uint8_t>(part[2]), static_cast<std::uint8_t>(part[3])};
        std::memcpy(ip_be, b, 4);
        return true;
    }
#ifndef PLAY_PS5
    addrinfo hints{}, *res = nullptr;
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    if (getaddrinfo(addr, nullptr, &hints, &res) == 0 && res)
    {
        *ip_be = reinterpret_cast<sockaddr_in *>(res->ai_addr)->sin_addr.s_addr;
        freeaddrinfo(res);
        return true;
    }
#endif
    return false;
}

void ip_text(std::uint32_t ip_be, char out[16]) noexcept
{
    const auto *b = reinterpret_cast<const std::uint8_t *>(&ip_be);
    std::snprintf(out, 16, "%u.%u.%u.%u", b[0], b[1], b[2], b[3]);
}

// ---- WebSocket client -----------------------------------------------------------------------
WsClient::WsClient(std::uint8_t *buf, std::size_t cap) noexcept : reader_{buf, cap}
{
    pthread_mutex_init(&send_lock_, nullptr);
}

WsClient::~WsClient()
{
    close();
    pthread_mutex_destroy(&send_lock_);
}

bool WsClient::open(const char *addr, std::uint16_t port, const char *path, unsigned timeout_ms,
                    int *status) noexcept
{
    close();
    if (status)
        *status = 0;
    std::uint32_t ip = 0;
    if (!resolve(addr, &ip))
    {
        err_ = "address";
        return false;
    }
    const int fd = dial(ip, port, timeout_ms, &err_);
    if (fd < 0)
        return false;
    char host[96], key[32];
    char ipt[16];
    ip_text(ip, ipt);
    std::snprintf(host, sizeof host, "%s:%u", ipt, static_cast<unsigned>(port));
    std::uint8_t k16[16];
    random_bytes(k16, sizeof k16);
    char req[512];
    const std::size_t rl = ws::handshake_request(req, sizeof req, host, path, k16, key);
    if (!rl || !send_all(fd, req, rl, timeout_ms))
    {
        err_ = "send";
        ::close(fd);
        return false;
    }
    // Read the answer's header block; anything after it is the first frames.
    char resp[2048];
    std::size_t have = 0, end = 0;
    const std::uint64_t until = now_us() + timeout_ms * 1000ull;
    while (!end)
    {
        pollfd p{fd, POLLIN, 0};
        const std::uint64_t now = now_us();
        if (now >= until || poll(&p, 1, static_cast<int>((until - now) / 1000 + 1)) <= 0)
        {
            err_ = "timeout";
            ::close(fd);
            return false;
        }
        const long n = recv(fd, resp + have, sizeof resp - 1 - have, 0);
        if (n <= 0)
        {
            err_ = "closed";
            ::close(fd);
            return false;
        }
        have += static_cast<std::size_t>(n);
        end = ws::header_end(resp, have);
        if (!end && have >= sizeof resp - 1)
        {
            err_ = "header";
            ::close(fd);
            return false;
        }
    }
    int st = 0;
    if (!ws::handshake_ok(resp, end, key, &st))
    {
        if (status)
            *status = st;
        err_ = st == 429 ? "busy" : "upgrade";
        ::close(fd);
        return false;
    }
    reader_.reset();
    in_len_ = have - end;
    in_at_ = 0;
    std::memcpy(in_, resp + end, in_len_);
    fd_ = fd;
    err_ = "";
    return true;
}

void WsClient::close() noexcept
{
    if (fd_ >= 0)
    {
        pthread_mutex_lock(&send_lock_);
        const int fd = fd_;
        fd_ = -1;
        pthread_mutex_unlock(&send_lock_);
        shutdown(fd, SHUT_RDWR);
        ::close(fd);
    }
}

bool WsClient::send(ws::Op op, const void *data, std::size_t len) noexcept
{
    pthread_mutex_lock(&send_lock_);
    bool ok = false;
    if (fd_ >= 0)
    {
        std::uint8_t mask[4];
        random_bytes(mask, 4);
        const std::size_t n = ws::frame(out_, sizeof out_, op, data, len, mask);
        ok = n && send_all(fd_, out_, n, 2000);
    }
    pthread_mutex_unlock(&send_lock_);
    return ok;
}

bool WsClient::send_text(const char *s) noexcept
{
    return send(ws::kText, s, std::strlen(s));
}

WsClient::Got WsClient::next(unsigned timeout_ms) noexcept
{
    const std::uint64_t until = now_us() + timeout_ms * 1000ull;
    for (;;)
    {
        while (in_at_ < in_len_)
        {
            std::size_t used = 0;
            const auto got = reader_.feed(in_ + in_at_, in_len_ - in_at_, &used);
            in_at_ += used;
            switch (got)
            {
            case ws::Reader::Got::need_more:
                break;
            case ws::Reader::Got::message:
                return Got::message;
            case ws::Reader::Got::control:
                if (reader_.op() == ws::kPing)
                    send(ws::kPong, reader_.data(), reader_.size());
                else if (reader_.op() == ws::kClose)
                {
                    send(ws::kClose, reader_.data(), reader_.size() >= 2 ? 2 : 0);
                    err_ = "closed";
                    close();
                    return Got::closed;
                }
                continue;
            case ws::Reader::Got::too_big:
                err_ = "too_big";
                continue; // skipped; the stream goes on
            case ws::Reader::Got::bad:
                err_ = "protocol";
                close();
                return Got::closed;
            }
        }
        const int fd = fd_;
        if (fd < 0)
            return Got::closed;
        const std::uint64_t now = now_us();
        if (now >= until)
            return Got::timeout;
        pollfd p{fd, POLLIN, 0};
        const int pr = poll(&p, 1, static_cast<int>((until - now) / 1000 + 1));
        if (pr == 0)
            return Got::timeout;
        if (pr < 0)
        {
            if (errno == EINTR)
                continue;
            err_ = "poll";
            close();
            return Got::closed;
        }
        const long n = recv(fd, in_, sizeof in_, 0);
        if (n <= 0)
        {
            if (n < 0 && (errno == EINTR || errno == EAGAIN))
                continue;
            err_ = n == 0 ? "closed" : "recv";
            close();
            return Got::closed;
        }
        bytes_in_ += static_cast<std::uint64_t>(n);
        in_at_ = 0;
        in_len_ = static_cast<std::size_t>(n);
    }
}

// ---- discovery ------------------------------------------------------------------------------
int discover(const std::uint32_t *unicast, int n_unicast, unsigned timeout_ms, Answer *out, int cap) noexcept
{
    const int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0)
        return 0;
    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_BROADCAST, &one, sizeof one);
    std::uint8_t probe[proto::kProbeSize];
    proto::probe(probe);
    auto send_to = [&](std::uint32_t ip_be) {
        sockaddr_in a{};
        a.sin_family = AF_INET;
        a.sin_port = htons(proto::kDiscoveryPort);
        a.sin_addr.s_addr = ip_be;
        sendto(fd, probe, sizeof probe, 0, reinterpret_cast<sockaddr *>(&a), sizeof a);
    };
    // The limited broadcast, each interface's own broadcast address, and the ones we know.
    send_to(htonl(INADDR_BROADCAST));
    ifaddrs *ifs = nullptr;
    if (getifaddrs(&ifs) == 0)
    {
        for (ifaddrs *i = ifs; i; i = i->ifa_next)
        {
            if (!i->ifa_addr || i->ifa_addr->sa_family != AF_INET || !i->ifa_netmask || (i->ifa_flags & IFF_LOOPBACK))
                continue;
            const std::uint32_t ip = reinterpret_cast<sockaddr_in *>(i->ifa_addr)->sin_addr.s_addr;
            const std::uint32_t mask = reinterpret_cast<sockaddr_in *>(i->ifa_netmask)->sin_addr.s_addr;
            send_to(ip | ~mask);
        }
        freeifaddrs(ifs);
    }
    for (int k = 0; k < n_unicast; ++k)
        send_to(unicast[k]);
    int found = 0;
    const std::uint64_t until = now_us() + timeout_ms * 1000ull;
    for (;;)
    {
        const std::uint64_t now = now_us();
        if (now >= until)
            break;
        pollfd p{fd, POLLIN, 0};
        if (poll(&p, 1, static_cast<int>((until - now) / 1000 + 1)) <= 0)
            break;
        char buf[1600];
        sockaddr_in from{};
        socklen_t fl = sizeof from;
        const long n = recvfrom(fd, buf, sizeof buf - 1, 0, reinterpret_cast<sockaddr *>(&from), &fl);
        if (n <= 0)
            continue;
        Answer a{};
        a.ip_be = from.sin_addr.s_addr;
        if (!proto::parse_found(buf, static_cast<std::size_t>(n), a.info))
            continue;
        bool dup = false;
        for (int k = 0; k < found; ++k)
            dup = dup || (out[k].ip_be == a.ip_be && out[k].info.port == a.info.port);
        if (!dup && found < cap)
            out[found++] = a;
    }
    ::close(fd);
    return found;
}

int http_get(const char *addr, std::uint16_t port, const char *path, char *out, std::size_t cap,
             unsigned timeout_ms) noexcept
{
    if (cap)
        out[0] = 0;
    std::uint32_t ip = 0;
    const char *err = "";
    if (!resolve(addr, &ip))
        return -1;
    const int fd = dial(ip, port, timeout_ms, &err);
    if (fd < 0)
        return -1;
    char ipt[16], req[256];
    ip_text(ip, ipt);
    const int rl = std::snprintf(req, sizeof req, "GET %s HTTP/1.1\r\nHost: %s:%u\r\nConnection: close\r\n\r\n",
                                 path, ipt, static_cast<unsigned>(port));
    if (!send_all(fd, req, static_cast<std::size_t>(rl), timeout_ms))
    {
        ::close(fd);
        return -1;
    }
    char buf[8192];
    std::size_t have = 0;
    const std::uint64_t until = now_us() + timeout_ms * 1000ull;
    while (have < sizeof buf - 1)
    {
        pollfd p{fd, POLLIN, 0};
        const std::uint64_t now = now_us();
        if (now >= until || poll(&p, 1, static_cast<int>((until - now) / 1000 + 1)) <= 0)
            break;
        const long n = recv(fd, buf + have, sizeof buf - 1 - have, 0);
        if (n <= 0)
            break;
        have += static_cast<std::size_t>(n);
    }
    ::close(fd);
    buf[have] = 0;
    int status = -1;
    if (have > 12 && std::strncmp(buf, "HTTP/1.", 7) == 0 && buf[8] == ' ')
        status = (buf[9] - '0') * 100 + (buf[10] - '0') * 10 + (buf[11] - '0');
    const std::size_t end = ws::header_end(buf, have);
    if (end && cap)
    {
        const std::size_t bl = have - end < cap - 1 ? have - end : cap - 1;
        std::memcpy(out, buf + end, bl);
        out[bl] = 0;
    }
    return status;
}
} // namespace play::net
