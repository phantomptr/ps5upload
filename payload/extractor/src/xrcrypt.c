/* RAR encryption (see xrcrypt.h). */
#include "xrcrypt.h"

#include <string.h>

#include "../C/Aes.h"
#include "../C/Sha256.h"

/* RAR caps a password at 127 characters. */
#define XRC_PW_CHARS 127

void xrc_wipe(void *p, size_t n) {
    volatile uint8_t *v = (volatile uint8_t *)p;
    while (n--) *v++ = 0;
}

void xrc_init(void) {
    static int done;
    if (done) return;
    AesGenTables();
    Sha256Prepare();
    done = 1;
}

void xrc_aes_init(xrc_aes_t *c, const uint8_t *key, unsigned keylen, const uint8_t iv[16]) {
    Aes_SetKey_Dec(c->w + 4, key, keylen);
    AesCbc_Init(c->w, iv);
}

void xrc_aes_dec(xrc_aes_t *c, uint8_t *data, size_t nblocks) {
    if (nblocks) g_AesCbc_Decode(c->w, data, nblocks);
}

/* ---- SHA-1, as RAR 3 uses it ----
 *
 * RAR 3's key derivation hashes the same buffer 2^18 times, and the SHA-1 it was written with
 * stores its message schedule back into the caller's buffer whenever an update call hashes a
 * second (or later) whole block straight from that buffer: the 16 words left after the 80 rounds,
 * little-endian, over the 64 bytes just hashed. Later rounds then hash the changed bytes. Every
 * RAR 3/4 archive with a password of 28 or more characters depends on this, so it is kept. */

typedef struct {
    uint32_t h[5];
    uint32_t w[16]; /* the block being filled, as big-endian words; after a transform, W[64..79] */
    uint64_t count;
} rsha1_t;

#define ROL(x, n) (((x) << (n)) | ((x) >> (32 - (n))))

static void rsha1_init(rsha1_t *s) {
    s->h[0] = 0x67452301u;
    s->h[1] = 0xEFCDAB89u;
    s->h[2] = 0x98BADCFEu;
    s->h[3] = 0x10325476u;
    s->h[4] = 0xC3D2E1F0u;
    s->count = 0;
}

/* One block from s->w, expanding the schedule in place (s->w ends as W[64..79]). */
static void rsha1_block(rsha1_t *s) {
    uint32_t a = s->h[0], b = s->h[1], c = s->h[2], d = s->h[3], e = s->h[4], *w = s->w;
    for (unsigned i = 0; i < 80; i++) {
        uint32_t f, k, t;
        if (i >= 16) w[i & 15] = ROL(w[(i + 13) & 15] ^ w[(i + 8) & 15] ^ w[(i + 2) & 15] ^ w[i & 15], 1);
        if (i < 20) {
            f = (b & c) | (~b & d);
            k = 0x5A827999u;
        } else if (i < 40) {
            f = b ^ c ^ d;
            k = 0x6ED9EBA1u;
        } else if (i < 60) {
            f = (b & c) | (b & d) | (c & d);
            k = 0x8F1BBCDCu;
        } else {
            f = b ^ c ^ d;
            k = 0xCA62C1D6u;
        }
        t = ROL(a, 5) + f + e + k + w[i & 15];
        e = d;
        d = c;
        c = ROL(b, 30);
        b = a;
        a = t;
    }
    s->h[0] += a;
    s->h[1] += b;
    s->h[2] += c;
    s->h[3] += d;
    s->h[4] += e;
}

/* `rar`: write the schedule back over whole blocks after the first in this call (see above). */
static void rsha1_update(rsha1_t *s, uint8_t *data, size_t n, int rar) {
    unsigned pos = (unsigned)(s->count & 63);
    int blocks = 0;
    s->count += n;
    for (size_t i = 0; i < n; i++) {
        unsigned sh = 8 * (3 - (pos & 3));
        if ((pos & 3) == 0) s->w[pos >> 2] = (uint32_t)data[i] << sh;
        else s->w[pos >> 2] |= (uint32_t)data[i] << sh;
        if (++pos == 64) {
            pos = 0;
            rsha1_block(s);
            if (rar && blocks++ > 0) {
                uint8_t *blk = data + i + 1 - 64;
                for (unsigned k = 0; k < 16; k++) {
                    blk[4 * k] = (uint8_t)s->w[k];
                    blk[4 * k + 1] = (uint8_t)(s->w[k] >> 8);
                    blk[4 * k + 2] = (uint8_t)(s->w[k] >> 16);
                    blk[4 * k + 3] = (uint8_t)(s->w[k] >> 24);
                }
            }
        }
    }
}

