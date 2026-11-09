/* IPv4 parse/build helpers for the tunnel (spec 10): checksums, UDP, ICMP echo. */
#include "tn.h"

#include <string.h>

static uint32_t csum_add(uint32_t sum, const uint8_t *p, size_t n)
{
	for (; n > 1; n -= 2, p += 2)
		sum += (uint32_t)p[0] << 8 | p[1];
	if (n)
		sum += (uint32_t)p[0] << 8;
	return sum;
}

static uint16_t csum_fold(uint32_t sum)
{
	while (sum >> 16)
		sum = (sum & 0xffff) + (sum >> 16);
	return (uint16_t)~sum;
}

uint16_t tn_ip_csum(const uint8_t *p, size_t n) { return csum_fold(csum_add(0, p, n)); }

static uint32_t pseudo(uint32_t src, uint32_t dst, uint8_t proto, size_t len)
{
	return (src >> 16) + (src & 0xffff) + (dst >> 16) + (dst & 0xffff) + proto + (uint32_t)len;
}

uint16_t tn_l4_csum(uint32_t src, uint32_t dst, uint8_t proto, const uint8_t *seg, size_t n)
{
	return csum_fold(csum_add(pseudo(src, dst, proto, n), seg, n));
}

int tn_ip_parse(const uint8_t *p, size_t n, tn_ip_info *o)
{
	memset(o, 0, sizeof *o);
	if (n < 20 || (p[0] >> 4) != 4)
		return -1;
	size_t hl = (size_t)(p[0] & 15) * 4, tl = tn_be16(p + 2);
	if (hl < 20 || tl < hl || tl > n)
		return -1;
	if (tn_ip_csum(p, hl) != 0)
		return -1;
	if ((tn_be16(p + 6) & 0x3fff) != 0)
		return -1; /* fragments are dropped (spec 10) */
	o->proto = p[9];
	o->src = tn_be32(p + 12);
	o->dst = tn_be32(p + 16);
	o->hlen = hl;
	o->len = tl;
	const uint8_t *l4 = p + hl;
	size_t l4n = tl - hl;
	switch (o->proto) {
	case TN_IPPROTO_UDP:
		if (l4n < 8 || tn_be16(l4 + 4) < 8 || tn_be16(l4 + 4) > l4n)
			return -1;
		l4n = tn_be16(l4 + 4);
		if (tn_be16(l4 + 6) != 0 && tn_l4_csum(o->src, o->dst, o->proto, l4, l4n) != 0)
			return -1; /* zero = no checksum, allowed for IPv4 */
		o->sport = tn_be16(l4);
		o->dport = tn_be16(l4 + 2);
		o->payload = l4 + 8;
		o->plen = l4n - 8;
		break;
	case TN_IPPROTO_TCP:
		if (l4n < 20 || (size_t)(l4[12] >> 4) * 4 < 20 || (size_t)(l4[12] >> 4) * 4 > l4n)
			return -1;
		if (tn_l4_csum(o->src, o->dst, o->proto, l4, l4n) != 0)
			return -1;
		o->sport = tn_be16(l4);
		o->dport = tn_be16(l4 + 2);
		o->tcp_flags = l4[13];
		o->payload = l4 + (l4[12] >> 4) * 4;
		o->plen = l4n - (size_t)(l4[12] >> 4) * 4;
		break;
	case TN_IPPROTO_ICMP:
		if (l4n < 8 || tn_ip_csum(l4, l4n) != 0)
			return -1;
		o->icmp_type = l4[0];
		o->sport = tn_be16(l4 + 4); /* echo id */
		o->dport = tn_be16(l4 + 6); /* echo seq */
		o->payload = l4 + 8;
		o->plen = l4n - 8;
		break;
	default:
		o->payload = l4;
		o->plen = l4n;
		break;
	}
	return 0;
}

static void ip_header(uint8_t *out, uint8_t proto, uint32_t src, uint32_t dst, size_t total)
{
	static uint16_t id;
	memset(out, 0, 20);
	out[0] = 0x45;
	tn_put_be16(out + 2, (uint16_t)total);
	tn_put_be16(out + 4, ++id);
	out[6] = 0x40; /* DF */
	out[8] = 64;
	out[9] = proto;
	tn_put_be32(out + 12, src);
	tn_put_be32(out + 16, dst);
	tn_put_be16(out + 10, tn_ip_csum(out, 20));
}

