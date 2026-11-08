/* HPACK (RFC 7541): a decoder with Huffman and the dynamic table, and a
 * literal-only encoder. Bounded: strings over TN_HPACK_STR_MAX are rejected. */
#include "tn.h"
#include "hpack_tables.h"

#include <string.h>

void tn_hpack_init(tn_hpack *h, uint32_t max_size)
{
	h->n_ent = h->used_bytes = h->size = 0;
	h->max_size = h->settings_max = max_size > TN_HPACK_TABLE ? TN_HPACK_TABLE : max_size;
}

int tn_huff_decode(const uint8_t *p, size_t n, char *out, size_t cap)
{
	uint32_t code = 0;
	unsigned len = 0;
	size_t o = 0;
	for (size_t i = 0; i < n; i++) {
		for (int b = 7; b >= 0; b--) {
			code = code << 1 | ((p[i] >> b) & 1);
			len++;
			if (len < 5)
				continue;
			if (len > 30)
				return -1;
			/* entries of this length are sorted by code: binary search */
			int lo = tn_huff_first[len], hi = tn_huff_first[len + 1 < 32 ? len + 1 : 31];
			if (len == 30)
				hi = 257;
			while (lo < hi) {
				int mid = (lo + hi) / 2;
				if (tn_huff[mid].code < code)
					lo = mid + 1;
				else
					hi = mid;
			}
			if (lo < 257 && tn_huff[lo].len == len && tn_huff[lo].code == code) {
				if (tn_huff[lo].sym == 256)
					return -1; /* EOS in a string is an error */
				if (o >= cap)
					return -2;
				out[o++] = (char)tn_huff[lo].sym;
				code = 0;
				len = 0;
			}
		}
	}
	/* Padding: at most 7 bits, all ones (a prefix of EOS). */
	if (len > 7 || code != (1u << len) - 1)
		return -1;
	return (int)o;
}

static int get_int(const uint8_t **pp, const uint8_t *end, int prefix, uint32_t *v)
{
	const uint8_t *p = *pp;
	if (p >= end)
		return -1;
	uint32_t max = (1u << prefix) - 1;
	uint32_t x = *p++ & max;
	if (x == max) {
		int shift = 0;
		uint8_t b;
		do {
			if (p >= end || shift > 21)
				return -1;
			b = *p++;
			x += (uint32_t)(b & 0x7f) << shift;
			shift += 7;
		} while (b & 0x80);
	}
	*pp = p;
	*v = x;
	return 0;
}

static int get_str(const uint8_t **pp, const uint8_t *end, char *out, size_t *outlen)
{
	const uint8_t *p = *pp;
	uint32_t n;
	if (p >= end)
		return TN_E_PROTO;
	int huff = *p & 0x80;
	if (get_int(&p, end, 7, &n) < 0 || n > (size_t)(end - p))
		return TN_E_PROTO;
	if (huff) {
		int r = tn_huff_decode(p, n, out, TN_HPACK_STR_MAX);
		if (r < 0)
			return r == -2 ? TN_E_TOOBIG : TN_E_PROTO;
		*outlen = (size_t)r;
	} else {
		if (n > TN_HPACK_STR_MAX)
			return TN_E_TOOBIG;
		memcpy(out, p, n);
		*outlen = n;
	}
	*pp = p + n;
	return TN_OK;
}

static void evict_one(tn_hpack *h)
{
	uint32_t len = (uint32_t)h->ent[0].nlen + h->ent[0].vlen;
	memmove(h->data, h->data + len, h->used_bytes - len);
	h->used_bytes -= len;
	h->size -= len + 32;
	memmove(h->ent, h->ent + 1, (h->n_ent - 1) * sizeof h->ent[0]);
	h->n_ent--;
	for (uint32_t i = 0; i < h->n_ent; i++)
		h->ent[i].off = (uint16_t)(h->ent[i].off - len);
}

static void evict_to(tn_hpack *h, uint32_t limit)
{
	while (h->n_ent && h->size > limit)
		evict_one(h);
}

