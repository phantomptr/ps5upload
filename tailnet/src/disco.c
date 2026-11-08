/* Disco packets (spec 6) and Tailscale-flavoured STUN (spec 5). Pure functions.
 * Layouts follow tailscale disco/disco.go and net/stun/stun.go (BSD-3-Clause). */
#include "tn.h"
#include "crypto.h"

#include <string.h>

const uint8_t tn_disco_magic[6] = {'T', 'S', 0xf0, 0x9f, 0x92, 0xac};

int tn_disco_is(const uint8_t *p, size_t n)
{
	return n >= TN_DISCO_HDR + 16 && !memcmp(p, tn_disco_magic, 6);
}

size_t tn_disco_seal(uint8_t *out, const uint8_t my_pub[32], const uint8_t shared[32], const uint8_t *pt,
		     size_t n)
{
	memcpy(out, tn_disco_magic, 6);
	memcpy(out + 6, my_pub, 32);
	tn_random(out + 38, 24);
	tn_secretbox_seal(out + TN_DISCO_HDR, pt, n, out + 38, shared);
	return TN_DISCO_HDR + 16 + n;
}

int tn_disco_open(uint8_t *pt, const uint8_t *pkt, size_t n, const uint8_t shared[32])
{
	if (!tn_disco_is(pkt, n))
		return -1;
	size_t m = n - TN_DISCO_HDR;
	if (tn_secretbox_open(pt, pkt + TN_DISCO_HDR, m, pkt + 38, shared) < 0)
		return -1;
	return (int)(m - 16);
}

/* IPv4-mapped IPv6 (16 bytes) + BE port, as disco Pong and CallMeMaybe carry them. */
void tn_disco_put_ep(uint8_t out[18], tn_ep4 ep)
{
	memset(out, 0, 10);
	out[10] = out[11] = 0xff;
	tn_put_be32(out + 12, ep.ip);
	tn_put_be16(out + 16, ep.port);
}

int tn_disco_get_ep(const uint8_t in[18], tn_ep4 *ep)
{
	static const uint8_t v4pre[12] = {0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff};
	if (memcmp(in, v4pre, 12) != 0)
		return -1; /* IPv6: not used by this node */
	ep->ip = tn_be32(in + 12);
	ep->port = tn_be16(in + 16);
	return 0;
}

/* ---- STUN ---- */

uint32_t tn_crc32(const uint8_t *p, size_t n)
{
	uint32_t c = 0xffffffffu;
	for (size_t i = 0; i < n; i++) {
		c ^= p[i];
		for (int k = 0; k < 8; k++)
			c = (c >> 1) ^ (0xedb88320u & (0u - (c & 1)));
	}
	return ~c;
}

#define STUN_COOKIE 0x2112a442u

size_t tn_stun_request(uint8_t out[TN_STUN_REQ_LEN], const uint8_t txid[12])
{
	/* Binding request, SOFTWARE "tailnode", FINGERPRINT: the Tailscale STUN server
	 * drops requests without both (stun.go:17-71, stunserver.go:106-124). */
	tn_put_be16(out, 0x0001);
	tn_put_be16(out + 2, 20);
	tn_put_be32(out + 4, STUN_COOKIE);
	memcpy(out + 8, txid, 12);
	tn_put_be16(out + 20, 0x8022);
	tn_put_be16(out + 22, 8);
	memcpy(out + 24, "tailnode", 8);
	tn_put_be16(out + 32, 0x8028);
	tn_put_be16(out + 34, 4);
	tn_put_be32(out + 36, tn_crc32(out, 32) ^ 0x5354554eu);
	return TN_STUN_REQ_LEN;
}

int tn_stun_is(const uint8_t *p, size_t n)
{
	return n >= 20 && (p[0] & 0xc0) == 0 && tn_be32(p + 4) == STUN_COOKIE;
}

int tn_stun_parse(const uint8_t *p, size_t n, uint8_t txid[12], tn_ep4 *ep)
{
	if (!tn_stun_is(p, n) || tn_be16(p) != 0x0101)
		return -1;
	size_t len = tn_be16(p + 2);
	if (20 + len > n)
		return -1;
	memcpy(txid, p + 8, 12);
	int have = 0;
	for (size_t off = 20; off + 4 <= 20 + len;) {
		uint16_t t = tn_be16(p + off), l = tn_be16(p + off + 2);
		const uint8_t *v = p + off + 4;
		if (off + 4 + l > 20 + len)
			return -1;
		if ((t == 0x0020 || t == 0x8020) && l >= 8 && v[1] == 0x01) {
			ep->port = (uint16_t)(tn_be16(v + 2) ^ (STUN_COOKIE >> 16));
			ep->ip = tn_be32(v + 4) ^ STUN_COOKIE;
			have = 2;
		} else if (t == 0x0001 && l >= 8 && v[1] == 0x01 && have < 2) {
			ep->port = tn_be16(v + 2);
			ep->ip = tn_be32(v + 4);
			have = 1;
		}
		off += 4 + (((size_t)l + 3) & ~(size_t)3);
	}
	return have ? 0 : -1;
}
