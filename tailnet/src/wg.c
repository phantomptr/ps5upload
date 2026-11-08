/* WireGuard (Noise_IKpsk2_25519_ChaChaPoly_BLAKE2s) as Tailscale uses it: zero PSK,
 * node keys as statics, no persistent keepalive (spec 8). Pure protocol: no sockets,
 * no clocks of its own; dp.c owns the timers and the paths.
 *
 * Message math follows wireguard-go device/noise-protocol.go, cookie.go and
 * replay/replay.go (MIT); constants are from the WireGuard paper. */
#include "tn.h"
#include "crypto.h"
#include "monocypher.h"

#include <string.h>

static const char CONSTRUCTION[] = "Noise_IKpsk2_25519_ChaChaPoly_BLAKE2s";
static const char IDENTIFIER[] = "WireGuard v1 zx2c4 Jason@zx2c4.com";
static const uint8_t ZERO32[32];

static void mix_hash(uint8_t h[32], const void *data, size_t n)
{
	tn_b2s s;
	tn_b2s_init(&s, 32, NULL, 0);
	tn_b2s_update(&s, h, 32);
	tn_b2s_update(&s, data, n);
	tn_b2s_final(&s, h);
}

static void initial(uint8_t ck[32], uint8_t h[32])
{
	tn_b2s256(ck, CONSTRUCTION, sizeof CONSTRUCTION - 1);
	tn_b2s s;
	tn_b2s_init(&s, 32, NULL, 0);
	tn_b2s_update(&s, ck, 32);
	tn_b2s_update(&s, IDENTIFIER, sizeof IDENTIFIER - 1);
	tn_b2s_final(&s, h);
}

static void label_key(uint8_t out[32], const char *label, const uint8_t pub[32])
{
	tn_b2s s;
	tn_b2s_init(&s, 32, NULL, 0);
	tn_b2s_update(&s, label, 8);
	tn_b2s_update(&s, pub, 32);
	tn_b2s_final(&s, out);
}

static void mac16(uint8_t out[16], const uint8_t *key, size_t keylen, const uint8_t *msg, size_t n)
{
	tn_b2s s;
	tn_b2s_init(&s, 16, key, keylen);
	tn_b2s_update(&s, msg, n);
	tn_b2s_final(&s, out);
}

static const uint8_t NONCE0[12];

static void tai64n(uint8_t out[12], int64_t wall_ns)
{
	uint64_t sec = (uint64_t)(wall_ns / 1000000000) + 0x400000000000000aULL;
	uint32_t ns = (uint32_t)(wall_ns % 1000000000) & ~0xffffffu; /* whitened (tai64n.go) */
	for (int i = 0; i < 8; i++)
		out[i] = (uint8_t)(sec >> (56 - 8 * i));
	tn_put_be32(out + 8, ns);
}

void tn_wg_self_init(tn_wg_self *s, const uint8_t priv[32], const uint8_t pub[32])
{
	memcpy(s->priv, priv, 32);
	memcpy(s->pub, pub, 32);
	label_key(s->mac1_key, "mac1----", pub);
}

int tn_wg_peer_init(const tn_wg_self *s, tn_wg_peer *p, const uint8_t peer_pub[32])
{
	memset(p, 0, sizeof *p);
	memcpy(p->pub, peer_pub, 32);
	label_key(p->mac1_key, "mac1----", peer_pub);
	label_key(p->cookie_key, "cookie--", peer_pub);
	return tn_x25519(p->ss, s->priv, peer_pub) == 0 ? 0 : -1;
}

/* mac1 (and mac2 when a fresh cookie is held) over msg[0..len-32). */
static void put_macs(tn_wg_peer *p, uint8_t *msg, size_t len, int64_t now_ms)
{
	mac16(msg + len - 32, p->mac1_key, 32, msg, len - 32);
	memcpy(p->last_mac1, msg + len - 32, 16);
	if (p->has_cookie && now_ms - p->cookie_at < (TN_WG_COOKIE_REFRESH_MS - 2000))
		mac16(msg + len - 16, p->cookie, 16, msg, len - 16);
	else
		memset(msg + len - 16, 0, 16);
}

int tn_wg_check_mac1(const tn_wg_self *s, const uint8_t *msg, size_t len)
{
	uint8_t m[16];
	mac16(m, s->mac1_key, 32, msg, len - 32);
	return crypto_verify16(m, msg + len - 32) == 0 ? 0 : -1;
}

