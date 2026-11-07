/* Internal declarations shared by the tailnet modules. Not part of the API. */
#ifndef TN_INTERNAL_H
#define TN_INTERNAL_H

#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>
#include <pthread.h>

#include "bearssl.h"
#include "tailnet.h"
#include "json.h"

/* ---------- util.c ---------- */
int64_t tn_now_ms(void);      /* monotonic */
int64_t tn_wall_s(void);      /* unix seconds */
void tn_hex(char *out, const uint8_t *in, size_t n); /* writes 2n chars + NUL */
int tn_unhex(uint8_t *out, size_t n, const char *in, size_t inlen);
size_t tn_b64(char *out, size_t cap, const uint8_t *in, size_t n); /* std, padded */
int tn_parse_ip4(const char *s, size_t n, uint32_t *ip);
int tn_parse_prefix4(const char *s, size_t n, tn_prefix4 *p); /* "a.b.c.d/n" or bare */
int tn_parse_ep4(const char *s, size_t n, tn_ep4 *ep);        /* "a.b.c.d:port" */
int64_t tn_parse_time(const char *s, size_t n);               /* RFC 3339 -> unix s; 0 for year 1 */
size_t tn_strlcpy(char *dst, const char *src, size_t cap);
static inline uint16_t tn_be16(const uint8_t *p) { return (uint16_t)(p[0] << 8 | p[1]); }
static inline uint32_t tn_be32(const uint8_t *p)
{
	return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 | p[3];
}
static inline uint32_t tn_le32(const uint8_t *p)
{
	return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}
static inline void tn_put_be16(uint8_t *p, uint16_t v) { p[0] = (uint8_t)(v >> 8); p[1] = (uint8_t)v; }
static inline void tn_put_be32(uint8_t *p, uint32_t v)
{
	p[0] = (uint8_t)(v >> 24); p[1] = (uint8_t)(v >> 16); p[2] = (uint8_t)(v >> 8); p[3] = (uint8_t)v;
}

/* ---------- keys.c ---------- */
typedef struct {
	uint8_t machine_priv[32], machine_pub[32];
	uint8_t node_priv[32], node_pub[32];
	uint8_t old_node_pub[32];
	uint8_t has_old_node;
	uint8_t disco_priv[32], disco_pub[32]; /* per process, never persisted */
	uint8_t control_key[32];
	uint8_t has_control_key;
	uint8_t logged_in;
	char control_url[256];
	int64_t node_id;
} tn_keys;

void tn_key_new(uint8_t priv[32], uint8_t pub[32]); /* clamped X25519 */
/* "prefix:<64 hex>" */
void tn_key_text(char out[80], const char *prefix, const uint8_t k[32]);
int tn_key_parse(uint8_t k[32], const char *prefix, const char *s, size_t n);
/* Load (or create) state_dir/tailnet-state.json; control_url changes reset the login. */
int tn_state_load(tn_keys *k, const char *dir, const char *control_url);
int tn_state_save(const tn_keys *k, const char *dir);

/* ---------- net.c: TCP (+TLS) transport with a wake fd ---------- */
typedef struct {
	br_ssl_client_context sc;
	br_x509_minimal_context xc;
	unsigned char iobuf[BR_SSL_BUFSIZE_BIDI];
	br_x509_trust_anchor tas[16];
	size_t n_tas;
	unsigned char ta_mem[8192]; /* DNs and keys of user-supplied anchors */
	size_t ta_used;
} tn_tls;

typedef struct {
	int fd;
	int wake_fd;          /* readable = stop/pause requested */
	int64_t deadline;     /* tn_now_ms() deadline for the current operation, 0 = none */
	int err;              /* last TN_E_* from a low-level call */
	tn_tls *tls;          /* NULL for plain TCP */
	br_sslio_context ioc;
	uint8_t pre[4096];    /* bytes read past an HTTP header, served first */
	size_t pre_pos, pre_len;
} tn_tr;

typedef struct {
	int https;
	char host[128];
	int port;            /* explicit port, or 0 */
} tn_url;

