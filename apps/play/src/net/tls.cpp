// PS5Upload Play - TLS 1.2 client (BearSSL's engine API, as the helper's tailnet uses it).
// SPDX-License-Identifier: GPL-3.0-or-later
#include "tls.hpp"

#include "net.hpp"

#include <bearssl.h>
#include <cerrno>
#include <cstring>
#include <netdb.h>
#include <netinet/in.h>
#include <poll.h>
#include <sys/socket.h>
#include <time.h>

#ifdef PLAY_PS5
extern "C" int sceRandomGetRandomNumber(void *buf, std::size_t size);
extern "C" int sceNetResolverCreate(const char *name, int memid, int flags);
extern "C" int sceNetResolverStartNtoa(int rid, const char *hostname, std::uint32_t *addr, int timeout, int retry,
                                       int flags);
extern "C" int sceNetResolverDestroy(int rid);
#else
#include <fcntl.h>
#include <unistd.h>
#endif

extern "C" const br_x509_trust_anchor *play_tls_roots(std::size_t *n);

namespace play::net
{
#ifdef MSG_NOSIGNAL
constexpr int kSendFlags = MSG_NOSIGNAL;
#else
constexpr int kSendFlags = 0;
#endif

struct Tls::State
{
    br_ssl_client_context sc;
    br_x509_minimal_context xc;
    unsigned char iobuf[BR_SSL_BUFSIZE_BIDI];
};

namespace
{
// A trimmed client profile (tailnet/src/derp.c): TLS 1.2, ECDHE with AES-GCM or ChaCha20, ECDSA
// and RSA signatures, SHA-256/384.
void setup(br_ssl_client_context *cc, br_x509_minimal_context *xc) noexcept
{
    static const std::uint16_t suites[] = {
        BR_TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,       BR_TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
        BR_TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256, BR_TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
        BR_TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,       BR_TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
    };
    std::size_t n_tas = 0;
    const br_x509_trust_anchor *tas = play_tls_roots(&n_tas);
    br_ssl_engine_context *e = &cc->eng;
    br_ssl_client_zero(cc);
    br_ssl_engine_set_versions(e, BR_TLS12, BR_TLS12);
    br_ssl_engine_set_suites(e, suites, sizeof suites / sizeof suites[0]);
    br_ssl_engine_set_hash(e, br_sha256_ID, &br_sha256_vtable);
    br_ssl_engine_set_hash(e, br_sha384_ID, &br_sha384_vtable);
    br_ssl_engine_set_prf_sha256(e, br_tls12_sha256_prf);
    br_ssl_engine_set_prf_sha384(e, br_tls12_sha384_prf);
    br_ssl_engine_set_ec(e, &br_ec_prime_i31);
    br_ssl_engine_set_rsavrfy(e, br_rsa_i31_pkcs1_vrfy);
    br_ssl_engine_set_ecdsa(e, br_ecdsa_i31_vrfy_asn1);
    br_ssl_engine_set_gcm(e, &br_sslrec_in_gcm_vtable, &br_sslrec_out_gcm_vtable);
    br_ssl_engine_set_aes_ctr(e, &br_aes_ct64_ctr_vtable);
    br_ssl_engine_set_ghash(e, br_ghash_ctmul64);
    br_ssl_engine_set_chapol(e, &br_sslrec_in_chapol_vtable, &br_sslrec_out_chapol_vtable);
    br_ssl_engine_set_chacha20(e, br_chacha20_ct_run);
    br_ssl_engine_set_poly1305(e, br_poly1305_ctmul_run);
    br_x509_minimal_init(xc, &br_sha256_vtable, tas, n_tas);
    br_x509_minimal_set_hash(xc, br_sha256_ID, &br_sha256_vtable);
    br_x509_minimal_set_hash(xc, br_sha384_ID, &br_sha384_vtable);
    br_x509_minimal_set_rsa(xc, br_rsa_i31_pkcs1_vrfy);
    br_x509_minimal_set_ecdsa(xc, &br_ec_prime_i31, br_ecdsa_i31_vrfy_asn1);
    // The certificate's dates against the console's clock (days since 1 January of year 0).
    timespec ts{};
    clock_gettime(CLOCK_REALTIME, &ts);
    const std::uint64_t secs = static_cast<std::uint64_t>(ts.tv_sec);
    br_x509_minimal_set_time(xc, static_cast<std::uint32_t>(719528 + secs / 86400),
                             static_cast<std::uint32_t>(secs % 86400));
    br_ssl_engine_set_x509(e, &xc->vtable);
}

int ms_left(std::uint64_t until) noexcept
{
    const std::uint64_t now = now_us();
    return now >= until ? 0 : static_cast<int>((until - now) / 1000 + 1);
}
} // namespace

Tls::Tls() noexcept : st_{new State} {}

Tls::~Tls()
{
    delete st_;
}

void Tls::reset() noexcept
{
    fd_ = -1;
    err_ = "";
}

int Tls::code() const noexcept
{
    return br_ssl_engine_last_error(&st_->sc.eng);
}

bool Tls::start(int fd, const char *server_name, unsigned timeout_ms) noexcept
{
    fd_ = fd;
    err_ = "";
    setup(&st_->sc, &st_->xc);
    br_ssl_engine_set_buffer(&st_->sc.eng, st_->iobuf, sizeof st_->iobuf, 1);
    std::uint8_t seed[32];
    if (!secure_random(seed, sizeof seed))
    {
        err_ = "random";
        return false;
    }
    br_ssl_engine_inject_entropy(&st_->sc.eng, seed, sizeof seed);
    if (!br_ssl_client_reset(&st_->sc, server_name, 0))
    {
        err_ = "tls";
        return false;
    }
    // The handshake: done once the engine takes application data.
    const int r = pump(false, now_us() + timeout_ms * 1000ull);
    if (r <= 0)
    {
        if (!*err_ || r == 0)
            err_ = r == 0 ? "timeout" : "tls";
        return false;
    }
    return true;
}

int Tls::pump(bool want_read, std::uint64_t until) noexcept
{
    br_ssl_engine_context *e = &st_->sc.eng;
    for (;;)
    {
        const unsigned s = br_ssl_engine_current_state(e);
        if (s & BR_SSL_CLOSED)
        {
            err_ = br_ssl_engine_last_error(e) ? (br_ssl_engine_last_error(e) >= BR_ERR_X509_OK ? "certificate" : "tls")
                                               : "closed";
            return -1;
        }
        if (s & BR_SSL_SENDREC)
        {
            std::size_t len = 0;
            unsigned char *b = br_ssl_engine_sendrec_buf(e, &len);
            pollfd p{fd_, POLLOUT, 0};
            if (poll(&p, 1, ms_left(until)) <= 0)
                return 0;
            const long n = ::send(fd_, b, len, kSendFlags);
            if (n <= 0)
            {
                if (n < 0 && (errno == EINTR || errno == EAGAIN))
                    continue;
                err_ = "send";
                return -1;
            }
            br_ssl_engine_sendrec_ack(e, static_cast<std::size_t>(n));
            continue;
        }
        if (want_read ? (s & BR_SSL_RECVAPP) : (s & BR_SSL_SENDAPP))
            return 1;
        if (s & BR_SSL_RECVREC)
        {
            pollfd p{fd_, POLLIN, 0};
            const int left = ms_left(until);
            if (!left || poll(&p, 1, left) <= 0)
                return 0;
            std::size_t len = 0;
            unsigned char *b = br_ssl_engine_recvrec_buf(e, &len);
            const long n = ::recv(fd_, b, len, 0);
            if (n <= 0)
            {
                if (n < 0 && (errno == EINTR || errno == EAGAIN))
                    continue;
                err_ = "closed";
                return -1;
            }
            br_ssl_engine_recvrec_ack(e, static_cast<std::size_t>(n));
            continue;
        }
        // Waiting to send app data with nothing to read, or the other way: nothing moves.
        return 0;
    }
}

bool Tls::write(const void *data, std::size_t len, unsigned timeout_ms) noexcept
{
    br_ssl_engine_context *e = &st_->sc.eng;
    const auto *p = static_cast<const std::uint8_t *>(data);
    const std::uint64_t until = now_us() + timeout_ms * 1000ull;
    while (len)
    {
        if (pump(false, until) <= 0)
            return false;
        std::size_t room = 0;
        unsigned char *b = br_ssl_engine_sendapp_buf(e, &room);
        const std::size_t n = room < len ? room : len;
        std::memcpy(b, p, n);
        br_ssl_engine_sendapp_ack(e, n);
        p += n;
        len -= n;
    }
    br_ssl_engine_flush(e, 0);
    // Out with the records now.
    while (br_ssl_engine_current_state(e) & BR_SSL_SENDREC)
        if (pump(false, until) <= 0)
            return false;
    return true;
}

long Tls::read(void *out, std::size_t cap, unsigned timeout_ms) noexcept
{
    br_ssl_engine_context *e = &st_->sc.eng;
    const int r = pump(true, now_us() + timeout_ms * 1000ull);
    if (r < 0)
        return -1;
    if (r == 0)
        return 0;
    std::size_t len = 0;
    unsigned char *b = br_ssl_engine_recvapp_buf(e, &len);
    const std::size_t n = len < cap ? len : cap;
    std::memcpy(out, b, n);
    br_ssl_engine_recvapp_ack(e, n);
    return static_cast<long>(n);
}

bool secure_random(std::uint8_t *out, std::size_t n) noexcept
{
#ifdef PLAY_PS5
    while (n)
    {
        const std::size_t k = n < 64 ? n : 64; // the generator's most per call
        if (sceRandomGetRandomNumber(out, k) != 0)
            return false;
        out += k;
        n -= k;
    }
    return true;
#else
    const int fd = ::open("/dev/urandom", O_RDONLY);
    if (fd < 0)
        return false;
    std::size_t got = 0;
    while (got < n)
    {
        const long r = ::read(fd, out + got, n - got);
        if (r <= 0)
            break;
        got += static_cast<std::size_t>(r);
    }
    ::close(fd);
    return got == n;
#endif
}

bool resolve_name(const char *host, std::uint32_t *ip_be) noexcept
{
    if (resolve(host, ip_be)) // a dotted quad (and, on a computer, any name)
        return true;
#ifdef PLAY_PS5
    const int rid = sceNetResolverCreate("play-hub", 0, 0);
    if (rid < 0)
        return false;
    std::uint32_t a = 0;
    const int r = sceNetResolverStartNtoa(rid, host, &a, 0, 0, 0);
    sceNetResolverDestroy(rid);
    if (r < 0 || !a)
        return false;
    *ip_be = a;
    return true;
#else
    return false;
#endif
}
} // namespace play::net
