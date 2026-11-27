/* RAR encryption: the key derivations and the AES-CBC decryption that password-protected RAR 3/4
 * (AES-128) and RAR5 (AES-256) archives use. Written from the RAR format's description, not from
 * UnRAR (whose license the extractor cannot take, see xrar.c); the primitives are 7-Zip's AES and
 * SHA-256 (C code, LGPL) plus a SHA-1 of our own, which RAR 3's key derivation needs in a form no
 * library offers (below). */
#ifndef PS5UPLOAD_XRCRYPT_H
#define PS5UPLOAD_XRCRYPT_H

#include <stddef.h>
#include <stdint.h>

/* Once, before the rest (idempotent). */
void xrc_init(void);

/* AES-CBC decryption; the chaining value carries over between calls. 16-byte aligned. */
typedef struct {
    _Alignas(16) uint32_t w[(1 + 1 + 15) * 4];
} xrc_aes_t;
void xrc_aes_init(xrc_aes_t *c, const uint8_t *key, unsigned keylen, const uint8_t iv[16]);
/* `data` 16-byte aligned, `nblocks` blocks of 16, in place. */
void xrc_aes_dec(xrc_aes_t *c, uint8_t *data, size_t nblocks);

/* RAR 3/4: the AES-128 key and IV from a password (UTF-8, used as UTF-16LE) and an 8-byte salt
 * (NULL: no salt). 0, or -1 when the password is too long. Results are cached. */
int xrc_kdf3(const char *password, const uint8_t *salt8, uint8_t key[16], uint8_t iv[16]);

/* RAR5: from the password (UTF-8), a 16-byte salt and log2 of the iteration count, the AES-256 key,
 * the key for checksums (`hashkey`) and the 8-byte password check. 0, or -1 (count out of range).
 * Results are cached. */
int xrc_kdf5(const char *password, const uint8_t salt[16], unsigned lg2, uint8_t key[32], uint8_t hashkey[32],
             uint8_t check[8]);

/* RAR5's stored check value (12 bytes: check, then the first 4 bytes of its SHA-256): 1 when its
 * own checksum holds (the check can be trusted), else 0. */
int xrc_check5_valid(const uint8_t stored[12]);

/* RAR5 files whose checksums are keyed ("tweaked"): the stored CRC32 / BLAKE2sp digest is this
 * function of the real one. */
uint32_t xrc_mac_crc(const uint8_t hashkey[32], uint32_t crc);
void xrc_mac_blake(const uint8_t hashkey[32], const uint8_t digest[32], uint8_t out[32]);

/* Clears the cached keys (they derive from the password). */
void xrc_forget(void);

/* Zeroes memory in a way the compiler keeps. */
void xrc_wipe(void *p, size_t n);

#endif
