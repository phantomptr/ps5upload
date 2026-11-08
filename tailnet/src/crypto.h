/* Crypto primitives Tailscale needs that Monocypher lacks, plus thin wrappers. */
#ifndef TN_CRYPTO_H
#define TN_CRYPTO_H

#include <stddef.h>
#include <stdint.h>

/* BLAKE2s (RFC 7693), keyed or not, 1..32 byte output. */
typedef struct {
	uint32_t h[8], t[2];
	uint8_t buf[64];
	size_t c, outlen;
} tn_b2s;

void tn_b2s_init(tn_b2s *s, size_t outlen, const uint8_t *key, size_t keylen);
void tn_b2s_update(tn_b2s *s, const void *in, size_t n);
void tn_b2s_final(tn_b2s *s, uint8_t *out);
void tn_b2s256(uint8_t out[32], const void *in, size_t n);

/* HMAC-BLAKE2s-256 (RFC 2104 with a 64-byte block). */
void tn_hmac_b2s(uint8_t out[32], const uint8_t *key, size_t keylen, const void *in, size_t n);
/* HKDF-BLAKE2s as Noise/WireGuard use it: prk = HMAC(ck, ikm); T1 = HMAC(prk, 1);
 * T2 = HMAC(prk, T1 || 2); T3 = HMAC(prk, T2 || 3). Outputs may be NULL. */
void tn_kdf(const uint8_t ck[32], const uint8_t *ikm, size_t ikmlen,
	    uint8_t t1[32], uint8_t t2[32], uint8_t t3[32]);

/* Salsa20 family (Bernstein; NaCl). */
void tn_hsalsa20(uint8_t out[32], const uint8_t in[16], const uint8_t key[32]);
void tn_salsa20_xor(uint8_t *out, const uint8_t *in, size_t n, const uint8_t nonce[8],
		    uint64_t counter, const uint8_t key[32]);
void tn_xsalsa20_xor(uint8_t *out, const uint8_t *in, size_t n, const uint8_t nonce[24],
		     const uint8_t key[32]);

/* NaCl secretbox / box (XSalsa20-Poly1305). Wire form (Go's box.Seal): tag[16] || ct.
 * out must hold n + 16 bytes; out and msg must not overlap. */
void tn_secretbox_seal(uint8_t *out, const uint8_t *msg, size_t n, const uint8_t nonce[24],
		       const uint8_t key[32]);
/* in = tag || ct (n >= 16); writes n - 16 bytes. Returns 0 or -1 on a bad tag. */
int tn_secretbox_open(uint8_t *out, const uint8_t *in, size_t n, const uint8_t nonce[24],
		      const uint8_t key[32]);
/* crypto_box_beforenm: HSalsa20(X25519(sk, pk), 0). Returns -1 for an all-zero DH. */
int tn_box_beforenm(uint8_t k[32], const uint8_t pk[32], const uint8_t sk[32]);

/* X25519; returns -1 for an all-zero shared secret (low-order point). */
int tn_x25519(uint8_t out[32], const uint8_t priv[32], const uint8_t pub[32]);
void tn_x25519_public(uint8_t pub[32], const uint8_t priv[32]);

/* ChaCha20-Poly1305 IETF (RFC 8439). out = ct || tag (n + 16 bytes). A fresh
 * Monocypher context per call: crypto_aead_write ratchets its key (spec 11). */
void tn_aead_seal(uint8_t *out, const uint8_t key[32], const uint8_t nonce[12],
		  const uint8_t *ad, size_t adlen, const uint8_t *pt, size_t n);
/* in = ct || tag (n >= 16); writes n - 16 bytes. 0 or -1. */
int tn_aead_open(uint8_t *out, const uint8_t key[32], const uint8_t nonce[12],
		 const uint8_t *ad, size_t adlen, const uint8_t *in, size_t n);

void tn_wipe(void *p, size_t n);
int tn_random(void *buf, size_t n);

#endif
