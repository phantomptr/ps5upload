/* BLAKE2s, HMAC/HKDF-BLAKE2s, Salsa20/HSalsa20/XSalsa20, NaCl secretbox/box,
 * and wrappers over Monocypher (X25519, ChaCha20-Poly1305, Poly1305). */
#include "crypto.h"

#include <string.h>

#include "monocypher.h"

#ifdef _WIN32
#include "os.h"
#include <bcrypt.h>
#endif

#if defined(__PROSPERO__)
#include <sys/types.h>
#include <sys/sysctl.h>
#elif defined(__linux__)
#include <sys/syscall.h>
#include <unistd.h>
#include <errno.h>
#else
#include <stdlib.h>
#endif

static uint32_t ld32(const uint8_t *p)
{
	return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}

static void st32(uint8_t *p, uint32_t v)
{
	p[0] = (uint8_t)v; p[1] = (uint8_t)(v >> 8); p[2] = (uint8_t)(v >> 16); p[3] = (uint8_t)(v >> 24);
}

static uint32_t rotr(uint32_t x, int n) { return x >> n | x << (32 - n); }
static uint32_t rotl(uint32_t x, int n) { return x << n | x >> (32 - n); }

void tn_wipe(void *p, size_t n) { crypto_wipe(p, n); }

/* ---- BLAKE2s: ported from the RFC 7693 Appendix C reference code ---- */

static const uint32_t b2s_iv[8] = {
	0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A,
	0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19,
};

static const uint8_t b2s_sigma[10][16] = {
	{0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15},
	{14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3},
	{11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4},
	{7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8},
	{9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13},
	{2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9},
	{12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11},
	{13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10},
	{6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5},
	{10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0},
};

#define B2S_G(a, b, c, d, x, y)                     \
	do {                                        \
		v[a] = v[a] + v[b] + (x);           \
		v[d] = rotr(v[d] ^ v[a], 16);       \
		v[c] = v[c] + v[d];                 \
		v[b] = rotr(v[b] ^ v[c], 12);       \
		v[a] = v[a] + v[b] + (y);           \
		v[d] = rotr(v[d] ^ v[a], 8);        \
		v[c] = v[c] + v[d];                 \
		v[b] = rotr(v[b] ^ v[c], 7);        \
	} while (0)

static void b2s_compress(tn_b2s *s, int last)
{
	uint32_t v[16], m[16];
	for (int i = 0; i < 8; i++) {
		v[i] = s->h[i];
		v[i + 8] = b2s_iv[i];
	}
	v[12] ^= s->t[0];
	v[13] ^= s->t[1];
	if (last)
		v[14] = ~v[14];
	for (int i = 0; i < 16; i++)
		m[i] = ld32(s->buf + 4 * i);
	for (int r = 0; r < 10; r++) {
		const uint8_t *z = b2s_sigma[r];
		B2S_G(0, 4, 8, 12, m[z[0]], m[z[1]]);
		B2S_G(1, 5, 9, 13, m[z[2]], m[z[3]]);
		B2S_G(2, 6, 10, 14, m[z[4]], m[z[5]]);
		B2S_G(3, 7, 11, 15, m[z[6]], m[z[7]]);
		B2S_G(0, 5, 10, 15, m[z[8]], m[z[9]]);
		B2S_G(1, 6, 11, 12, m[z[10]], m[z[11]]);
		B2S_G(2, 7, 8, 13, m[z[12]], m[z[13]]);
		B2S_G(3, 4, 9, 14, m[z[14]], m[z[15]]);
	}
	for (int i = 0; i < 8; i++)
		s->h[i] ^= v[i] ^ v[i + 8];
}

void tn_b2s_init(tn_b2s *s, size_t outlen, const uint8_t *key, size_t keylen)
{
	if (outlen == 0 || outlen > 32)
		outlen = 32;
	if (keylen > 32)
		keylen = 32;
	memcpy(s->h, b2s_iv, sizeof s->h);
	s->h[0] ^= 0x01010000u ^ (uint32_t)(keylen << 8) ^ (uint32_t)outlen;
	s->t[0] = s->t[1] = 0;
	s->c = 0;
	s->outlen = outlen;
	memset(s->buf, 0, sizeof s->buf);
	if (keylen > 0) {
		memcpy(s->buf, key, keylen);
		s->c = 64;
	}
}