static void rsha1_final(rsha1_t *s, uint32_t out[5]) {
    uint64_t bits = s->count * 8;
    uint8_t pad[72];
    unsigned pos = (unsigned)(s->count & 63), n = (pos < 56 ? 56 : 120) - pos;
    memset(pad, 0, sizeof pad);
    pad[0] = 0x80;
    for (unsigned i = 0; i < 8; i++) pad[n + i] = (uint8_t)(bits >> (56 - 8 * i));
    rsha1_update(s, pad, n + 8, 0);
    memcpy(out, s->h, 20);
}

/* ---- the caches: the derivations are deliberately slow, and an archive reuses its salt ---- */

typedef struct {
    int used;
    uint8_t salt[16];
    unsigned lg2; /* RAR5 */
    int salted;   /* RAR 3 */
    uint8_t key[32], iv_or_hash[32], check[8];
} kcache_t;

static kcache_t g_k3[4], g_k5[4];
static unsigned g_k3_next, g_k5_next;

void xrc_forget(void) {
    xrc_wipe(g_k3, sizeof g_k3);
    xrc_wipe(g_k5, sizeof g_k5);
}

/* UTF-8 to UTF-16LE (at most XRC_PW_CHARS units); bad bytes count as U+FFFD. Returns the bytes. */
static size_t utf16le(const char *s, uint8_t *out) {
    const unsigned char *p = (const unsigned char *)s;
    size_t n = 0;
    while (*p && n < XRC_PW_CHARS) {
        uint32_t c = *p++;
        int more = 0;
        if (c >= 0xF0 && c < 0xF8) {
            c &= 7;
            more = 3;
        } else if (c >= 0xE0) {
            c &= 15;
            more = 2;
        } else if (c >= 0xC0) {
            c &= 31;
            more = 1;
        } else if (c >= 0x80) {
            c = 0xFFFD;
        }
        while (more-- > 0) {
            if ((*p & 0xC0) != 0x80) {
                c = 0xFFFD;
                break;
            }
            c = (c << 6) | (*p++ & 0x3F);
        }
        if (c > 0xFFFF) {
            if (n + 2 > XRC_PW_CHARS) break;
            c -= 0x10000;
            out[2 * n] = (uint8_t)((0xD800 | (c >> 10)) & 0xFF);
            out[2 * n + 1] = (uint8_t)((0xD800 | (c >> 10)) >> 8);
            n++;
            c = 0xDC00 | (c & 0x3FF);
        }
        out[2 * n] = (uint8_t)c;
        out[2 * n + 1] = (uint8_t)(c >> 8);
        n++;
    }
    return 2 * n;
}

int xrc_kdf3(const char *password, const uint8_t *salt8, uint8_t key[16], uint8_t iv[16]) {
    enum { ROUNDS = 1 << 18 };
    uint8_t buf[2 * XRC_PW_CHARS + 8];
    uint32_t d[5];
    rsha1_t s;
    size_t len;
    for (unsigned i = 0; i < 4; i++) {
        kcache_t *k = &g_k3[i];
        if (k->used && k->salted == (salt8 != NULL) && (!salt8 || memcmp(k->salt, salt8, 8) == 0)) {
            memcpy(key, k->key, 16);
            memcpy(iv, k->iv_or_hash, 16);
            return 0;
        }
    }
    len = utf16le(password, buf);
    if (salt8) {
        memcpy(buf + len, salt8, 8);
        len += 8;
    }
    rsha1_init(&s);
    for (uint32_t i = 0; i < ROUNDS; i++) {
        uint8_t num[3] = {(uint8_t)i, (uint8_t)(i >> 8), (uint8_t)(i >> 16)};
        rsha1_update(&s, buf, len, 1);
        rsha1_update(&s, num, 3, 0);
        if (i % (ROUNDS / 16) == 0) {
            rsha1_t t = s;
            rsha1_final(&t, d);
            iv[i / (ROUNDS / 16)] = (uint8_t)d[4];
            xrc_wipe(&t, sizeof t);
        }
    }
    rsha1_final(&s, d);
    for (unsigned i = 0; i < 4; i++)
        for (unsigned j = 0; j < 4; j++) key[i * 4 + j] = (uint8_t)(d[i] >> (8 * j));
    {
        kcache_t *k = &g_k3[g_k3_next++ % 4];
        k->used = 1;
        k->salted = salt8 != NULL;
        if (salt8) memcpy(k->salt, salt8, 8);
        memcpy(k->key, key, 16);
        memcpy(k->iv_or_hash, iv, 16);
    }
    xrc_wipe(buf, sizeof buf);
    xrc_wipe(&s, sizeof s);
    xrc_wipe(d, sizeof d);
    return 0;
}

/* ---- HMAC-SHA256 and RAR5's PBKDF2 ---- */

typedef struct {
    CSha256 in, out;
} hmac_t;

