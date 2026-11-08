/* A small bounded JSON tokenizer (jsmn-style: no allocation, a caller-owned
 * token array) and a bounded JSON writer. Written for tailnet. */
#ifndef TN_JSON_H
#define TN_JSON_H

#include <stddef.h>
#include <stdint.h>

enum { TN_J_OBJ = 1, TN_J_ARR, TN_J_STR, TN_J_NUM, TN_J_TRUE, TN_J_FALSE, TN_J_NULL };

typedef struct {
	uint32_t start, end; /* byte range; strings exclude the quotes */
	uint32_t next;       /* index of the first token after this subtree */
	uint8_t type;
} tn_jtok;

typedef struct {
	const char *s;
	tn_jtok *t;
	uint32_t cap, n;
} tn_json;

#define TN_JSON_MAX_DEPTH 64

/* Tokenize s[0..len). Returns the token count, -1 for malformed input, -2 if the
 * token cap or depth limit is exceeded. Token 0 is the root value. */
int tn_json_parse(tn_json *j, const char *s, size_t len);

/* Object member lookup: the value token index or -1. */
int tn_json_get(const tn_json *j, int obj, const char *key);
/* First child (array element or object key) or -1; next sibling via tn_json_next. */
int tn_json_first(const tn_json *j, int tok);
int tn_json_next(const tn_json *j, int tok, int parent);
int tn_json_count(const tn_json *j, int tok);
static inline int tn_json_type(const tn_json *j, int tok) { return tok < 0 ? 0 : j->t[tok].type; }
/* Unescaped string into buf (NUL-terminated, truncated to cap-1). Returns the full
 * unescaped length, or -1 if tok is not a string. */
int tn_json_str(const tn_json *j, int tok, char *buf, size_t cap);
int tn_json_streq(const tn_json *j, int tok, const char *lit);
/* Integer number (no fraction); 0 on success. */
int tn_json_i64(const tn_json *j, int tok, int64_t *v);
/* 1 true, 0 false, -1 not a bool. */
int tn_json_bool(const tn_json *j, int tok);

/* Writer: appends into a fixed buffer; err is set (and output stops) on overflow. */
typedef struct {
	char *buf;
	size_t cap, len;
	int err;
} tn_jw;

void tn_jw_init(tn_jw *w, char *buf, size_t cap);
void tn_jw_raw(tn_jw *w, const char *s);
void tn_jw_str(tn_jw *w, const char *s);      /* quoted + escaped */
void tn_jw_obj(tn_jw *w);                     /* "{" */
void tn_jw_end(tn_jw *w);                     /* "}" (drops a trailing comma) */
void tn_jw_kstr(tn_jw *w, const char *k, const char *v);
void tn_jw_kint(tn_jw *w, const char *k, int64_t v);
void tn_jw_kbool(tn_jw *w, const char *k, int v);
void tn_jw_kraw(tn_jw *w, const char *k, const char *raw);
void tn_jw_key(tn_jw *w, const char *k);      /* "k": (then write the value, then tn_jw_raw(",")) */

#endif