void tn_wg_create_init(tn_wg_self *s, tn_wg_peer *p, uint8_t out[TN_WG_INIT_LEN], uint32_t local_idx,
		       int64_t wall_ns, int64_t now_ms)
{
	uint8_t k[32], e_pub[32], dh[32], ts[12];
	initial(p->hs_ck, p->hs_h);
	mix_hash(p->hs_h, p->pub, 32);
	tn_key_new(p->hs_e_priv, e_pub);
	memset(out, 0, TN_WG_INIT_LEN);
	out[0] = TN_WG_MSG_INIT;
	tn_put_le32(out + 4, local_idx);
	memcpy(out + 8, e_pub, 32);
	tn_kdf(p->hs_ck, e_pub, 32, p->hs_ck, NULL, NULL);
	mix_hash(p->hs_h, e_pub, 32);
	if (tn_x25519(dh, p->hs_e_priv, p->pub) < 0)
		memset(dh, 0, 32); /* a low-order peer key: the peer will just not answer */
	tn_kdf(p->hs_ck, dh, 32, p->hs_ck, k, NULL);
	tn_aead_seal(out + 40, k, NONCE0, p->hs_h, 32, s->pub, 32);
	mix_hash(p->hs_h, out + 40, 48);
	tn_kdf(p->hs_ck, p->ss, 32, p->hs_ck, k, NULL);
	/* Strictly increasing timestamps, even if the wall clock steps back (spec 8.7). */
	tai64n(ts, wall_ns);
	if (memcmp(ts, s->last_ts_sent, 12) <= 0) {
		memcpy(ts, s->last_ts_sent, 12);
		for (int i = 11; i >= 0 && ++ts[i] == 0; i--)
			;
	}
	memcpy(s->last_ts_sent, ts, 12);
	tn_aead_seal(out + 88, k, NONCE0, p->hs_h, 32, ts, 12);
	mix_hash(p->hs_h, out + 88, 28);
	put_macs(p, out, TN_WG_INIT_LEN, now_ms);
	p->hs_local_idx = local_idx;
	p->hs_active = 1;
	tn_wipe(k, sizeof k);
	tn_wipe(dh, sizeof dh);
}

int tn_wg_init_open(const tn_wg_self *s, const uint8_t *msg, size_t len, tn_wg_rx_init *st)
{
	uint8_t k[32], dh[32];
	if (len != TN_WG_INIT_LEN || tn_wg_check_mac1(s, msg, len) < 0)
		return -1;
	initial(st->ck, st->h);
	mix_hash(st->h, s->pub, 32);
	memcpy(st->e_pub, msg + 8, 32);
	tn_kdf(st->ck, st->e_pub, 32, st->ck, NULL, NULL);
	mix_hash(st->h, st->e_pub, 32);
	if (tn_x25519(dh, s->priv, st->e_pub) < 0)
		return -1;
	tn_kdf(st->ck, dh, 32, st->ck, k, NULL);
	int r = tn_aead_open(st->peer_pub, k, NONCE0, st->h, 32, msg + 40, 48);
	tn_wipe(k, sizeof k);
	tn_wipe(dh, sizeof dh);
	if (r < 0)
		return -1;
	mix_hash(st->h, msg + 40, 48);
	st->sender = tn_le32(msg + 4);
	return 0;
}

int tn_wg_init_finish(tn_wg_peer *p, tn_wg_rx_init *st, const uint8_t *msg, int64_t now_ms)
{
	uint8_t k[32], ts[12];
	tn_kdf(st->ck, p->ss, 32, st->ck, k, NULL);
	int r = tn_aead_open(ts, k, NONCE0, st->h, 32, msg + 88, 28);
	tn_wipe(k, sizeof k);
	if (r < 0)
		return -1;
	/* Replayed or reordered initiation, or one too soon after the last (noise-protocol.go:553-559). */
	if (memcmp(ts, p->last_ts, 12) <= 0)
		return -2;
	if (p->last_init_rx && now_ms - p->last_init_rx < TN_WG_INIT_RATE_MS)
		return -2;
	memcpy(p->last_ts, ts, 12);
	p->last_init_rx = now_ms;
	mix_hash(st->h, msg + 88, 28);
	return 0;
}

