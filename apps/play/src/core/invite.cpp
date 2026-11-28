// PS5Upload Play - easy join: invite links, the hand-off message and the waiting room's ids.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "invite.hpp"

#include "json.hpp"

#include <cstdio>
#include <cstdlib>
#include <cstring>

namespace play::invite
{
namespace
{
bool key_ok(const char *k, std::size_t n) noexcept
{
    if (n < 8 || n > 128)
        return false;
    for (std::size_t i = 0; i < n; ++i)
    {
        const char c = k[i];
        if (!((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '-' || c == '_'))
            return false;
    }
    return true;
}

int hexv(char c) noexcept
{
    return c >= '0' && c <= '9' ? c - '0' : c >= 'a' && c <= 'f' ? c - 'a' + 10 : c >= 'A' && c <= 'F' ? c - 'A' + 10 : -1;
}

// Percent-decodes [s, s+n) into out; false when broken or too long.
bool pct_decode(const char *s, std::size_t n, char *out, std::size_t cap) noexcept
{
    std::size_t o = 0;
    for (std::size_t i = 0; i < n; ++i)
    {
        char c = s[i];
        if (c == '%')
        {
            if (i + 2 >= n)
                return false;
            const int a = hexv(s[i + 1]), b = hexv(s[i + 2]);
            if (a < 0 || b < 0)
                return false;
            c = static_cast<char>(a * 16 + b);
            i += 2;
        }
        else if (c == '+')
            c = ' ';
        if (o + 1 >= cap || c == 0)
            return false;
        out[o++] = c;
    }
    out[o] = 0;
    return true;
}

// A fragment's value for `key` ("i", "k", "lan"…), decoded; false when absent.
bool frag_get(const char *frag, const char *key, char *out, std::size_t cap) noexcept
{
    const std::size_t kl = std::strlen(key);
    const char *p = frag;
    while (*p)
    {
        const char *amp = std::strchr(p, '&');
        const std::size_t n = amp ? static_cast<std::size_t>(amp - p) : std::strlen(p);
        if (n > kl && !std::strncmp(p, key, kl) && p[kl] == '=')
            return pct_decode(p + kl + 1, n - kl - 1, out, cap);
        if (!amp)
            break;
        p = amp + 1;
    }
    return false;
}

struct Url
{
    bool https = false;
    char host[64] = {};
    std::uint16_t port = 0;
    char path[80] = {}; // before '#'
    const char *frag = ""; // after '#', in the original text
};

bool parse_url(const char *s, Url &u) noexcept
{
    if (!std::strncmp(s, "http://", 7))
        s += 7;
    else if (!std::strncmp(s, "https://", 8))
        s += 8, u.https = true;
    else
        return false;
    std::size_t h = 0;
    while (s[h] && s[h] != ':' && s[h] != '/' && s[h] != '#' && s[h] != '?')
        ++h;
    if (!h || h >= sizeof u.host)
        return false;
    std::memcpy(u.host, s, h);
    u.host[h] = 0;
    s += h;
    u.port = u.https ? 443 : 80;
    if (*s == ':')
    {
        char *end = nullptr;
        const long p = std::strtol(s + 1, &end, 10);
        if (p < 1 || p > 65535 || end == s + 1)
            return false;
        u.port = static_cast<std::uint16_t>(p);
        s = end;
    }
    std::size_t pl = 0;
    while (s[pl] && s[pl] != '#' && s[pl] != '?')
        ++pl;
    if (pl >= sizeof u.path)
        return false;
    std::memcpy(u.path, s, pl);
    u.path[pl] = 0;
    s += pl;
    if (*s == '?')
        while (*s && *s != '#')
            ++s;
    u.frag = *s == '#' ? s + 1 : "";
    return true;
}

// "/prefix/join" or "/prefix/join/" -> the guest socket's path under the same prefix.
bool join_path(const char *path, char *ws_path, std::size_t cap) noexcept
{
    std::size_t n = std::strlen(path);
    if (n && path[n - 1] == '/')
        --n;
    if (n < 5 || std::strncmp(path + n - 5, "/join", 5))
        return false;
    const int w = std::snprintf(ws_path, cap, "%.*s/api/join/ws", static_cast<int>(n - 5), path);
    return w > 0 && static_cast<std::size_t>(w) < cap;
}

// A page URL (`http://ip:port[/prefix]/join`) as a direct route, when it is one the app may use.
void add_route(const char *page, Invite &inv) noexcept
{
    Url u;
    if (!parse_url(page, u) || u.https || inv.n_routes >= 6)
        return;
    Route r;
    if (!private_v4(u.host, &r.tailnet) || !join_path(u.path, r.path, sizeof r.path))
        return;
    std::snprintf(r.addr, sizeof r.addr, "%s", u.host);
    r.port = u.port;
    for (int i = 0; i < inv.n_routes; ++i)
        if (inv.routes[i].port == r.port && !std::strcmp(inv.routes[i].addr, r.addr))
            return;
    if (!inv.home_addr[0])
    {
        std::snprintf(inv.home_addr, sizeof inv.home_addr, "%s", r.addr);
        inv.home_port = r.port;
    }
    inv.routes[inv.n_routes++] = r;
}

// A hub room id: 22 base64url characters.
bool room_ok(const char *r) noexcept
{
    return std::strlen(r) == 22 && key_ok(r, 22);
}

// A certificate fingerprint as the link carries it: hex pairs split by colons (SHA-256: 95).
bool fp_ok(const char *f) noexcept
{
    const std::size_t n = std::strlen(f);
    if (n < 2 || n >= sizeof(Invite{}.fp))
        return false;
    for (std::size_t i = 0; i < n; ++i)
        if (i % 3 == 2 ? f[i] != ':' : hexv(f[i]) < 0)
            return false;
    return n % 3 == 2;
}

bool set_key(Invite &inv, const char *k) noexcept
{
    const std::size_t n = std::strlen(k);
    if (!key_ok(k, n))
        return false;
    if (inv.key[0])
        return !std::strcmp(inv.key, k);
    std::snprintf(inv.key, sizeof inv.key, "%s", k);
    return true;
}

// ---- SHA-256 (FIPS 180-4), for the room id -------------------------------------------------
struct Sha256
{
    std::uint32_t h[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};
    std::uint8_t buf[64] = {};
    std::size_t have = 0;
    std::uint64_t total = 0;

    static std::uint32_t rotr(std::uint32_t x, int n) noexcept
    {
        return (x >> n) | (x << (32 - n));
    }
    void block(const std::uint8_t *p) noexcept
    {
        static const std::uint32_t k[64] = {
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
            0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
            0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
            0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
            0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
            0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
            0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2};
        std::uint32_t w[64];
        for (int i = 0; i < 16; ++i)
            w[i] = static_cast<std::uint32_t>(p[4 * i]) << 24 | static_cast<std::uint32_t>(p[4 * i + 1]) << 16 |
                   static_cast<std::uint32_t>(p[4 * i + 2]) << 8 | p[4 * i + 3];
        for (int i = 16; i < 64; ++i)
        {
            const std::uint32_t s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >> 3);
            const std::uint32_t s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16] + s0 + w[i - 7] + s1;
        }
        std::uint32_t a = h[0], b = h[1], c = h[2], d = h[3], e = h[4], f = h[5], g = h[6], hh = h[7];
        for (int i = 0; i < 64; ++i)
        {
            const std::uint32_t t1 = hh + (rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25)) + ((e & f) ^ (~e & g)) + k[i] + w[i];
            const std::uint32_t t2 = (rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22)) + ((a & b) ^ (a & c) ^ (b & c));
            hh = g, g = f, f = e, e = d + t1, d = c, c = b, b = a, a = t1 + t2;
        }
        h[0] += a, h[1] += b, h[2] += c, h[3] += d, h[4] += e, h[5] += f, h[6] += g, h[7] += hh;
    }
    void update(const void *data, std::size_t n) noexcept
    {
        const auto *p = static_cast<const std::uint8_t *>(data);
        total += n;
        while (n)
        {
            const std::size_t take = 64 - have < n ? 64 - have : n;
            std::memcpy(buf + have, p, take);
            have += take, p += take, n -= take;
            if (have == 64)
                block(buf), have = 0;
        }
    }
    void finish(std::uint8_t out[32]) noexcept
    {
        const std::uint64_t bits = total * 8;
        const std::uint8_t one = 0x80, zero = 0;
        update(&one, 1);
        while (have != 56)
            update(&zero, 1);
        std::uint8_t len[8];
        for (int i = 0; i < 8; ++i)
            len[i] = static_cast<std::uint8_t>(bits >> (56 - 8 * i));
        update(len, 8);
        for (int i = 0; i < 8; ++i)
            for (int k = 0; k < 4; ++k)
                out[4 * i + k] = static_cast<std::uint8_t>(h[i] >> (24 - 8 * k));
    }
};
} // namespace