void tn_b2s_update(tn_b2s *s, const void *in, size_t n)
{
	const uint8_t *p = in;
	for (size_t i = 0; i < n; i++) {
		if (s->c == 64) {
			s->t[0] += 64;
			if (s->t[0] < 64)
				s->t[1]++;
			b2s_compress(s, 0);
			s->c = 0;
		}
		s->buf[s->c++] = p[i];
	}
}

void tn_b2s_final(tn_b2s *s, uint8_t *out)
{
	uint8_t full[32];
	s->t[0] += (uint32_t)s->c;
	if (s->t[0] < s->c)
		s->t[1]++;
	while (s->c < 64)
		s->buf[s->c++] = 0;
	b2s_compress(s, 1);
	for (int i = 0; i < 8; i++)
		st32(full + 4 * i, s->h[i]);
	memcpy(out, full, s->outlen);
	tn_wipe(full, sizeof full);
	tn_wipe(s, sizeof *s);
}

void tn_b2s256(uint8_t out[32], const void *in, size_t n)
{
	tn_b2s s;
	tn_b2s_init(&s, 32, NULL, 0);
	tn_b2s_update(&s, in, n);
	tn_b2s_final(&s, out);
}

void tn_hmac_b2s(uint8_t out[32], const uint8_t *key, size_t keylen, const void *in, size_t n)
{
	uint8_t k[64] = {0}, pad[64], inner[32];
	tn_b2s s;
	if (keylen > 64)
		tn_b2s256(k, key, keylen);
	else if (keylen)
		memcpy(k, key, keylen);
	for (int i = 0; i < 64; i++)
		pad[i] = k[i] ^ 0x36;
	tn_b2s_init(&s, 32, NULL, 0);
	tn_b2s_update(&s, pad, 64);
	tn_b2s_update(&s, in, n);
	tn_b2s_final(&s, inner);
	for (int i = 0; i < 64; i++)
		pad[i] = k[i] ^ 0x5c;
	tn_b2s_init(&s, 32, NULL, 0);
	tn_b2s_update(&s, pad, 64);
	tn_b2s_update(&s, inner, 32);
	tn_b2s_final(&s, out);
	tn_wipe(k, sizeof k);
	tn_wipe(pad, sizeof pad);
	tn_wipe(inner, sizeof inner);
}

/* Same construction as wireguard-go device/noise-helpers.go KDF1/2/3 and
 * tailscale control/controlbase/handshake.go hkdf (BSD-3 / MIT). */
void tn_kdf(const uint8_t ck[32], const uint8_t *ikm, size_t ikmlen,
	    uint8_t t1[32], uint8_t t2[32], uint8_t t3[32])
{
	uint8_t prk[32], a[32], buf[33];
	tn_hmac_b2s(prk, ck, 32, ikm, ikmlen);
	buf[0] = 1;
	tn_hmac_b2s(a, prk, 32, buf, 1);
	if (t1)
		memcpy(t1, a, 32);
	if (t2 || t3) {
		memcpy(buf, a, 32);
		buf[32] = 2;
		tn_hmac_b2s(a, prk, 32, buf, 33);
		if (t2)
			memcpy(t2, a, 32);
		if (t3) {
			memcpy(buf, a, 32);
			buf[32] = 3;
			tn_hmac_b2s(t3, prk, 32, buf, 33);
		}
	}
	tn_wipe(prk, sizeof prk);
	tn_wipe(a, sizeof a);
	tn_wipe(buf, sizeof buf);
}

/* ---- Salsa20 family: written from the Salsa20 specification
 * (Bernstein, "Salsa20 specification", 2005) and "Extending the Salsa20 nonce"
 * (XSalsa20/HSalsa20, 2008), checked against NaCl/x/crypto test vectors. ---- */

