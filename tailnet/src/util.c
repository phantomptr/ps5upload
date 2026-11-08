#include "tn.h"

#include <string.h>

int64_t tn_now_ms(void) { return tn_mono_us() / 1000; }

int64_t tn_wall_s(void) { return tn_wall_ns() / 1000000000; }

size_t tn_strlcpy(char *dst, const char *src, size_t cap)
{
	size_t n = strlen(src);
	if (cap) {
		size_t m = n < cap - 1 ? n : cap - 1;
		memcpy(dst, src, m);
		dst[m] = 0;
	}
	return n;
}

void tn_hex(char *out, const uint8_t *in, size_t n)
{
	static const char d[] = "0123456789abcdef";
	for (size_t i = 0; i < n; i++) {
		out[2 * i] = d[in[i] >> 4];
		out[2 * i + 1] = d[in[i] & 15];
	}
	out[2 * n] = 0;
}

static int hv(char c)
{
	if (c >= '0' && c <= '9') return c - '0';
	if (c >= 'a' && c <= 'f') return c - 'a' + 10;
	if (c >= 'A' && c <= 'F') return c - 'A' + 10;
	return -1;
}

int tn_unhex(uint8_t *out, size_t n, const char *in, size_t inlen)
{
	if (inlen != 2 * n)
		return -1;
	for (size_t i = 0; i < n; i++) {
		int a = hv(in[2 * i]), b = hv(in[2 * i + 1]);
		if (a < 0 || b < 0)
			return -1;
		out[i] = (uint8_t)(a << 4 | b);
	}
	return 0;
}

size_t tn_b64(char *out, size_t cap, const uint8_t *in, size_t n)
{
	static const char a[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
	size_t need = (n + 2) / 3 * 4;
	if (cap < need + 1)
		return 0;
	size_t o = 0;
	for (size_t i = 0; i < n; i += 3) {
		uint32_t v = (uint32_t)in[i] << 16;
		if (i + 1 < n) v |= (uint32_t)in[i + 1] << 8;
		if (i + 2 < n) v |= in[i + 2];
		out[o++] = a[v >> 18 & 63];
		out[o++] = a[v >> 12 & 63];
		out[o++] = i + 1 < n ? a[v >> 6 & 63] : '=';
		out[o++] = i + 2 < n ? a[v & 63] : '=';
	}
	out[o] = 0;
	return o;
}

int tn_parse_ip4(const char *s, size_t n, uint32_t *ip)
{
	uint32_t v = 0;
	int parts = 0;
	size_t i = 0;
	while (parts < 4) {
		uint32_t x = 0;
		size_t st = i;
		while (i < n && s[i] >= '0' && s[i] <= '9' && i - st < 3)
			x = x * 10 + (uint32_t)(s[i++] - '0');
		if (i == st || x > 255)
			return -1;
		v = v << 8 | x;
		parts++;
		if (parts < 4) {
			if (i >= n || s[i] != '.')
				return -1;
			i++;
		}
	}
	if (i != n)
		return -1;
	*ip = v;
	return 0;
}

int tn_parse_prefix4(const char *s, size_t n, tn_prefix4 *p)
{
	const char *slash = memchr(s, '/', n);
	size_t ipn = slash ? (size_t)(slash - s) : n;
	if (tn_parse_ip4(s, ipn, &p->addr) < 0)
		return -1;
	p->bits = 32;
	if (slash) {
		unsigned b = 0;
		size_t i = ipn + 1;
		if (i == n || n - i > 2)
			return -1;
		for (; i < n; i++) {
			if (s[i] < '0' || s[i] > '9')
				return -1;
			b = b * 10 + (unsigned)(s[i] - '0');
		}
		if (b > 32)
			return -1;
		p->bits = (uint8_t)b;
	}
	return 0;
}

int tn_parse_ep4(const char *s, size_t n, tn_ep4 *ep)
{
	const char *colon = NULL;
	for (size_t i = n; i > 0; i--)
		if (s[i - 1] == ':') {
			colon = s + i - 1;
			break;
		}
	if (!colon || s[0] == '[')
		return -1;
	size_t ipn = (size_t)(colon - s);
	uint32_t port = 0;
	size_t i = ipn + 1;
	if (i == n || n - i > 5)
		return -1;
	for (; i < n; i++) {
		if (s[i] < '0' || s[i] > '9')
			return -1;
		port = port * 10 + (uint32_t)(s[i] - '0');
	}
	if (port == 0 || port > 65535 || tn_parse_ip4(s, ipn, &ep->ip) < 0)
		return -1;
	ep->port = (uint16_t)port;
	return 0;
}

/* days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm) */
static int64_t days_from_civil(int64_t y, unsigned m, unsigned d)
{
	y -= m <= 2;
	int64_t era = (y >= 0 ? y : y - 399) / 400;
	unsigned yoe = (unsigned)(y - era * 400);
	unsigned doy = (153 * (m + (m > 2 ? -3 : 9)) + 2) / 5 + d - 1;
	unsigned doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
	return era * 146097 + (int64_t)doe - 719468;
}

static int num(const char *s, int n, int *v)
{
	*v = 0;
	for (int i = 0; i < n; i++) {
		if (s[i] < '0' || s[i] > '9')
			return -1;
		*v = *v * 10 + (s[i] - '0');
	}
	return 0;
}

int64_t tn_parse_time(const char *s, size_t n)
{
	int y, mo, d, h, mi, se;
	if (n < 20 || s[4] != '-' || s[7] != '-' || (s[10] != 'T' && s[10] != 't') || s[13] != ':' ||
	    s[16] != ':')
		return -1;
	if (num(s, 4, &y) || num(s + 5, 2, &mo) || num(s + 8, 2, &d) || num(s + 11, 2, &h) ||
	    num(s + 14, 2, &mi) || num(s + 17, 2, &se))
		return -1;
	if (y <= 1)
		return 0; /* Go's zero time.Time */
	if (mo < 1 || mo > 12 || d < 1 || d > 31 || h > 23 || mi > 59 || se > 60)
		return -1;
	size_t i = 19;
	if (i < n && s[i] == '.')
		for (i++; i < n && s[i] >= '0' && s[i] <= '9'; i++)
			;
	int64_t off = 0;
	if (i < n && (s[i] == 'Z' || s[i] == 'z')) {
		i++;
	} else if (i + 6 == n && (s[i] == '+' || s[i] == '-') && s[i + 3] == ':') {
		int oh, om;
		if (num(s + i + 1, 2, &oh) || num(s + i + 4, 2, &om))
			return -1;
		off = (oh * 3600 + om * 60) * (s[i] == '+' ? 1 : -1);
		i += 6;
	} else {
		return -1;
	}
	if (i != n)
		return -1;
	return days_from_civil(y, (unsigned)mo, (unsigned)d) * 86400 + h * 3600 + mi * 60 + se - off;
}
