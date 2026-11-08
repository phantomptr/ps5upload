/* A minimal HTTP/2 client (the RFC 7540 successor) over the Noise stream (spec 3.5-3.6):
 * prior knowledge, POST only, a few concurrent streams, no push or priorities. */
#include "tn.h"

#include <stdio.h>
#include <string.h>

enum { F_DATA = 0, F_HEADERS = 1, F_PRIORITY = 2, F_RST = 3, F_SETTINGS = 4, F_PUSH = 5,
       F_PING = 6, F_GOAWAY = 7, F_WINDOW = 8, F_CONT = 9 };
enum { FL_END_STREAM = 1, FL_ACK = 1, FL_END_HEADERS = 4, FL_PADDED = 8, FL_PRIORITY = 0x20 };

#define OUR_STREAM_WINDOW (1u << 20)
#define OUR_CONN_WINDOW (16u << 20)
#define WINDOW_ACK_AT (256u << 10)

static int send_frame(tn_h2 *h, uint8_t type, uint8_t flags, uint32_t sid, const void *p, size_t n)
{
	if (n > TN_H2_FRAME_MAX)
		return TN_E_TOOBIG;
	uint8_t *o = h->out;
	o[0] = (uint8_t)(n >> 16);
	o[1] = (uint8_t)(n >> 8);
	o[2] = (uint8_t)n;
	o[3] = type;
	o[4] = flags;
	tn_put_be32(o + 5, sid & 0x7fffffff);
	if (n)
		memcpy(o + 9, p, n);
	return tn_nconn_write(h->nc, o, 9 + n);
}

static int send_window(tn_h2 *h, uint32_t sid, uint32_t inc)
{
	uint8_t b[4];
	tn_put_be32(b, inc & 0x7fffffff);
	return send_frame(h, F_WINDOW, 0, sid, b, 4);
}

int tn_h2_start(tn_h2 *h, tn_nconn *nc)
{
	static const char preface[] = "PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
	uint8_t first[9];
	memset(h->st, 0, sizeof h->st);
	h->nc = nc;
	h->next_id = 1;
	h->conn_unacked = 0;
	h->peer_max_frame = 16384;
	h->goaway = 0;
	h->hdr_len = 0;
	h->hdr_stream = 0;
	h->npend = 0;
	tn_hpack_init(&h->dec, TN_HPACK_TABLE);

	/* The first 9 plaintext bytes are an early payload marker or the server's
	 * first frame header (ts2021/conn.go:81-91). */
	int r = tn_nconn_read_full(nc, first, 9);
	if (r < 0)
		return r;
	if (!memcmp(first, "\xff\xff\xff" "TS", 5)) {
		uint32_t len = tn_be32(first + 5);
		uint8_t skip[512];
		if (len > 65536)
			return TN_E_TOOBIG;
		while (len) { /* EarlyNoise JSON: only for posture/attestation, ignored */
			size_t m = len < sizeof skip ? len : sizeof skip;
			if ((r = tn_nconn_read_full(nc, skip, m)) < 0)
				return r;
			len -= (uint32_t)m;
		}
	} else {
		memcpy(h->pend, first, 9);
		h->npend = 9;
	}

	if ((r = tn_nconn_write(nc, preface, sizeof preface - 1)) < 0)
		return r;
	uint8_t s[18];
	/* ENABLE_PUSH = 0, INITIAL_WINDOW_SIZE, HEADER_TABLE_SIZE */
	tn_put_be16(s, 2);  tn_put_be32(s + 2, 0);
	tn_put_be16(s + 6, 4); tn_put_be32(s + 8, OUR_STREAM_WINDOW);
	tn_put_be16(s + 12, 1); tn_put_be32(s + 14, TN_HPACK_TABLE);
	if ((r = send_frame(h, F_SETTINGS, 0, 0, s, sizeof s)) < 0)
		return r;
	return send_window(h, 0, OUR_CONN_WINDOW - 65535);
}