static void keypair_set(tn_wg_keypair *kp, const uint8_t ck[32], int initiator, uint32_t local, uint32_t remote,
			int64_t now_ms)
{
	memset(kp, 0, sizeof *kp);
	if (initiator)
		tn_kdf(ck, NULL, 0, kp->send_key, kp->recv_key, NULL);
	else
		tn_kdf(ck, NULL, 0, kp->recv_key, kp->send_key, NULL);
	kp->local_idx = local;
	kp->remote_idx = remote;
	kp->created = now_ms;
	kp->initiator = (uint8_t)initiator;
	kp->valid = 1;
}

void tn_wg_create_resp(tn_wg_peer *p, tn_wg_rx_init *st, uint8_t out[TN_WG_RESP_LEN], uint32_t local_idx,
		       int64_t now_ms)
{
	uint8_t e_priv[32], e_pub[32], dh[32], tau[32], k[32];
	memset(out, 0, TN_WG_RESP_LEN);
	out[0] = TN_WG_MSG_RESP;
	tn_put_le32(out + 4, local_idx);
	tn_put_le32(out + 8, st->sender);
	tn_key_new(e_priv, e_pub);
	memcpy(out + 12, e_pub, 32);
	tn_kdf(st->ck, e_pub, 32, st->ck, NULL, NULL);
	mix_hash(st->h, e_pub, 32);
	if (tn_x25519(dh, e_priv, st->e_pub) < 0)
		memset(dh, 0, 32);
	tn_kdf(st->ck, dh, 32, st->ck, NULL, NULL);
	if (tn_x25519(dh, e_priv, p->pub) < 0)
		memset(dh, 0, 32);
	tn_kdf(st->ck, dh, 32, st->ck, NULL, NULL);
	tn_kdf(st->ck, ZERO32, 32, st->ck, tau, k); /* psk = 0 */
	mix_hash(st->h, tau, 32);
	tn_aead_seal(out + 44, k, NONCE0, st->h, 32, NULL, 0);
	mix_hash(st->h, out + 44, 16);
	put_macs(p, out, TN_WG_RESP_LEN, now_ms);
	/* Responder: the new keypair waits in `next` until the initiator's first data
	 * confirms it (WG timers: keypair confirmation). */
	keypair_set(&p->next, st->ck, 0, local_idx, st->sender, now_ms);
	tn_wipe(e_priv, sizeof e_priv);
	tn_wipe(dh, sizeof dh);
	tn_wipe(k, sizeof k);
	tn_wipe(st, sizeof *st);
}

int tn_wg_consume_resp(const tn_wg_self *s, tn_wg_peer *p, const uint8_t *msg, size_t len, int64_t now_ms)
{
	uint8_t ck[32], h[32], dh[32], tau[32], k[32], e_pub[32], empty[1];
	if (len != TN_WG_RESP_LEN || !p->hs_active || tn_le32(msg + 8) != p->hs_local_idx ||
	    tn_wg_check_mac1(s, msg, len) < 0)
		return -1;
	memcpy(ck, p->hs_ck, 32);
	memcpy(h, p->hs_h, 32);
	memcpy(e_pub, msg + 12, 32);
	tn_kdf(ck, e_pub, 32, ck, NULL, NULL);
	mix_hash(h, e_pub, 32);
	if (tn_x25519(dh, p->hs_e_priv, e_pub) < 0)
		return -1;
	tn_kdf(ck, dh, 32, ck, NULL, NULL);
	if (tn_x25519(dh, s->priv, e_pub) < 0)
		return -1;
	tn_kdf(ck, dh, 32, ck, NULL, NULL);
	tn_kdf(ck, ZERO32, 32, ck, tau, k);
	mix_hash(h, tau, 32);
	int r = tn_aead_open(empty, k, NONCE0, h, 32, msg + 44, 16);
	tn_wipe(k, sizeof k);
	tn_wipe(dh, sizeof dh);
	if (r < 0)
		return -1;
	/* Initiator: the keypair is usable at once. */
	p->prev = p->cur;
	keypair_set(&p->cur, ck, 1, p->hs_local_idx, tn_le32(msg + 4), now_ms);
	memset(&p->next, 0, sizeof p->next);
	p->hs_active = 0;
	tn_wipe(p->hs_e_priv, 32);
	tn_wipe(ck, sizeof ck);
	return 0;
}