bool private_v4(const char *addr, bool *tailnet) noexcept
{
    unsigned o[4] = {};
    int n = 0, digits = 0;
    const char *p = addr;
    for (; *p; ++p)
    {
        if (*p >= '0' && *p <= '9' && digits < 3)
            o[n] = o[n] * 10 + static_cast<unsigned>(*p - '0'), ++digits;
        else if (*p == '.' && digits && n < 3)
            ++n, digits = 0;
        else
            return false;
    }
    if (n != 3 || !digits || o[0] > 255 || o[1] > 255 || o[2] > 255 || o[3] > 255)
        return false;
    const bool ts = o[0] == 100 && (o[1] & 0xc0) == 64;
    if (tailnet)
        *tailnet = ts;
#ifdef PLAY_TEST_LOOPBACK
    // play-cli's end-to-end test: the stand-in engine is on this computer.
    if (o[0] == 127)
        return true;
#endif
    return ts || o[0] == 10 || (o[0] == 172 && (o[1] & 0xf0) == 16) || (o[0] == 192 && o[1] == 168);
}

bool add_link(const char *link, Invite &inv) noexcept
{
    while (*link == ' ')
        ++link;
    Url u;
    if (!parse_url(link, u))
        return false;
    char ws_path[80];
    if (!join_path(u.path, ws_path, sizeof ws_path))
        return false;
    char k[132], v[200];
    if (frag_get(u.frag, "room", v, sizeof v))
    {
        // The one link for everywhere: its key, and the host's own pages as direct routes.
        if (!frag_get(u.frag, "k", k, sizeof k) || !set_key(inv, k))
            return false;
        inv.internet = true;
        // The room and the certificate, for the bridge through this home's PS5Upload.
        char fp[sizeof inv.fp];
        if (!inv.room[0] && room_ok(v) && frag_get(u.frag, "fp", fp, sizeof fp) && fp_ok(fp))
        {
            std::snprintf(inv.room, sizeof inv.room, "%s", v);
            std::snprintf(inv.fp, sizeof inv.fp, "%s", fp);
        }
        if (frag_get(u.frag, "lan", v, sizeof v))
            add_route(v, inv);
        if (frag_get(u.frag, "ts", v, sizeof v))
            add_route(v, inv);
    }
    else
    {
        if (!frag_get(u.frag, "i", k, sizeof k) || !set_key(inv, k))
            return false;
        add_route(link, inv);
    }
    if (!inv.name[0] && frag_get(u.frag, "n", v, sizeof v))
        std::snprintf(inv.name, sizeof inv.name, "%s", v);
    return true;
}