static void salsa_rounds(uint32_t x[16])
{
	for (int i = 0; i < 10; i++) {
		x[4] ^= rotl(x[0] + x[12], 7);   x[8] ^= rotl(x[4] + x[0], 9);
		x[12] ^= rotl(x[8] + x[4], 13);  x[0] ^= rotl(x[12] + x[8], 18);
		x[9] ^= rotl(x[5] + x[1], 7);    x[13] ^= rotl(x[9] + x[5], 9);
		x[1] ^= rotl(x[13] + x[9], 13);  x[5] ^= rotl(x[1] + x[13], 18);
		x[14] ^= rotl(x[10] + x[6], 7);  x[2] ^= rotl(x[14] + x[10], 9);
		x[6] ^= rotl(x[2] + x[14], 13);  x[10] ^= rotl(x[6] + x[2], 18);
		x[3] ^= rotl(x[15] + x[11], 7);  x[7] ^= rotl(x[3] + x[15], 9);
		x[11] ^= rotl(x[7] + x[3], 13);  x[15] ^= rotl(x[11] + x[7], 18);
		x[1] ^= rotl(x[0] + x[3], 7);    x[2] ^= rotl(x[1] + x[0], 9);
		x[3] ^= rotl(x[2] + x[1], 13);   x[0] ^= rotl(x[3] + x[2], 18);
		x[6] ^= rotl(x[5] + x[4], 7);    x[7] ^= rotl(x[6] + x[5], 9);
		x[4] ^= rotl(x[7] + x[6], 13);   x[5] ^= rotl(x[4] + x[7], 18);
		x[11] ^= rotl(x[10] + x[9], 7);  x[8] ^= rotl(x[11] + x[10], 9);
		x[9] ^= rotl(x[8] + x[11], 13);  x[10] ^= rotl(x[9] + x[8], 18);
		x[12] ^= rotl(x[15] + x[14], 7); x[13] ^= rotl(x[12] + x[15], 9);
		x[14] ^= rotl(x[13] + x[12], 13); x[15] ^= rotl(x[14] + x[13], 18);
	}
}

static void salsa_setup(uint32_t x[16], const uint8_t in[16], const uint8_t k[32])
{
	x[0] = 0x61707865; x[5] = 0x3320646e; x[10] = 0x79622d32; x[15] = 0x6b206574;
	for (int i = 0; i < 4; i++) {
		x[1 + i] = ld32(k + 4 * i);
		x[11 + i] = ld32(k + 16 + 4 * i);
		x[6 + i] = ld32(in + 4 * i);
	}
}

void tn_hsalsa20(uint8_t out[32], const uint8_t in[16], const uint8_t key[32])
{
	uint32_t x[16];
	salsa_setup(x, in, key);
	salsa_rounds(x);
	st32(out, x[0]); st32(out + 4, x[5]); st32(out + 8, x[10]); st32(out + 12, x[15]);
	st32(out + 16, x[6]); st32(out + 20, x[7]); st32(out + 24, x[8]); st32(out + 28, x[9]);
	tn_wipe(x, sizeof x);
}

static void salsa_block(uint8_t out[64], const uint8_t nonce[8], uint64_t ctr, const uint8_t key[32])
{
	uint8_t in[16];
	uint32_t x[16], o[16];
	memcpy(in, nonce, 8);
	for (int i = 0; i < 8; i++)
		in[8 + i] = (uint8_t)(ctr >> (8 * i));
	salsa_setup(x, in, key);
	memcpy(o, x, sizeof o);
	salsa_rounds(x);
	for (int i = 0; i < 16; i++)
		st32(out + 4 * i, x[i] + o[i]);
	tn_wipe(x, sizeof x);
	tn_wipe(o, sizeof o);
}

void tn_salsa20_xor(uint8_t *out, const uint8_t *in, size_t n, const uint8_t nonce[8],
		    uint64_t counter, const uint8_t key[32])
{
	uint8_t ks[64];
	while (n > 0) {
		size_t m = n < 64 ? n : 64;
		salsa_block(ks, nonce, counter++, key);
		for (size_t i = 0; i < m; i++)
			out[i] = (uint8_t)((in ? in[i] : 0) ^ ks[i]);
		out += m;
		if (in)
			in += m;
		n -= m;
	}
	tn_wipe(ks, sizeof ks);
}

void tn_xsalsa20_xor(uint8_t *out, const uint8_t *in, size_t n, const uint8_t nonce[24],
		     const uint8_t key[32])
{
	uint8_t sub[32];
	tn_hsalsa20(sub, nonce, key);
	tn_salsa20_xor(out, in, n, nonce + 16, 0, sub);
	tn_wipe(sub, sizeof sub);
}

/* secretbox: block 0 of the XSalsa20 stream gives the Poly1305 key (bytes 0..31)
 * and the keystream for the first 32 message bytes (bytes 32..63). */