int tn_wg_consume_cookie(tn_wg_peer *p, const uint8_t *msg, size_t len, int64_t now_ms)
{
	if (len != TN_WG_COOKIE_LEN)
		return -1;
	crypto_aead_ctx a;
	crypto_aead_init_x(&a, p->cookie_key, msg + 8);
	uint8_t ck[16];
	int r = crypto_aead_read(&a, ck, msg + 8 + 24 + 16, p->last_mac1, 16, msg + 32, 16);
	tn_wipe(&a, sizeof a);
	if (r != 0)
		return -1;
	memcpy(p->cookie, ck, 16);
	p->cookie_at = now_ms;
	p->has_cookie = 1;
	return 0;
}

void tn_wg_confirm_next(tn_wg_peer *p)
{
	p->prev = p->cur;
	p->cur = p->next;
	memset(&p->next, 0, sizeof p->next);
}

size_t tn_wg_seal(tn_wg_keypair *kp, uint8_t *out, const uint8_t *pt, size_t n)
{
	uint8_t nonce[12] = {0};
	size_t padded = (n + 15) & ~(size_t)15;
	uint64_t ctr = kp->send_n++;
	out[0] = TN_WG_MSG_DATA;
	out[1] = out[2] = out[3] = 0;
	tn_put_le32(out + 4, kp->remote_idx);
	tn_put_le32(out + 8, (uint32_t)ctr);
	tn_put_le32(out + 12, (uint32_t)(ctr >> 32));
	memcpy(nonce + 4, out + 8, 8);
	/* pad in place: the plaintext area of out is the ciphertext area */
	if (n && pt != out + 16)
		memmove(out + 16, pt, n);
	memset(out + 16 + n, 0, padded - n);
	tn_aead_seal(out + 16, kp->send_key, nonce, NULL, 0, out + 16, padded);
	return 16 + padded + 16;
}

/* Sliding-window replay check (RFC 6479; wireguard-go replay.go with a smaller ring). */
static int replay_ok(tn_wg_keypair *kp, uint64_t ctr)
{
	const uint64_t nblk = TN_WG_REPLAY_BLOCKS, win = (nblk - 1) * 64;
	if (ctr >= TN_WG_REJECT_AFTER_MSGS)
		return 0;
	uint64_t idx = ctr >> 6;
	if (ctr > kp->replay_last) { /* move the window forward */
		uint64_t cur = kp->replay_last >> 6, diff = idx - cur;
		if (diff > nblk)
			diff = nblk;
		for (uint64_t i = cur + 1; i <= cur + diff; i++)
			kp->replay[i % nblk] = 0;
		kp->replay_last = ctr;
	} else if (kp->replay_last - ctr > win) {
		return 0;
	}
	uint64_t *blk = &kp->replay[idx % nblk], bit = 1ULL << (ctr & 63);
	if (*blk & bit)
		return 0;
	*blk |= bit;
	return 1;
}

int tn_wg_open(tn_wg_keypair *kp, uint8_t *msg, size_t len, int64_t now_ms)
{
	uint8_t nonce[12] = {0};
	if (len < 32 || !kp->valid || now_ms - kp->created >= TN_WG_REJECT_AFTER_MS)
		return -1;
	memcpy(nonce + 4, msg + 8, 8);
	uint64_t ctr = (uint64_t)tn_le32(msg + 8) | (uint64_t)tn_le32(msg + 12) << 32;
	if (tn_aead_open(msg + 16, kp->recv_key, nonce, NULL, 0, msg + 16, len - 16) < 0)
		return -1;
	if (!replay_ok(kp, ctr))
		return -2;
	return (int)(len - 32);
}

tn_wg_keypair *tn_wg_send_keypair(tn_wg_peer *p, int64_t now_ms)
{
	tn_wg_keypair *kp = &p->cur;
	if (!kp->valid || now_ms - kp->created >= TN_WG_REJECT_AFTER_MS || kp->send_n >= TN_WG_REJECT_AFTER_MSGS)
		return NULL;
	return kp;
}

void tn_wg_reset(tn_wg_peer *p)
{
	tn_wipe(&p->cur, sizeof p->cur);
	tn_wipe(&p->prev, sizeof p->prev);
	tn_wipe(&p->next, sizeof p->next);
	tn_wipe(p->hs_e_priv, 32);
	p->hs_active = 0;
	p->hs_started = p->hs_sent = 0;
	p->last_init_rx = 0;
	p->keepalive_due = 0;
}