int tn_url_parse(tn_url *u, const char *url);
int tn_tls_init(tn_tls *t, const char *ca_file);
int tn_tr_dial(tn_tr *tr, const char *host, int port, tn_tls *tls, int wake_fd, int64_t deadline);
int tn_tr_read(tn_tr *tr, void *buf, size_t n);       /* > 0 bytes, or TN_E_* */
int tn_tr_read_full(tn_tr *tr, void *buf, size_t n);
int tn_tr_write(tn_tr *tr, const void *buf, size_t n); /* all, then flush */
void tn_tr_close(tn_tr *tr);
/* Read an HTTP/1.1 response header into hdr (NUL-terminated); keeps the rest in tr->pre.
 * Returns the status code or TN_E_*. */
int tn_http_read_header(tn_tr *tr, char *hdr, size_t cap);
/* Value of header `name` (case-insensitive) in hdr, copied into out; 0 or -1. */
int tn_http_header(const char *hdr, const char *name, char *out, size_t cap);

/* ---------- noise.c: ts2021 Noise IK + record layer ---------- */
#define TN_NOISE_INIT_LEN 101
#define TN_NOISE_RESP_LEN 51
#define TN_NOISE_MAX_REC 4096
#define TN_NOISE_MAX_PT (TN_NOISE_MAX_REC - 3 - 16)

typedef struct {
	uint8_t h[32], ck[32];
	uint8_t e_priv[32], e_pub[32];
	uint8_t control_pub[32];
} tn_noise_hs;

typedef struct {
	tn_tr *tr;
	uint8_t tx_key[32], rx_key[32];
	uint64_t tx_n, rx_n;
	uint8_t rec[TN_NOISE_MAX_REC];
	uint8_t pt[TN_NOISE_MAX_REC];
	size_t pt_pos, pt_len;
} tn_nconn;

/* e_priv NULL = random ephemeral. */
int tn_noise_initiate(tn_noise_hs *hs, uint8_t out[TN_NOISE_INIT_LEN], uint16_t capver,
		      const uint8_t machine_priv[32], const uint8_t control_pub[32], const uint8_t *e_priv);
/* Consume the 51-byte response; on success fill nc keys. */
int tn_noise_finish(tn_noise_hs *hs, const uint8_t resp[TN_NOISE_RESP_LEN],
		    const uint8_t machine_priv[32], tn_nconn *nc);
int tn_nconn_read(tn_nconn *nc, void *buf, size_t n);  /* > 0 or TN_E_* */
int tn_nconn_read_full(tn_nconn *nc, void *buf, size_t n);
int tn_nconn_write(tn_nconn *nc, const void *buf, size_t n);
/* Seal one record (n <= TN_NOISE_MAX_PT) into out (n + 19 bytes). */
size_t tn_noise_seal_record(tn_nconn *nc, uint8_t *out, const uint8_t *pt, size_t n);

/* ---------- hpack.c ---------- */
#define TN_HPACK_TABLE 4096
#define TN_HPACK_STR_MAX 4096
typedef struct {
	uint8_t data[TN_HPACK_TABLE];    /* strings, oldest first */
	struct { uint16_t off, nlen, vlen; } ent[TN_HPACK_TABLE / 32];
	uint32_t n_ent, used_bytes;      /* used = sum of strings */
	uint32_t size, max_size;         /* RFC size (strings + 32 each) and its limit */
	uint32_t settings_max;           /* our SETTINGS_HEADER_TABLE_SIZE */
	char sname[TN_HPACK_STR_MAX], svalue[TN_HPACK_STR_MAX];
} tn_hpack;

typedef int (*tn_hpack_cb)(void *u, const char *name, size_t nlen, const char *value, size_t vlen);
void tn_hpack_init(tn_hpack *h, uint32_t max_size);
int tn_hpack_decode(tn_hpack *h, const uint8_t *p, size_t n, tn_hpack_cb cb, void *u);
int tn_huff_decode(const uint8_t *p, size_t n, char *out, size_t cap); /* length or < 0 */
/* Literal without indexing, new name, no Huffman. Returns bytes written or -1. */
int tn_hpack_put(uint8_t *out, size_t cap, const char *name, const char *value);