static void hmac_init(hmac_t *h, const uint8_t *key, size_t n) {
    uint8_t k[64], pad[64];
    memset(k, 0, sizeof k);
    if (n > 64) {
        CSha256 s;
        Sha256_Init(&s);
        Sha256_Update(&s, key, n);
        Sha256_Final(&s, k);
    } else if (n) {
        memcpy(k, key, n);
    }
    for (unsigned i = 0; i < 64; i++) pad[i] = k[i] ^ 0x36;
    Sha256_Init(&h->in);
    Sha256_Update(&h->in, pad, 64);
    for (unsigned i = 0; i < 64; i++) pad[i] = k[i] ^ 0x5c;
    Sha256_Init(&h->out);
    Sha256_Update(&h->out, pad, 64);
    xrc_wipe(k, sizeof k);
    xrc_wipe(pad, sizeof pad);
}

static void hmac_run(const hmac_t *h, const uint8_t *msg, size_t n, uint8_t out[32]) {
    CSha256 s = h->in;
    uint8_t inner[32];
    Sha256_Update(&s, msg, n);
    Sha256_Final(&s, inner);
    s = h->out;
    Sha256_Update(&s, inner, 32);
    Sha256_Final(&s, out);
}

/* The UTF-8 password, cut at XRC_PW_CHARS characters. */
static size_t pw5_len(const char *pw) {
    size_t n = 0, chars = 0;
    while (pw[n] && chars < XRC_PW_CHARS) {
        n++;
        while (((unsigned char)pw[n] & 0xC0) == 0x80) n++;
        chars++;
    }
    return n;
}

int xrc_kdf5(const char *password, const uint8_t salt[16], unsigned lg2, uint8_t key[32], uint8_t hashkey[32],
             uint8_t check[8]) {
    uint8_t s1[20], u[32], f[32], v[3][32];
    hmac_t h;
    uint32_t counts[3];
    if (lg2 > 24) return -1;
    for (unsigned i = 0; i < 4; i++) {
        kcache_t *k = &g_k5[i];
        if (k->used && k->lg2 == lg2 && memcmp(k->salt, salt, 16) == 0) {
            memcpy(key, k->key, 32);
            memcpy(hashkey, k->iv_or_hash, 32);
            memcpy(check, k->check, 8);
            return 0;
        }
    }
    /* PBKDF2-HMAC-SHA256, one block; RAR5 then runs the same chain on for 16 more iterations for the
     * checksum key and 16 more for the password check. */
    hmac_init(&h, (const uint8_t *)password, pw5_len(password));
    memcpy(s1, salt, 16);
    s1[16] = 0;
    s1[17] = 0;
    s1[18] = 0;
    s1[19] = 1;
    hmac_run(&h, s1, sizeof s1, u);
    memcpy(f, u, 32);
    counts[0] = (1u << lg2) - 1;
    counts[1] = 16;
    counts[2] = 16;
    for (unsigned r = 0; r < 3; r++) {
        for (uint32_t i = 0; i < counts[r]; i++) {
            hmac_run(&h, u, 32, u);
            for (unsigned b = 0; b < 32; b++) f[b] ^= u[b];
        }
        memcpy(v[r], f, 32);
    }
    memcpy(key, v[0], 32);
    memcpy(hashkey, v[1], 32);
    memset(check, 0, 8);
    for (unsigned b = 0; b < 32; b++) check[b % 8] ^= v[2][b];
    {
        kcache_t *k = &g_k5[g_k5_next++ % 4];
        k->used = 1;
        k->lg2 = lg2;
        memcpy(k->salt, salt, 16);
        memcpy(k->key, key, 32);
        memcpy(k->iv_or_hash, hashkey, 32);
        memcpy(k->check, check, 8);
    }
    xrc_wipe(&h, sizeof h);
    xrc_wipe(u, sizeof u);
    xrc_wipe(f, sizeof f);
    xrc_wipe(v, sizeof v);
    return 0;
}

int xrc_check5_valid(const uint8_t stored[12]) {
    uint8_t d[32];
    CSha256 s;
    Sha256_Init(&s);
    Sha256_Update(&s, stored, 8);
    Sha256_Final(&s, d);
    return memcmp(d, stored + 8, 4) == 0;
}

uint32_t xrc_mac_crc(const uint8_t hashkey[32], uint32_t crc) {
    uint8_t raw[4] = {(uint8_t)crc, (uint8_t)(crc >> 8), (uint8_t)(crc >> 16), (uint8_t)(crc >> 24)}, d[32];
    uint32_t r = 0;
    hmac_t h;
    hmac_init(&h, hashkey, 32);
    hmac_run(&h, raw, 4, d);
    for (unsigned i = 0; i < 32; i++) r ^= (uint32_t)d[i] << ((i & 3) * 8);
    xrc_wipe(&h, sizeof h);
    return r;
}

void xrc_mac_blake(const uint8_t hashkey[32], const uint8_t digest[32], uint8_t out[32]) {
    hmac_t h;
    hmac_init(&h, hashkey, 32);
    hmac_run(&h, digest, 32, out);
    xrc_wipe(&h, sizeof h);
}
