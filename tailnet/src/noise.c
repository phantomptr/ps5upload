/* ts2021 control channel: Noise_IK_25519_ChaChaPoly_BLAKE2s client and the record
 * layer (spec 3.3-3.4). Ported from tailscale control/controlbase handshake.go and
 * conn.go (BSD-3-Clause, Tailscale Inc & contributors). */
#include "tn.h"
#include "crypto.h"

#include <stdio.h>
#include <string.h>

static void mix_hash(tn_noise_hs *s, const uint8_t *d, size_t n)
{
	tn_b2s b;
	tn_b2s_init(&b, 32, NULL, 0);
	tn_b2s_update(&b, s->h, 32);
	tn_b2s_update(&b, d, n);
	tn_b2s_final(&b, s->h);
}

/* (ck, k) = HKDF(ck, DH(priv, pub)) — handshake.go MixDH */
static int mix_dh(tn_noise_hs *s, const uint8_t priv[32], const uint8_t pub[32], uint8_t k[32])
{
	uint8_t ss[32];
	int r = tn_x25519(ss, priv, pub);
	tn_kdf(s->ck, ss, 32, s->ck, k, NULL);
	tn_wipe(ss, sizeof ss);
	return r;
}

static const uint8_t zero_nonce[12];

/* EncryptAndHash with a single-use key: nonce 0, AD = h. */
static void encrypt_and_hash(tn_noise_hs *s, const uint8_t k[32], uint8_t *out, const uint8_t *pt, size_t n)
{
	tn_aead_seal(out, k, zero_nonce, s->h, 32, pt, n);
	mix_hash(s, out, n + 16);
}

int tn_noise_initiate(tn_noise_hs *s, uint8_t out[TN_NOISE_INIT_LEN], uint16_t capver,
		      const uint8_t machine_priv[32], const uint8_t control_pub[32], const uint8_t *e_priv)
{
	static const char proto[] = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
	char prologue[48];
	uint8_t k[32], machine_pub[32];
	int bad = 0;

	tn_b2s256(s->h, proto, sizeof proto - 1);
	memcpy(s->ck, s->h, 32);
	int pn = snprintf(prologue, sizeof prologue, "Tailscale Control Protocol v%u", capver);
	mix_hash(s, (const uint8_t *)prologue, (size_t)pn);
	memcpy(s->control_pub, control_pub, 32);
	mix_hash(s, control_pub, 32);

	if (e_priv)
		memcpy(s->e_priv, e_priv, 32);
	else
		tn_random(s->e_priv, 32);
	tn_x25519_public(s->e_pub, s->e_priv);
	mix_hash(s, s->e_pub, 32);

	tn_put_be16(out, capver);
	out[2] = 0x01; /* msgTypeInitiation */
	tn_put_be16(out + 3, 96);
	memcpy(out + 5, s->e_pub, 32);
	/* es */
	bad |= mix_dh(s, s->e_priv, control_pub, k);
	tn_x25519_public(machine_pub, machine_priv);
	encrypt_and_hash(s, k, out + 37, machine_pub, 32);
	/* ss */
	bad |= mix_dh(s, machine_priv, control_pub, k);
	encrypt_and_hash(s, k, out + 85, NULL, 0);
	tn_wipe(k, sizeof k);
	return bad ? TN_E_CRYPTO : TN_OK;
}

int tn_noise_finish(tn_noise_hs *s, const uint8_t resp[TN_NOISE_RESP_LEN],
		    const uint8_t machine_priv[32], tn_nconn *nc)
{
	uint8_t k[32], dummy[1];
	int bad = 0;
	if (resp[0] != 0x02 || tn_be16(resp + 1) != 48)
		return TN_E_PROTO;
	const uint8_t *re = resp + 3, *tag = resp + 35;
	mix_hash(s, re, 32);
	bad |= mix_dh(s, s->e_priv, re, k);       /* ee: only ck matters */
	bad |= mix_dh(s, machine_priv, re, k);    /* se */
	if (bad || tn_aead_open(dummy, k, zero_nonce, s->h, 32, tag, 16) != 0) {
		tn_wipe(k, sizeof k);
		return TN_E_CRYPTO;
	}
	mix_hash(s, tag, 16);
	/* Split: k1 = client->server, k2 = server->client (handshake.go:177-186). */
	tn_kdf(s->ck, NULL, 0, nc->tx_key, nc->rx_key, NULL);
	nc->tx_n = nc->rx_n = 0;
	nc->pt_pos = nc->pt_len = 0;
	tn_wipe(k, sizeof k);
	tn_wipe(s, sizeof *s);
	return TN_OK;
}

/* Nonce: 4 zero bytes, then a big-endian counter (conn.go:385-396). */
static void rec_nonce(uint8_t n[12], uint64_t c)
{
	memset(n, 0, 4);
	for (int i = 0; i < 8; i++)
		n[4 + i] = (uint8_t)(c >> (56 - 8 * i));
}

size_t tn_noise_seal_record(tn_nconn *nc, uint8_t *out, const uint8_t *pt, size_t n)
{
	uint8_t nonce[12];
	rec_nonce(nonce, nc->tx_n++);
	out[0] = 0x04; /* msgTypeRecord */
	tn_put_be16(out + 1, (uint16_t)(n + 16));
	tn_aead_seal(out + 3, nc->tx_key, nonce, NULL, 0, pt, n);
	return n + 19;
}

static int read_record(tn_nconn *nc)
{
	uint8_t nonce[12];
	int r = tn_tr_read_full(nc->tr, nc->rec, 3);
	if (r < 0)
		return r;
	size_t len = tn_be16(nc->rec + 1);
	if (nc->rec[0] != 0x04 || len < 16 || len > TN_NOISE_MAX_REC - 3)
		return TN_E_PROTO;
	r = tn_tr_read_full(nc->tr, nc->rec + 3, len);
	if (r < 0)
		return r;
	if (nc->rx_n == UINT64_MAX)
		return TN_E_PROTO;
	rec_nonce(nonce, nc->rx_n++);
	if (tn_aead_open(nc->pt, nc->rx_key, nonce, NULL, 0, nc->rec + 3, len) != 0)
		return TN_E_CRYPTO;
	nc->pt_pos = 0;
	nc->pt_len = len - 16;
	return TN_OK;
}

int tn_nconn_read(tn_nconn *nc, void *buf, size_t n)
{
	while (nc->pt_pos == nc->pt_len) {
		int r = read_record(nc);
		if (r < 0)
			return r;
	}
	size_t m = nc->pt_len - nc->pt_pos;
	if (m > n)
		m = n;
	memcpy(buf, nc->pt + nc->pt_pos, m);
	nc->pt_pos += m;
	return (int)m;
}

int tn_nconn_read_full(tn_nconn *nc, void *buf, size_t n)
{
	uint8_t *p = buf;
	while (n > 0) {
		int r = tn_nconn_read(nc, p, n);
		if (r < 0)
			return r;
		p += r;
		n -= (size_t)r;
	}
	return TN_OK;
}

int tn_nconn_write(tn_nconn *nc, const void *buf, size_t n)
{
	const uint8_t *p = buf;
	uint8_t out[TN_NOISE_MAX_REC];
	while (n > 0) {
		size_t m = n < TN_NOISE_MAX_PT ? n : TN_NOISE_MAX_PT;
		size_t w = tn_noise_seal_record(nc, out, p, m);
		int r = tn_tr_write(nc->tr, out, w);
		if (r < 0)
			return r;
		p += m;
		n -= m;
	}
	return TN_OK;
}