/* ---------- h2.c ---------- */
#define TN_H2_STREAMS 4
#define TN_H2_FRAME_MAX 16384
#define TN_H2_HDR_MAX 16384

typedef struct {
	uint32_t id;           /* 0 = free slot */
	int status;            /* :status, 0 until headers */
	uint8_t ended, reset, overflow;
	uint8_t *body;         /* caller-owned sink */
	size_t cap, len;
	uint32_t unacked;      /* received bytes not yet returned by WINDOW_UPDATE */
} tn_h2_stream;

typedef struct {
	tn_nconn *nc;
	tn_hpack dec;
	uint32_t next_id;
	uint32_t conn_unacked;
	uint32_t peer_max_frame;
	uint8_t goaway;
	uint8_t pend[9];       /* frame header bytes read ahead (early payload check) */
	uint8_t npend;
	tn_h2_stream st[TN_H2_STREAMS];
	uint8_t frame[TN_H2_FRAME_MAX];
	uint8_t hdr[TN_H2_HDR_MAX];  /* HEADERS + CONTINUATION assembly */
	size_t hdr_len;
	uint32_t hdr_stream;
	uint8_t hdr_flags;
	uint8_t out[9 + TN_H2_FRAME_MAX];
} tn_h2;

/* Read the optional early payload, send the preface + SETTINGS + WINDOW_UPDATE. */
int tn_h2_start(tn_h2 *h, tn_nconn *nc);
/* Start a POST; returns the stream slot index or TN_E_*. */
int tn_h2_post(tn_h2 *h, const char *authority, const char *path, const char *body, size_t n,
	       uint8_t *sink, size_t sink_cap);
/* Process one frame; *slot = stream slot touched (or -1). */
int tn_h2_step(tn_h2 *h, int *slot);
void tn_h2_release(tn_h2 *h, int slot);

/* ---------- netmap.c ---------- */
typedef struct {
	tn_netmap nm;
	uint32_t max_peers, max_regions, max_rules;
	char sets[8][16];      /* PacketFilters set names; rule.set indexes this */
	uint32_t n_sets;
} tn_nmstate;

typedef struct {
	uint8_t changed;       /* anything a consumer should see */
	uint8_t keepalive_only;
	uint8_t self_expired;
	char pop_url[512];
} tn_map_result;

/* Apply one MapResponse (JSON already tokenized in j). */
int tn_netmap_apply(tn_nmstate *s, const tn_json *j, tn_map_result *res);
void tn_netmap_reset(tn_nmstate *s);

/* ---------- the context (tailnet.c, control.c) ---------- */
struct tn_ctx {
	tn_config cfg;
	char state_dir[512], control_url[256], auth_key[256], hostname[64], os[32], app[32];
	char ca_file[512], dump_dir[512];
	tn_url url;
	tn_keys keys;

	pthread_t thread;
	int thread_started;
	int wake_r, wake_w;
	atomic_int req_stop, req_pause;
	tn_state state;

	/* control session */
	tn_tls *tls;
	tn_tr tr;
	tn_nconn nc;
	tn_h2 h2;
	char followup[512];    /* AuthURL being waited on, kept across reconnects */
	char last_url[512];
	int64_t backoff_ms;
	uint32_t dumped;

	/* netmap + parsing memory */
	tn_nmstate nms;
	tn_json json;
	uint8_t *mapbuf;       /* map stream body: [le32 len][json]... */
	size_t mapbuf_cap;
	uint8_t smallbuf[16384]; /* register / lite update bodies */
	uint8_t litebuf[2048];
	char reqbuf[8192];     /* request JSON */
	char logbuf[768];
	size_t footprint;
};

void tn_emit(tn_ctx *c, tn_event_type type, int err, const char *text);
void tn_logf(tn_ctx *c, int level, const char *fmt, ...)
#if defined(__GNUC__) || defined(__clang__)
	__attribute__((format(printf, 3, 4)))
#endif
	;
void tn_set_state(tn_ctx *c, tn_state st);

/* control.c */
int tn_fetch_control_key(tn_ctx *c);
int tn_control_session(tn_ctx *c);
void tn_control_close(tn_ctx *c);

#endif
