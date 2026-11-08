#include "json.h"

#include <stdio.h>
#include <string.h>

static int tok_new(tn_json *j, uint8_t type, uint32_t start)
{
	if (j->n >= j->cap)
		return -2;
	tn_jtok *t = &j->t[j->n];
	t->type = type;
	t->start = start;
	t->end = start;
	t->next = j->n + 1;
	return (int)j->n++;
}

static int is_ws(char c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r'; }

static int hexv(char c)
{
	if (c >= '0' && c <= '9') return c - '0';
	if (c >= 'a' && c <= 'f') return c - 'a' + 10;
	if (c >= 'A' && c <= 'F') return c - 'A' + 10;
	return -1;
}

/* Validates a string body starting after the opening quote; returns the index of
 * the closing quote or -1. */
static long scan_string(const char *s, size_t len, size_t i)
{
	while (i < len) {
		unsigned char c = (unsigned char)s[i];
		if (c == '"')
			return (long)i;
		if (c < 0x20)
			return -1;
		if (c == '\\') {
			if (++i >= len)
				return -1;
			switch (s[i]) {
			case '"': case '\\': case '/': case 'b': case 'f': case 'n': case 'r': case 't':
				break;
			case 'u':
				if (i + 4 >= len)
					return -1;
				for (int k = 1; k <= 4; k++)
					if (hexv(s[i + k]) < 0)
						return -1;
				i += 4;
				break;
			default:
				return -1;
			}
		}
		i++;
	}
	return -1;
}

int tn_json_parse(tn_json *j, const char *s, size_t len)
{
	uint32_t stack[TN_JSON_MAX_DEPTH];
	int depth = 0;
	/* expect: 0 value, 1 key or '}', 2 ':', 3 ',' or close, 4 value or ']', 5 key */
	int expect = 0, done = 0;
	j->s = s;
	j->n = 0;
	if (len > 0xffffffffu)
		return -2;
	for (size_t i = 0; i < len; i++) {
		char c = s[i];
		if (is_ws(c))
			continue;
		if (done)
			return -1;
		int parent = depth ? (int)stack[depth - 1] : -1;
		int in_obj = parent >= 0 && j->t[parent].type == TN_J_OBJ;
		if (expect == 2) {
			if (c != ':')
				return -1;
			expect = 0;
			continue;
		}
		if (expect == 3) {
			if (c == ',') {
				expect = in_obj ? 5 : 0;
				continue;
			}
			if (c != (in_obj ? '}' : ']'))
				return -1;
			goto close;
		}
		if ((expect == 1 && c == '}') || (expect == 4 && c == ']'))
			goto close;
		if ((expect == 1 || expect == 5) && c != '"')
			return -1;
		/* a value (or an object key) */
		if (c == '{' || c == '[') {
			int t = tok_new(j, c == '{' ? TN_J_OBJ : TN_J_ARR, (uint32_t)i);
			if (t < 0)
				return t;
			if (depth >= TN_JSON_MAX_DEPTH)
				return -2;
			stack[depth++] = (uint32_t)t;
			expect = c == '{' ? 1 : 4;
			continue;
		}
		if (c == '"') {
			long e = scan_string(s, len, i + 1);
			if (e < 0)
				return -1;
			int t = tok_new(j, TN_J_STR, (uint32_t)i + 1);
			if (t < 0)
				return t;
			j->t[t].end = (uint32_t)e;
			int is_key = expect == 1 || expect == 5;
			i = (size_t)e;
			if (is_key) {
				expect = 2;
				continue;
			}
		} else {
			size_t k = i;
			uint8_t type;
			if (c == 't' && len - i >= 4 && !memcmp(s + i, "true", 4)) {
				type = TN_J_TRUE;
				k = i + 4;
			} else if (c == 'f' && len - i >= 5 && !memcmp(s + i, "false", 5)) {
				type = TN_J_FALSE;
				k = i + 5;
			} else if (c == 'n' && len - i >= 4 && !memcmp(s + i, "null", 4)) {
				type = TN_J_NULL;
				k = i + 4;
			} else if (c == '-' || (c >= '0' && c <= '9')) {
				type = TN_J_NUM;
				while (k < len && (s[k] == '-' || s[k] == '+' || s[k] == '.' || s[k] == 'e' ||
						   s[k] == 'E' || (s[k] >= '0' && s[k] <= '9')))
					k++;
			} else {
				return -1;
			}
			int t = tok_new(j, type, (uint32_t)i);
			if (t < 0)
				return t;
			j->t[t].end = (uint32_t)k;
			i = k - 1;
		}
		/* a complete scalar value */
		if (depth == 0)
			done = 1;
		expect = 3;
		continue;
	close:
		j->t[parent].end = (uint32_t)i + 1;
		j->t[parent].next = j->n;
		depth--;
		if (depth == 0)
			done = 1;
		expect = 3;
	}
	if (!done)
		return -1;
	return (int)j->n;
}

int tn_json_first(const tn_json *j, int tok)
{
	if (tok < 0 || (j->t[tok].type != TN_J_OBJ && j->t[tok].type != TN_J_ARR))
		return -1;
	return j->t[tok].next > (uint32_t)tok + 1 ? tok + 1 : -1;
}

int tn_json_next(const tn_json *j, int tok, int parent)
{
	uint32_t n;
	if (j->t[parent].type == TN_J_OBJ)
		n = j->t[tok + 1].next; /* skip key and its value */
	else
		n = j->t[tok].next;
	return n < j->t[parent].next ? (int)n : -1;
}

int tn_json_count(const tn_json *j, int tok)
{
	int n = 0;
	for (int i = tn_json_first(j, tok); i >= 0; i = tn_json_next(j, i, tok))
		n++;
	return n;
}

int tn_json_get(const tn_json *j, int obj, const char *key)
{
	if (obj < 0 || j->t[obj].type != TN_J_OBJ)
		return -1;
	size_t kl = strlen(key);
	for (int i = tn_json_first(j, obj); i >= 0; i = tn_json_next(j, i, obj)) {
		const tn_jtok *k = &j->t[i];
		if (k->end - k->start == kl && !memcmp(j->s + k->start, key, kl))
			return i + 1;
	}
	return -1;
}

static size_t put_utf8(char *o, uint32_t cp)
{
	if (cp < 0x80) { o[0] = (char)cp; return 1; }
	if (cp < 0x800) { o[0] = (char)(0xc0 | cp >> 6); o[1] = (char)(0x80 | (cp & 0x3f)); return 2; }
	if (cp < 0x10000) {
		o[0] = (char)(0xe0 | cp >> 12); o[1] = (char)(0x80 | ((cp >> 6) & 0x3f));
		o[2] = (char)(0x80 | (cp & 0x3f)); return 3;
	}
	o[0] = (char)(0xf0 | cp >> 18); o[1] = (char)(0x80 | ((cp >> 12) & 0x3f));
	o[2] = (char)(0x80 | ((cp >> 6) & 0x3f)); o[3] = (char)(0x80 | (cp & 0x3f));
	return 4;
}

int tn_json_str(const tn_json *j, int tok, char *buf, size_t cap)
{
	if (tok < 0 || j->t[tok].type != TN_J_STR)
		return -1;
	const char *s = j->s;
	size_t o = 0;
	for (uint32_t i = j->t[tok].start; i < j->t[tok].end; i++) {
		char tmp[4];
		size_t n = 1;
		tmp[0] = s[i];
		if (s[i] == '\\') {
			char e = s[++i];
			switch (e) {
			case 'b': tmp[0] = '\b'; break;
			case 'f': tmp[0] = '\f'; break;
			case 'n': tmp[0] = '\n'; break;
			case 'r': tmp[0] = '\r'; break;
			case 't': tmp[0] = '\t'; break;
			case 'u': {
				uint32_t cp = 0;
				for (int k = 1; k <= 4; k++)
					cp = cp << 4 | (uint32_t)hexv(s[i + k]);
				i += 4;
				if (cp >= 0xd800 && cp < 0xdc00 && i + 6 < j->t[tok].end && s[i + 1] == '\\' &&
				    s[i + 2] == 'u') {
					uint32_t lo = 0;
					for (int k = 3; k <= 6; k++)
						lo = lo << 4 | (uint32_t)hexv(s[i + k]);
					if (lo >= 0xdc00 && lo < 0xe000) {
						cp = 0x10000 + ((cp - 0xd800) << 10) + (lo - 0xdc00);
						i += 6;
					}
				}
				n = put_utf8(tmp, cp);
				break;
			}
			default: tmp[0] = e; break;
			}
		}
		for (size_t k = 0; k < n; k++, o++)
			if (cap && o < cap - 1)
				buf[o] = tmp[k];
	}
	if (cap)
		buf[o < cap - 1 ? o : cap - 1] = 0;
	return (int)o;
}

int tn_json_streq(const tn_json *j, int tok, const char *lit)
{
	if (tok < 0 || j->t[tok].type != TN_J_STR)
		return 0;
	size_t n = strlen(lit);
	return j->t[tok].end - j->t[tok].start == n && !memcmp(j->s + j->t[tok].start, lit, n);
}

int tn_json_i64(const tn_json *j, int tok, int64_t *v)
{
	if (tok < 0 || j->t[tok].type != TN_J_NUM)
		return -1;
	const char *s = j->s + j->t[tok].start;
	size_t n = j->t[tok].end - j->t[tok].start;
	int neg = 0;
	uint64_t x = 0;
	size_t i = 0;
	if (n && s[0] == '-') {
		neg = 1;
		i = 1;
	}
	if (i == n)
		return -1;
	for (; i < n; i++) {
		if (s[i] < '0' || s[i] > '9')
			return -1;
		if (x > (UINT64_C(1) << 62))
			return -1;
		x = x * 10 + (uint64_t)(s[i] - '0');
	}
	*v = neg ? -(int64_t)x : (int64_t)x;
	return 0;
}

int tn_json_bool(const tn_json *j, int tok)
{
	if (tok < 0)
		return -1;
	if (j->t[tok].type == TN_J_TRUE)
		return 1;
	if (j->t[tok].type == TN_J_FALSE)
		return 0;
	return -1;
}

/* ---- writer ---- */

void tn_jw_init(tn_jw *w, char *buf, size_t cap)
{
	w->buf = buf;
	w->cap = cap;
	w->len = 0;
	w->err = cap == 0;
	if (cap)
		buf[0] = 0;
}

static void jw_put(tn_jw *w, const char *s, size_t n)
{
	if (w->err)
		return;
	if (w->len + n + 1 > w->cap) {
		w->err = 1;
		return;
	}
	memcpy(w->buf + w->len, s, n);
	w->len += n;
	w->buf[w->len] = 0;
}

void tn_jw_raw(tn_jw *w, const char *s) { jw_put(w, s, strlen(s)); }

void tn_jw_str(tn_jw *w, const char *s)
{
	jw_put(w, "\"", 1);
	for (; *s; s++) {
		unsigned char c = (unsigned char)*s;
		if (c == '"' || c == '\\') {
			char e[2] = {'\\', (char)c};
			jw_put(w, e, 2);
		} else if (c < 0x20) {
			char e[8];
			snprintf(e, sizeof e, "\\u%04x", c);
			jw_put(w, e, 6);
		} else {
			jw_put(w, (const char *)&c, 1);
		}
	}
	jw_put(w, "\"", 1);
}

void tn_jw_obj(tn_jw *w) { jw_put(w, "{", 1); }

void tn_jw_end(tn_jw *w)
{
	if (!w->err && w->len && w->buf[w->len - 1] == ',')
		w->len--;
	jw_put(w, "}", 1);
}

void tn_jw_key(tn_jw *w, const char *k)
{
	tn_jw_str(w, k);
	jw_put(w, ":", 1);
}

void tn_jw_kstr(tn_jw *w, const char *k, const char *v)
{
	tn_jw_key(w, k);
	tn_jw_str(w, v);
	jw_put(w, ",", 1);
}

void tn_jw_kint(tn_jw *w, const char *k, int64_t v)
{
	char b[24];
	snprintf(b, sizeof b, "%lld", (long long)v);
	tn_jw_kraw(w, k, b);
}

void tn_jw_kbool(tn_jw *w, const char *k, int v) { tn_jw_kraw(w, k, v ? "true" : "false"); }

void tn_jw_kraw(tn_jw *w, const char *k, const char *raw)
{
	tn_jw_key(w, k);
	tn_jw_raw(w, raw);
	jw_put(w, ",", 1);
}