static void secretbox_xor(uint8_t *out, const uint8_t *in, size_t n, const uint8_t nonce[24],
			  const uint8_t key[32], uint8_t polykey[32])
{
	uint8_t sub[32], b0[64];
	tn_hsalsa20(sub, nonce, key);
	salsa_block(b0, nonce + 16, 0, sub);
	memcpy(polykey, b0, 32);
	size_t first = n < 32 ? n : 32;
	for (size_t i = 0; i < first; i++)
		out[i] = in[i] ^ b0[32 + i];
	if (n > 32) {
		/* The rest continues the same stream from block 1. */
		tn_salsa20_xor(out + 32, in + 32, n - 32, nonce + 16, 1, sub);
	}
	tn_wipe(sub, sizeof sub);
	tn_wipe(b0, sizeof b0);
}

void tn_secretbox_seal(uint8_t *out, const uint8_t *msg, size_t n, const uint8_t nonce[24],
		       const uint8_t key[32])
{
	uint8_t pk[32];
	secretbox_xor(out + 16, msg, n, nonce, key, pk);
	crypto_poly1305(out, out + 16, n, pk);
	tn_wipe(pk, sizeof pk);
}

int tn_secretbox_open(uint8_t *out, const uint8_t *in, size_t n, const uint8_t nonce[24],
		      const uint8_t key[32])
{
	uint8_t sub[32], b0[64], tag[16];
	if (n < 16)
		return -1;
	tn_hsalsa20(sub, nonce, key);
	salsa_block(b0, nonce + 16, 0, sub);
	crypto_poly1305(tag, in + 16, n - 16, b0);
	int bad = crypto_verify16(tag, in);
	tn_wipe(sub, sizeof sub);
	tn_wipe(b0, sizeof b0);
	if (bad)
		return -1;
	secretbox_xor(out, in + 16, n - 16, nonce, key, sub /* poly key, unused here */);
	tn_wipe(sub, sizeof sub);
	return 0;
}

int tn_x25519(uint8_t out[32], const uint8_t priv[32], const uint8_t pub[32])
{
	static const uint8_t zero[32];
	crypto_x25519(out, priv, pub);
	return crypto_verify32(out, zero) == 0 ? -1 : 0;
}

void tn_x25519_public(uint8_t pub[32], const uint8_t priv[32])
{
	crypto_x25519_public_key(pub, priv);
}

int tn_box_beforenm(uint8_t k[32], const uint8_t pk[32], const uint8_t sk[32])
{
	static const uint8_t zero16[16];
	uint8_t s[32];
	int r = tn_x25519(s, sk, pk);
	tn_hsalsa20(k, zero16, s);
	tn_wipe(s, sizeof s);
	return r;
}

void tn_aead_seal(uint8_t *out, const uint8_t key[32], const uint8_t nonce[12],
		  const uint8_t *ad, size_t adlen, const uint8_t *pt, size_t n)
{
	crypto_aead_ctx c;
	crypto_aead_init_ietf(&c, key, nonce);
	crypto_aead_write(&c, out, out + n, ad, adlen, pt, n);
	tn_wipe(&c, sizeof c);
}

int tn_aead_open(uint8_t *out, const uint8_t key[32], const uint8_t nonce[12],
		 const uint8_t *ad, size_t adlen, const uint8_t *in, size_t n)
{
	crypto_aead_ctx c;
	if (n < 16)
		return -1;
	crypto_aead_init_ietf(&c, key, nonce);
	int r = crypto_aead_read(&c, out, in + n - 16, ad, adlen, in, n - 16);
	tn_wipe(&c, sizeof c);
	return r == 0 ? 0 : -1;
}

int tn_random(void *buf, size_t n)
{
	uint8_t *p = buf;
#if defined(__PROSPERO__)
	/* kern.arandom works on the PS5 (spec 11); at most 256 bytes per call. */
	while (n > 0) {
		int mib[2] = {CTL_KERN, KERN_ARND};
		size_t m = n < 256 ? n : 256;
		size_t got = m;
		if (sysctl(mib, 2, p, &got, NULL, 0) != 0 || got == 0)
			return -1;
		p += got;
		n -= got;
	}
	return 0;
#elif defined(_WIN32)
	return BCRYPT_SUCCESS(BCryptGenRandom(NULL, p, (ULONG)n, BCRYPT_USE_SYSTEM_PREFERRED_RNG)) ? 0 : -1;
#elif defined(__linux__)
	while (n > 0) {
		/* the syscall: Android's libc has getrandom() only from API 28 */
		long r = syscall(SYS_getrandom, p, n, 0);
		if (r < 0) {
			if (errno == EINTR)
				continue;
			return -1;
		}
		p += r;
		n -= (size_t)r;
	}
	return 0;
#else
	arc4random_buf(p, n);
	return 0;
#endif
}