int tn_h2_post(tn_h2 *h, const char *authority, const char *path, const char *body, size_t n,
	       uint8_t *sink, size_t sink_cap)
{
	int slot = -1;
	for (int i = 0; i < TN_H2_STREAMS; i++)
		if (!h->st[i].id) {
			slot = i;
			break;
		}
	if (slot < 0)
		return TN_E_TOOBIG;
	if (h->goaway || h->next_id > 0x7ffff000)
		return TN_E_IO; /* reconnect for fresh stream ids */
	if (n > 60000)
		return TN_E_TOOBIG; /* stays inside the peer's default 65535 window */

	uint8_t hb[1024];
	char clen[16];
	size_t o = 0;
	int w;
	snprintf(clen, sizeof clen, "%zu", n);
	/* :method POST (static 3), :scheme https (static 7) as indexed fields */
	hb[o++] = 0x83;
	hb[o++] = 0x87;
	const char *f[][2] = {{":authority", authority}, {":path", path},
			      {"content-type", "application/json"}, {"content-length", clen}};
	for (size_t i = 0; i < sizeof f / sizeof f[0]; i++) {
		if ((w = tn_hpack_put(hb + o, sizeof hb - o, f[i][0], f[i][1])) < 0)
			return TN_E_TOOBIG;
		o += (size_t)w;
	}
	tn_h2_stream *st = &h->st[slot];
	memset(st, 0, sizeof *st);
	st->id = h->next_id;
	h->next_id += 2;
	st->body = sink;
	st->cap = sink_cap;
	int r = send_frame(h, F_HEADERS, FL_END_HEADERS | (n ? 0 : FL_END_STREAM), st->id, hb, o);
	while (r >= 0 && n) {
		size_t m = n < h->peer_max_frame ? n : h->peer_max_frame;
		if (m > TN_H2_FRAME_MAX)
			m = TN_H2_FRAME_MAX;
		n -= m;
		r = send_frame(h, F_DATA, n ? 0 : FL_END_STREAM, st->id, body, m);
		body += m;
	}
	return r < 0 ? r : slot;
}

void tn_h2_release(tn_h2 *h, int slot)
{
	if (slot >= 0 && slot < TN_H2_STREAMS) {
		if (h->st[slot].id && !h->st[slot].ended && !h->st[slot].reset) {
			uint8_t code[4] = {0, 0, 0, 8}; /* CANCEL */
			send_frame(h, F_RST, 0, h->st[slot].id, code, 4);
		}
		memset(&h->st[slot], 0, sizeof h->st[slot]);
	}
}

static int find(tn_h2 *h, uint32_t sid)
{
	for (int i = 0; i < TN_H2_STREAMS; i++)
		if (h->st[i].id && h->st[i].id == sid)
			return i;
	return -1;
}

static int on_header(void *u, const char *name, size_t nl, const char *value, size_t vl)
{
	tn_h2_stream *st = u;
	if (st && nl == 7 && !memcmp(name, ":status", 7) && vl == 3)
		st->status = (value[0] - '0') * 100 + (value[1] - '0') * 10 + (value[2] - '0');
	/* Retry-After in seconds (429/503); the HTTP-date form is ignored. */
	if (st && nl == 11 && !memcmp(name, "retry-after", 11) && vl > 0 && vl < 8) {
		int32_t s = 0;
		for (size_t i = 0; i < vl && s >= 0; i++)
			s = value[i] >= '0' && value[i] <= '9' ? s * 10 + (value[i] - '0') : -1;
		if (s > 0)
			st->retry_after_s = s;
	}
	return 0;
}

/* Strip padding (and priority) from HEADERS/DATA payloads. */
static int unpad(uint8_t flags, int prio, const uint8_t **p, size_t *n)
{
	size_t pad = 0;
	if (flags & FL_PADDED) {
		if (*n < 1)
			return TN_E_PROTO;
		pad = (*p)[0];
		(*p)++;
		(*n)--;
	}
	if (prio) {
		if (*n < 5)
			return TN_E_PROTO;
		*p += 5;
		*n -= 5;
	}
	if (pad > *n)
		return TN_E_PROTO;
	*n -= pad;
	return TN_OK;
}