static void table_add(tn_hpack *h, const char *n, size_t nl, const char *v, size_t vl)
{
	uint32_t sz = (uint32_t)(nl + vl + 32);
	evict_to(h, sz > h->max_size ? 0 : h->max_size - sz);
	if (sz > h->max_size)
		return; /* too big: the table is now empty (RFC 7541 4.4) */
	memcpy(h->data + h->used_bytes, n, nl);
	memcpy(h->data + h->used_bytes + nl, v, vl);
	h->ent[h->n_ent].off = (uint16_t)h->used_bytes;
	h->ent[h->n_ent].nlen = (uint16_t)nl;
	h->ent[h->n_ent].vlen = (uint16_t)vl;
	h->n_ent++;
	h->used_bytes += (uint32_t)(nl + vl);
	h->size += sz;
}

/* Index 1..61 static, 62.. dynamic (62 = newest). */
static int lookup(tn_hpack *h, uint32_t idx, const char **n, size_t *nl, const char **v, size_t *vl)
{
	if (idx == 0)
		return -1;
	if (idx <= 61) {
		*n = tn_hpack_static[idx].name;
		*nl = strlen(*n);
		*v = tn_hpack_static[idx].value;
		*vl = strlen(*v);
		return 0;
	}
	uint32_t d = idx - 62;
	if (d >= h->n_ent)
		return -1;
	uint32_t e = h->n_ent - 1 - d;
	*n = (const char *)h->data + h->ent[e].off;
	*nl = h->ent[e].nlen;
	*v = *n + *nl;
	*vl = h->ent[e].vlen;
	return 0;
}

int tn_hpack_decode(tn_hpack *h, const uint8_t *p, size_t n, tn_hpack_cb cb, void *u)
{
	const uint8_t *end = p + n;
	while (p < end) {
		uint8_t b = *p;
		const char *name, *value;
		size_t nl, vl;
		uint32_t idx;
		if (b & 0x80) { /* indexed field */
			if (get_int(&p, end, 7, &idx) < 0 || lookup(h, idx, &name, &nl, &value, &vl) < 0)
				return TN_E_PROTO;
			/* copy out: the callback may run while the table changes later */
		} else if ((b & 0xe0) == 0x20) { /* dynamic table size update */
			if (get_int(&p, end, 5, &idx) < 0 || idx > h->settings_max)
				return TN_E_PROTO;
			h->max_size = idx;
			evict_to(h, idx);
			continue;
		} else {
			int prefix = (b & 0xc0) == 0x40 ? 6 : 4; /* incremental indexing : without/never */
			int add = prefix == 6;
			if (get_int(&p, end, prefix, &idx) < 0)
				return TN_E_PROTO;
			if (idx) {
				const char *tn, *tv;
				size_t tnl, tvl;
				if (lookup(h, idx, &tn, &tnl, &tv, &tvl) < 0)
					return TN_E_PROTO;
				memcpy(h->sname, tn, tnl);
				nl = tnl;
			} else {
				int r = get_str(&p, end, h->sname, &nl);
				if (r < 0)
					return r;
			}
			int r = get_str(&p, end, h->svalue, &vl);
			if (r < 0)
				return r;
			if (add)
				table_add(h, h->sname, nl, h->svalue, vl);
			name = h->sname;
			value = h->svalue;
		}
		if (cb && cb(u, name, nl, value, vl) < 0)
			return TN_E_PROTO;
	}
	return TN_OK;
}

static int put_int(uint8_t *out, size_t cap, size_t *o, uint8_t first, int prefix, uint32_t v)
{
	uint32_t max = (1u << prefix) - 1;
	if (*o >= cap)
		return -1;
	if (v < max) {
		out[(*o)++] = (uint8_t)(first | v);
		return 0;
	}
	out[(*o)++] = (uint8_t)(first | max);
	v -= max;
	while (v >= 128) {
		if (*o >= cap)
			return -1;
		out[(*o)++] = (uint8_t)(v % 128 + 128);
		v /= 128;
	}
	if (*o >= cap)
		return -1;
	out[(*o)++] = (uint8_t)v;
	return 0;
}

int tn_hpack_put(uint8_t *out, size_t cap, const char *name, const char *value)
{
	size_t o = 0, nl = strlen(name), vl = strlen(value);
	if (put_int(out, cap, &o, 0x00, 4, 0) < 0 || put_int(out, cap, &o, 0x00, 7, (uint32_t)nl) < 0 ||
	    o + nl > cap)
		return -1;
	memcpy(out + o, name, nl);
	o += nl;
	if (put_int(out, cap, &o, 0x00, 7, (uint32_t)vl) < 0 || o + vl > cap)
		return -1;
	memcpy(out + o, value, vl);
	o += vl;
	return (int)o;
}
