// PS5Upload Play - a knock's secret sealed to the room's key (seal.hpp).
// SPDX-License-Identifier: GPL-3.0-or-later
#include "seal.hpp"

#include "../core/invite.hpp"
#include "../core/json.hpp"
#include "tls.hpp"

#include <bearssl.h>

#include <cstring>

namespace play::seal
{
namespace
{
constexpr char kInfo[] = "ps5upload room knock v1";
constexpr std::size_t kPoint = 65, kNonce = 12, kTag = 16;

int b64v(char c) noexcept
{
    if (c >= 'A' && c <= 'Z')
        return c - 'A';
    if (c >= 'a' && c <= 'z')
        return c - 'a' + 26;
    if (c >= '0' && c <= '9')
        return c - '0' + 52;
    if (c == '-' || c == '+')
        return 62;
    if (c == '_' || c == '/')
        return 63;
    return -1;
}

void derive(const std::uint8_t shared_x[32], const std::uint8_t *salt, std::size_t salt_len, std::uint8_t key[16]) noexcept
{
    br_hkdf_context h;
    br_hkdf_init(&h, &br_sha256_vtable, salt, salt_len);
    br_hkdf_inject(&h, shared_x, 32);
    br_hkdf_flip(&h);
    br_hkdf_produce(&h, kInfo, sizeof kInfo - 1, key, 16);
}

// AES-128-GCM in place; encrypt or decrypt (then checks the tag).
bool gcm(const std::uint8_t key[16], const std::uint8_t nonce[12], std::uint8_t *data, std::size_t len,
         std::uint8_t tag[16], bool encrypt) noexcept
{
    br_aes_ct_ctr_keys ctr;
    br_aes_ct_ctr_init(&ctr, key, 16);
    br_gcm_context g;
    br_gcm_init(&g, &ctr.vtable, br_ghash_ctmul32);
    br_gcm_reset(&g, nonce, kNonce);
    br_gcm_flip(&g);
    br_gcm_run(&g, encrypt ? 1 : 0, data, len);
    if (encrypt)
    {
        br_gcm_get_tag(&g, tag);
        return true;
    }
    return br_gcm_check_tag(&g, tag) == 1;
}

// x = scalar * point; the x coordinate into out.
bool ecdh(const std::uint8_t point[kPoint], const std::uint8_t *scalar, std::size_t slen, std::uint8_t out[32]) noexcept
{
    std::uint8_t g[kPoint];
    std::memcpy(g, point, kPoint);
    if (g[0] != 0x04 || br_ec_p256_m31.mul(g, kPoint, scalar, slen, BR_EC_secp256r1) != 1)
        return false;
    std::memcpy(out, g + 1, 32);
    return true;
}

std::size_t plaintext(const char *challenge, const char *gid, const char *secret, char *out, std::size_t cap) noexcept
{
    json::Writer w(out, cap);
    w.raw("{\"secret\":").str(secret ? secret : "").raw(",\"gid\":").str(gid ? gid : "").raw(",\"challenge\":");
    w.str(challenge ? challenge : "").raw("}");
    return w.ok() ? w.len() : 0;
}
} // namespace

long b64url_decode(const char *in, std::uint8_t *out, std::size_t cap) noexcept
{
    std::size_t o = 0;
    std::uint32_t acc = 0;
    int bits = 0;
    for (const char *p = in; p && *p && *p != '='; ++p)
    {
        const int v = b64v(*p);
        if (v < 0)
            return -1;
        acc = acc << 6 | static_cast<std::uint32_t>(v);
        bits += 6;
        if (bits >= 8)
        {
            bits -= 8;
            if (o >= cap)
                return -1;
            out[o++] = static_cast<std::uint8_t>(acc >> bits);
        }
    }
    return static_cast<long>(o);
}

std::size_t seal_with(const char *room_pk, const char *challenge, const char *gid, const char *secret,
                      const std::uint8_t eph_sk[32], const std::uint8_t nonce[12], char *out, std::size_t cap) noexcept
{
    std::uint8_t pk[80], salt[64];
    if (b64url_decode(room_pk, pk, sizeof pk) != static_cast<long>(kPoint))
        return 0;
    const long sl = b64url_decode(challenge, salt, sizeof salt);
    if (sl < 1)
        return 0;
    // the ephemeral public key
    br_ec_private_key sk{BR_EC_secp256r1, const_cast<std::uint8_t *>(eph_sk), 32};
    std::uint8_t epk[BR_EC_KBUF_PUB_MAX_SIZE];
    if (br_ec_compute_pub(&br_ec_p256_m31, nullptr, epk, &sk) != kPoint)
        return 0;
    std::uint8_t shared[32], key[16];
    if (!ecdh(pk, eph_sk, 32, shared))
        return 0;
    derive(shared, salt, static_cast<std::size_t>(sl), key);
    char plain[512];
    const std::size_t pl = plaintext(challenge, gid, secret, plain, sizeof plain);
    if (!pl)
        return 0;
    std::uint8_t box[1 + kPoint + kNonce + sizeof plain + kTag];
    std::size_t n = 0;
    box[n++] = 0x01;
    std::memcpy(box + n, epk, kPoint);
    n += kPoint;
    std::memcpy(box + n, nonce, kNonce);
    n += kNonce;
    std::memcpy(box + n, plain, pl);
    gcm(key, nonce, box + n, pl, box + n + pl, true);
    n += pl + kTag;
    const std::size_t w = invite::b64url(box, n, out, cap);
    std::memset(shared, 0, sizeof shared);
    std::memset(key, 0, sizeof key);
    std::memset(plain, 0, sizeof plain);
    return w;
}

std::size_t seal(const char *room_pk, const char *challenge, const char *gid, const char *secret, char *out,
                 std::size_t cap) noexcept
{
    std::uint8_t seed[48], nonce[kNonce];
    if (!net::secure_random(seed, sizeof seed) || !net::secure_random(nonce, sizeof nonce))
        return 0;
    br_hmac_drbg_context rng;
    br_hmac_drbg_init(&rng, &br_sha256_vtable, seed, sizeof seed);
    std::uint8_t kbuf[BR_EC_KBUF_PRIV_MAX_SIZE];
    br_ec_private_key sk;
    if (br_ec_keygen(&rng.vtable, &br_ec_p256_m31, &sk, kbuf, BR_EC_secp256r1) != 32)
        return 0;
    const std::size_t n = seal_with(room_pk, challenge, gid, secret, kbuf, nonce, out, cap);
    std::memset(kbuf, 0, sizeof kbuf);
    std::memset(seed, 0, sizeof seed);
    return n;
}

bool open(const char *blob, const char *challenge, const std::uint8_t room_sk[32], char *plain, std::size_t cap) noexcept
{
    std::uint8_t box[1024], salt[64];
    const long n = b64url_decode(blob, box, sizeof box);
    const long sl = b64url_decode(challenge, salt, sizeof salt);
    if (n < static_cast<long>(1 + kPoint + kNonce + kTag) || sl < 1 || box[0] != 0x01)
        return false;
    std::uint8_t shared[32], key[16];
    if (!ecdh(box + 1, room_sk, 32, shared))
        return false;
    derive(shared, salt, static_cast<std::size_t>(sl), key);
    const std::size_t ct = static_cast<std::size_t>(n) - 1 - kPoint - kNonce - kTag;
    std::uint8_t *data = box + 1 + kPoint + kNonce;
    if (ct + 1 > cap || !gcm(key, box + 1 + kPoint, data, ct, data + ct, false))
        return false;
    std::memcpy(plain, data, ct);
    plain[ct] = 0;
    return true;
}
} // namespace play::seal