int tn_h2_step(tn_h2 *h, int *slot)
{
	uint8_t fh[9];
	int r;
	*slot = -1;
	if (h->npend) {
		memcpy(fh, h->pend, 9);
		h->npend = 0;
	} else if ((r = tn_nconn_read_full(h->nc, fh, 9)) < 0) {
		return r;
	}
	size_t len = (size_t)fh[0] << 16 | (size_t)fh[1] << 8 | fh[2];
	uint8_t type = fh[3], flags = fh[4];
	uint32_t sid = tn_be32(fh + 5) & 0x7fffffff;
	if (len > TN_H2_FRAME_MAX)
		return TN_E_PROTO; /* we never raised SETTINGS_MAX_FRAME_SIZE */
	if ((r = tn_nconn_read_full(h->nc, h->frame, len)) < 0)
		return r;
	const uint8_t *p = h->frame;
	size_t n = len;
	int i = sid ? find(h, sid) : -1;

	if (h->hdr_stream && type != F_CONT)
		return TN_E_PROTO;

	switch (type) {
	case F_DATA: {
		if ((r = unpad(flags, 0, &p, &n)) < 0)
			return r;
		h->conn_unacked += (uint32_t)len;
		if (i >= 0) {
			tn_h2_stream *st = &h->st[i];
			if (n > st->cap - st->len) {
				st->overflow = 1; /* dropped; the consumer decides if that is fatal */
			} else {
				memcpy(st->body + st->len, p, n);
				st->len += n;
			}
			st->unacked += (uint32_t)len;
			if (flags & FL_END_STREAM)
				st->ended = 1;
			else if (st->unacked >= WINDOW_ACK_AT) {
				if ((r = send_window(h, st->id, st->unacked)) < 0)
					return r;
				st->unacked = 0;
			}
			*slot = i;
		}
		if (h->conn_unacked >= WINDOW_ACK_AT) {
			if ((r = send_window(h, 0, h->conn_unacked)) < 0)
				return r;
			h->conn_unacked = 0;
		}
		return TN_OK;
	}
	case F_HEADERS:
		if ((r = unpad(flags, flags & FL_PRIORITY, &p, &n)) < 0)
			return r;
		if (n > sizeof h->hdr)
			return TN_E_TOOBIG;
		memcpy(h->hdr, p, n);
		h->hdr_len = n;
		if (!(flags & FL_END_HEADERS)) {
			h->hdr_stream = sid;
			h->hdr_flags = flags;
			return TN_OK;
		}
		break;
	case F_CONT:
		if (sid != h->hdr_stream || n > sizeof h->hdr - h->hdr_len)
			return TN_E_PROTO;
		memcpy(h->hdr + h->hdr_len, p, n);
		h->hdr_len += n;
		if (!(flags & FL_END_HEADERS))
			return TN_OK;
		h->hdr_stream = 0;
		flags = h->hdr_flags;
		break;
	case F_RST:
		if (i >= 0) {
			h->st[i].reset = 1;
			*slot = i;
		}
		return TN_OK;
	case F_SETTINGS:
		if (flags & FL_ACK)
			return TN_OK;
		if (n % 6)
			return TN_E_PROTO;
		for (size_t k = 0; k < n; k += 6) {
			uint16_t id = tn_be16(p + k);
			uint32_t v = tn_be32(p + k + 2);
			if (id == 5 && v >= 16384 && v <= 16777215)
				h->peer_max_frame = v;
			/* HEADER_TABLE_SIZE only bounds an encoder table; ours never indexes. */
		}
		return send_frame(h, F_SETTINGS, FL_ACK, 0, NULL, 0);
	case F_PING:
		if (n != 8)
			return TN_E_PROTO;
		if (flags & FL_ACK)
			return TN_OK;
		return send_frame(h, F_PING, FL_ACK, 0, p, 8);
	case F_GOAWAY:
		h->goaway = 1;
		return TN_OK; /* open streams may still finish; the caller reconnects after */
	case F_PUSH:
		return TN_E_PROTO; /* we disabled push */
	default:
		return TN_OK; /* WINDOW_UPDATE, PRIORITY, unknown: ignore (our bodies are small) */
	}

	/* A complete header block: decode even for unknown streams to keep HPACK in sync. */
	i = find(h, sid);
	r = tn_hpack_decode(&h->dec, h->hdr, h->hdr_len, on_header, i >= 0 ? &h->st[i] : NULL);
	if (r < 0)
		return r;
	if (i >= 0) {
		if (flags & FL_END_STREAM)
			h->st[i].ended = 1;
		*slot = i;
	}
	return TN_OK;
}