bool parse_handoff(const char *text, std::size_t len, Invite &inv) noexcept
{
    inv = Invite{};
    if (len > 2048)
        return false;
    json::Token toks[48];
    json::Doc d(toks, 48);
    if (!d.parse(text, len) || d.type(0) != json::Type::object)
        return false;
    const int o = d.get(0, "invite");
    if (o < 0 || d.type(o) != json::Type::object)
        return false;
    char link[600];
    if (!d.str(d.get(o, "link"), link, sizeof link) || !add_link(link, inv))
        return false;
    const int arr = d.get(o, "links");
    if (arr >= 0 && d.type(arr) == json::Type::array)
        for (std::uint32_t i = 0; i < d.count(arr) && i < 4; ++i)
        {
            Invite more = inv; // the same key only
            if (d.str(d.item(arr, i), link, sizeof link) && add_link(link, more))
                inv = more;
        }
    char name[64];
    if (d.str(d.get(o, "name"), name, sizeof name) && name[0])
    {
        // printable only
        std::size_t w = 0;
        for (std::size_t i = 0; name[i] && w + 1 < sizeof inv.name; ++i)
            if (static_cast<unsigned char>(name[i]) >= 0x20)
                inv.name[w++] = name[i];
        inv.name[w] = 0;
    }
    char t[40];
    if (d.str(d.get(o, "t"), t, sizeof t) && key_ok(t, std::strlen(t)))
        std::snprintf(inv.nonce, sizeof inv.nonce, "%s", t);
    return true;
}

std::size_t b64url(const std::uint8_t *in, std::size_t n, char *out, std::size_t cap) noexcept
{
    static const char a[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    std::size_t o = 0;
    for (std::size_t i = 0; i < n; i += 3)
    {
        const std::uint32_t v = static_cast<std::uint32_t>(in[i]) << 16 | (i + 1 < n ? in[i + 1] << 8 : 0) |
                                (i + 2 < n ? in[i + 2] : 0);
        const int chars = i + 2 < n ? 4 : i + 1 < n ? 3 : 2;
        for (int k = 0; k < chars; ++k)
            if (o + 1 < cap)
                out[o++] = a[(v >> (18 - 6 * k)) & 63];
    }
    if (cap)
        out[o < cap ? o : cap - 1] = 0;
    return o;
}

void room_for_secret(const char *secret, char out[23]) noexcept
{
    static const char domain[] = "ps5upload-hub room";
    Sha256 s;
    s.update(domain, sizeof domain); // with its zero byte
    s.update(secret, std::strlen(secret));
    std::uint8_t h[32];
    s.finish(h);
    char b[48];
    b64url(h, 32, b, sizeof b);
    std::memcpy(out, b, 21);
    out[21] = 'h';
    out[22] = 0;
}

std::size_t qr_link(char *out, std::size_t cap, const char *page, const char *room, const char *nonce,
                    const char *hub) noexcept
{
    const int n = std::snprintf(out, cap, "%s#ps5=%s&t=%s&hub=%s", page, room, nonce, hub);
    return n > 0 && static_cast<std::size_t>(n) < cap ? static_cast<std::size_t>(n) : 0;
}

bool hub_host(const char *url, char *out, std::size_t cap) noexcept
{
    const char *s = !std::strncmp(url, "wss://", 6) ? url + 6 : !std::strncmp(url, "ws://", 5) ? url + 5 : nullptr;
    if (!s)
        return false;
    std::size_t n = 0;
    while (s[n] && s[n] != '/')
        ++n;
    if (!n || n >= cap)
        return false;
    std::memcpy(out, s, n);
    out[n] = 0;
    return true;
}
} // namespace play::invite