size_t tn_ip_build_udp(uint8_t *out, size_t cap, tn_ep4 src, tn_ep4 dst, const uint8_t *data, size_t n)
{
	size_t total = 28 + n;
	if (total > cap || total > 0xffff)
		return 0;
	memmove(out + 28, data, n);
	ip_header(out, TN_IPPROTO_UDP, src.ip, dst.ip, total);
	uint8_t *u = out + 20;
	tn_put_be16(u, src.port);
	tn_put_be16(u + 2, dst.port);
	tn_put_be16(u + 4, (uint16_t)(8 + n));
	tn_put_be16(u + 6, 0);
	uint16_t c = tn_l4_csum(src.ip, dst.ip, TN_IPPROTO_UDP, u, 8 + n);
	tn_put_be16(u + 6, c ? c : 0xffff);
	return total;
}

size_t tn_ip_build_icmp_echo(uint8_t *out, size_t cap, uint32_t src, uint32_t dst, uint8_t type, uint16_t id,
			     uint16_t seq, const uint8_t *data, size_t n)
{
	size_t total = 28 + n;
	if (total > cap || total > 0xffff)
		return 0;
	memmove(out + 28, data, n);
	ip_header(out, TN_IPPROTO_ICMP, src, dst, total);
	uint8_t *ic = out + 20;
	ic[0] = type;
	ic[1] = 0;
	tn_put_be16(ic + 2, 0);
	tn_put_be16(ic + 4, id);
	tn_put_be16(ic + 6, seq);
	tn_put_be16(ic + 2, tn_ip_csum(ic, 8 + n));
	return total;
}

int tn_peer_allowed(const tn_peer_allow *a, uint32_t ip)
{
	if (a->all)
		return 1;
	for (uint32_t i = 0; i < a->n && i < TN_ALLOW_MAX; i++)
		if (a->ip[i] == ip)
			return 1;
	return 0;
}

void tn_denied_note(tn_denied_log *log, uint32_t ip, uint8_t proto, uint16_t port, int64_t now_s)
{
	uint32_t n = log->n < TN_DENIED_MAX ? log->n : TN_DENIED_MAX;
	for (uint32_t i = 0; i < n; i++) {
		tn_denied *e = &log->e[i];
		if (e->ip != ip)
			continue;
		if (e->count < UINT32_MAX)
			e->count++;
		e->last_s = now_s;
		e->proto = proto;
		e->port = port;
		log->seq[i] = ++log->next;
		return;
	}
	uint32_t slot = n;
	if (n == TN_DENIED_MAX) {
		slot = 0; /* the one noted longest ago (by order: many arrive in one second) */
		for (uint32_t i = 1; i < n; i++)
			if (log->seq[i] < log->seq[slot])
				slot = i;
	} else {
		log->n = n + 1;
	}
	log->e[slot] = (tn_denied){.ip = ip, .proto = proto, .port = port, .count = 1, .first_s = now_s, .last_s = now_s};
	log->seq[slot] = ++log->next;
}

void tn_denied_prune(tn_denied_log *log, const tn_peer_allow *a)
{
	uint32_t k = 0, n = log->n < TN_DENIED_MAX ? log->n : TN_DENIED_MAX;
	for (uint32_t i = 0; i < n; i++)
		if (!tn_peer_allowed(a, log->e[i].ip)) {
			log->seq[k] = log->seq[i];
			log->e[k++] = log->e[i];
		}
	log->n = k;
}

int tn_denied_copy(const tn_denied_log *log, tn_denied *out, int max)
{
	uint32_t n = log->n < TN_DENIED_MAX ? log->n : TN_DENIED_MAX;
	uint32_t order[TN_DENIED_MAX];
	for (uint32_t i = 0; i < n; i++) {
		/* insertion by when it was last noted, newest first (eight entries at most) */
		uint32_t j = i;
		while (j > 0 && log->seq[order[j - 1]] < log->seq[i]) {
			order[j] = order[j - 1];
			j--;
		}
		order[j] = i;
	}
	int k = max < (int)n ? max : (int)n;
	for (int i = 0; i < k; i++)
		out[i] = log->e[order[i]];
	return k < 0 ? 0 : k;
}
