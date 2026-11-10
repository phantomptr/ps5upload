/* Internal declarations shared by the tailnet modules. Not part of the API. */
#ifndef TN_INTERNAL_H
#define TN_INTERNAL_H

#include <stddef.h>
#include <stdint.h>
#include "os.h"

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
	int sys_err;          /* platform errno of the last failed socket call (0 = none, or EOF) */
	uint32_t ip;          /* the address dialed, host order (0 until connected) */
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
/* addr: optional IPv4 literal used when `host` does not resolve (no DNS on the PS5). */
int tn_tr_dial(tn_tr *tr, const char *host, const char *addr, int port, tn_tls *tls, int wake_fd,
	       int64_t deadline);
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
	int32_t retry_after_s; /* Retry-After header, 0 if absent */
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

static inline void tn_put_le32(uint8_t *p, uint32_t v)
{
	p[0] = (uint8_t)v; p[1] = (uint8_t)(v >> 8); p[2] = (uint8_t)(v >> 16); p[3] = (uint8_t)(v >> 24);
}

/* ---------- wg.c: WireGuard ---------- */
#define TN_WG_MSG_INIT 1
#define TN_WG_MSG_RESP 2
#define TN_WG_MSG_COOKIE 3
#define TN_WG_MSG_DATA 4
#define TN_WG_INIT_LEN 148
#define TN_WG_RESP_LEN 92
#define TN_WG_COOKIE_LEN 64
#define TN_WG_REKEY_AFTER_MS 120000
#define TN_WG_REJECT_AFTER_MS 180000
#define TN_WG_REKEY_ATTEMPT_MS 90000
#define TN_WG_REKEY_TIMEOUT_MS 5000
#define TN_WG_KEEPALIVE_MS 10000
#define TN_WG_COOKIE_REFRESH_MS 120000
#define TN_WG_INIT_RATE_MS 20
#define TN_WG_REJECT_AFTER_MSGS (UINT64_MAX - 8192 - 1)
#define TN_WG_REPLAY_BLOCKS 32 /* 31 * 64 = 1984-packet window */

typedef struct {
	uint8_t send_key[32], recv_key[32];
	uint64_t send_n;
	uint64_t replay_last;
	uint64_t replay[TN_WG_REPLAY_BLOCKS];
	uint32_t local_idx, remote_idx;
	int64_t created;
	uint8_t valid, initiator;
} tn_wg_keypair;

typedef struct {
	uint8_t priv[32], pub[32];
	uint8_t mac1_key[32];
	uint8_t last_ts_sent[12];
} tn_wg_self;

typedef struct {
	uint8_t pub[32];
	uint8_t ss[32];            /* static-static DH, precomputed */
	uint8_t mac1_key[32], cookie_key[32];
	/* an initiation we sent */
	uint8_t hs_active;
	uint8_t hs_h[32], hs_ck[32], hs_e_priv[32];
	uint32_t hs_local_idx;
	int64_t hs_started, hs_sent;
	uint8_t last_mac1[16], cookie[16], has_cookie;
	int64_t cookie_at;
	/* what we accepted from the peer */
	uint8_t last_ts[12];
	int64_t last_init_rx;
	tn_wg_keypair cur, prev, next;
	int64_t keepalive_due;     /* passive keepalive deadline, 0 = none */
} tn_wg_peer;

typedef struct {
	uint8_t h[32], ck[32], e_pub[32], peer_pub[32];
	uint32_t sender;
} tn_wg_rx_init;

void tn_wg_self_init(tn_wg_self *s, const uint8_t priv[32], const uint8_t pub[32]);
int tn_wg_peer_init(const tn_wg_self *s, tn_wg_peer *p, const uint8_t peer_pub[32]);
int tn_wg_check_mac1(const tn_wg_self *s, const uint8_t *msg, size_t len);
void tn_wg_create_init(tn_wg_self *s, tn_wg_peer *p, uint8_t out[TN_WG_INIT_LEN], uint32_t local_idx,
		       int64_t wall_ns, int64_t now_ms);
/* Responder, step 1: check mac1 and decrypt the initiator's static key. */
int tn_wg_init_open(const tn_wg_self *s, const uint8_t *msg, size_t len, tn_wg_rx_init *st);
/* Step 2, once the peer is known: timestamp + rate checks. 0, -1 bad, -2 replay/flood. */
int tn_wg_init_finish(tn_wg_peer *p, tn_wg_rx_init *st, const uint8_t *msg, int64_t now_ms);
/* Step 3: the response; the keypair lands in p->next. */
void tn_wg_create_resp(tn_wg_peer *p, tn_wg_rx_init *st, uint8_t out[TN_WG_RESP_LEN], uint32_t local_idx,
		       int64_t now_ms);
int tn_wg_consume_resp(const tn_wg_self *s, tn_wg_peer *p, const uint8_t *msg, size_t len, int64_t now_ms);
int tn_wg_consume_cookie(tn_wg_peer *p, const uint8_t *msg, size_t len, int64_t now_ms);
void tn_wg_confirm_next(tn_wg_peer *p);
/* Transport: out gets 16 + pad16(n) + 16 bytes; pt may equal out + 16. */
size_t tn_wg_seal(tn_wg_keypair *kp, uint8_t *out, const uint8_t *pt, size_t n);
/* Decrypt in place (plaintext at msg + 16). Returns its length, -1 bad, -2 replay. */
int tn_wg_open(tn_wg_keypair *kp, uint8_t *msg, size_t len, int64_t now_ms);
tn_wg_keypair *tn_wg_send_keypair(tn_wg_peer *p, int64_t now_ms);
void tn_wg_reset(tn_wg_peer *p);

/* ---------- disco.c: disco + STUN ---------- */
#define TN_DISCO_HDR 62 /* magic 6 + sender key 32 + nonce 24 */
#define TN_DISCO_PING 1
#define TN_DISCO_PONG 2
#define TN_DISCO_CMM 3
#define TN_STUN_REQ_LEN 40
extern const uint8_t tn_disco_magic[6];
int tn_disco_is(const uint8_t *p, size_t n);
size_t tn_disco_seal(uint8_t *out, const uint8_t my_pub[32], const uint8_t shared[32], const uint8_t *pt,
		     size_t n);
int tn_disco_open(uint8_t *pt, const uint8_t *pkt, size_t n, const uint8_t shared[32]);
void tn_disco_put_ep(uint8_t out[18], tn_ep4 ep);
int tn_disco_get_ep(const uint8_t in[18], tn_ep4 *ep);
uint32_t tn_crc32(const uint8_t *p, size_t n);
size_t tn_stun_request(uint8_t out[TN_STUN_REQ_LEN], const uint8_t txid[12]);
int tn_stun_is(const uint8_t *p, size_t n);
int tn_stun_parse(const uint8_t *p, size_t n, uint8_t txid[12], tn_ep4 *ep);

/* ---------- ip.c ---------- */
#define TN_IPPROTO_ICMP 1
#define TN_IPPROTO_TCP 6
#define TN_IPPROTO_UDP 17
typedef struct {
	uint8_t proto, tcp_flags, icmp_type;
	uint32_t src, dst;
	uint16_t sport, dport;   /* ICMP echo: id, seq */
	size_t hlen, len;
	const uint8_t *payload;
	size_t plen;
} tn_ip_info;
uint16_t tn_ip_csum(const uint8_t *p, size_t n);
uint16_t tn_l4_csum(uint32_t src, uint32_t dst, uint8_t proto, const uint8_t *seg, size_t n);
int tn_ip_parse(const uint8_t *p, size_t n, tn_ip_info *o);
size_t tn_ip_build_udp(uint8_t *out, size_t cap, tn_ep4 src, tn_ep4 dst, const uint8_t *data, size_t n);
/* The echo reply to an inbound echo request `ip`, built in `out` (which may hold the request):
 * its length, at most TN_MTU, or 0 when the request is too big to answer (dropped). */
size_t tn_dp_echo_reply(uint8_t *out, const tn_ip_info *ip);
size_t tn_ip_build_icmp_echo(uint8_t *out, size_t cap, uint32_t src, uint32_t dst, uint8_t type, uint16_t id,
			     uint16_t seq, const uint8_t *data, size_t n);

/* ---------- derp.c ---------- */
#define TN_DERP_SERVER_KEY 0x01
#define TN_DERP_CLIENT_INFO 0x02
#define TN_DERP_SERVER_INFO 0x03
#define TN_DERP_SEND 0x04
#define TN_DERP_RECV 0x05
#define TN_DERP_KEEPALIVE 0x06
#define TN_DERP_NOTE_PREFERRED 0x07
#define TN_DERP_PEER_GONE 0x08
#define TN_DERP_PING 0x12
#define TN_DERP_PONG 0x13
#define TN_DERP_HEALTH 0x14
#define TN_DERP_RESTARTING 0x15
#define TN_DERP_PKT_MAX 2048
enum { TN_DC_IDLE = 0, TN_DC_CONNECTING, TN_DC_TLS, TN_DC_HTTP, TN_DC_SERVERKEY, TN_DC_SERVERINFO, TN_DC_READY };

typedef struct {
	const br_x509_class *vtable;
	br_x509_decoder_context dc;
	int idx;
} tn_x509_any;

typedef struct {
	uint8_t node_priv[32], node_pub[32];
} tn_derp_keys;

typedef struct tn_derpc tn_derpc;
typedef void (*tn_derp_frame_cb)(void *u, tn_derpc *d, uint8_t type, const uint8_t *p, size_t n);
struct tn_derpc {
	int fd;
	uint8_t st, home;
	int32_t region;
	uint32_t ip;
	uint16_t port;
	char host[TN_HOST_MAX];
	int64_t t_start, last_rx, last_use, retry_at;
	int32_t backoff_ms;
	int sys_err;           /* platform errno of the last failed socket call (0 = none, or EOF) */
	const tn_derp_keys *keys;
	uint8_t server_key[32];
	br_ssl_client_context sc;
	br_x509_minimal_context xc;
	tn_x509_any xa;
	unsigned char iobuf[BR_SSL_BUFSIZE_BIDI];
	uint8_t in[4096];
	size_t in_len;
	uint32_t skip;
	uint8_t out[65536];    /* a burst of a 10 Mbit/s stream while TLS catches up */
	size_t out_len;
};

void tn_tls_client_setup(br_ssl_client_context *cc, br_x509_minimal_context *xc,
			 const br_x509_trust_anchor *tas, size_t n_tas);
int tn_derpc_open(tn_derpc *d, const tn_derp_node *node, int32_t region, const tn_derp_keys *keys,
		  const br_x509_trust_anchor *tas, size_t n_tas, int64_t now);
void tn_derpc_close(tn_derpc *d);
short tn_derpc_events(const tn_derpc *d);
int tn_derpc_io(tn_derpc *d, short revents, tn_derp_frame_cb cb, void *u, int64_t now);
int tn_derpc_send(tn_derpc *d, const uint8_t dst[32], const uint8_t *pkt, size_t n);
int tn_derpc_note_preferred(tn_derpc *d, int preferred);

/* ---------- dp.c: the data plane thread ---------- */
#define TN_REPORT_EPS 8
#define TN_REPORT_LAT 32
typedef struct {
	uint32_t n_ep;
	tn_ep4 ep[TN_REPORT_EPS];
	int32_t preferred_derp;
	uint32_t n_lat;
	struct { int32_t region, us; } lat[TN_REPORT_LAT];
} tn_report;

typedef struct tn_dp tn_dp;
/* Send an IPv4 packet to the peer owning its destination; dp_mu held (data-plane thread). */
int tn_dp_output(tn_ctx *c, const uint8_t *pkt, size_t n);
int tn_dp_create(tn_ctx *c);
void tn_dp_destroy(tn_ctx *c);
int tn_dp_start(tn_ctx *c);
void tn_dp_stop(tn_ctx *c);
void tn_dp_wake(tn_ctx *c);
void tn_dp_rewake(tn_ctx *c, tn_dp *d);

/* ---------- fwd.c: the forwarder (serve rules), on the data-plane thread ---------- */
typedef struct tn_fwd tn_fwd;
struct pollfd;
int tn_fwd_create(tn_ctx *c);
void tn_fwd_destroy(tn_ctx *c);
void tn_fwd_reset(tn_ctx *c);
void tn_fwd_set_addr(tn_ctx *c, uint32_t addr);
/* An inbound packet for a served port (already filtered). 1 = consumed. */
int tn_fwd_input(tn_ctx *c, const uint8_t *pkt, const tn_ip_info *ip, int64_t now);
int tn_fwd_has_udp_flow(tn_ctx *c, uint32_t rip, uint16_t rport, uint16_t port);
int tn_fwd_pollfds(tn_ctx *c, struct pollfd *pf, int cap);
void tn_fwd_io(tn_ctx *c, int fd, short revents, int64_t now);
/* A spliced TCP connection's local socket after poll: 1 when it is broken for good (error, invalid,
 * or hung up with data still queued for it that can never be delivered). poll reports these with no
 * events asked for and keeps reporting them, so leaving the connection as it is would spin the data
 * plane (a console going to rest breaks local sockets this way). A hang-up with nothing queued is
 * an ordinary close: the read sees EOF. */
static inline int tn_fwd_local_broken(short revents, int have_rxq)
{
	if (revents & (POLLERR | POLLNVAL))
		return 1;
	return (revents & POLLHUP) && !(revents & (POLLIN | POLLOUT)) && have_rxq;
}
/* Map poll (control.c): keep waiting after a poll that saw nothing on the connection? Never once
 * the watchdog deadline has passed: the read below then reports the timeout instead of the loop
 * polling with a zero timeout forever. */
static inline int tn_map_poll_keep_waiting(int pr, short conn_revents, int64_t left_ms)
{
	return !conn_revents && pr >= 0 && left_ms > 0;
}
void tn_fwd_tick(tn_ctx *c, int64_t now);
int tn_fwd_sleep_ms(tn_ctx *c);
int tn_fwd_map(tn_ctx *c, uint8_t proto, uint32_t rip, uint16_t rport, uint16_t want, uint16_t *lport);
void tn_fwd_unmap(tn_ctx *c, uint8_t proto, uint16_t lport);
int tn_fwd_expects(tn_ctx *c, const tn_ip_info *ip);
void tn_fwd_stats(tn_ctx *c, uint32_t *udp_flows, uint32_t *tcp_conns);

/* Which peers may open flows to served ports (tn_set_peer_allow). */
typedef struct {
	int all;                 /* 1: no list (the tailnet's filter decides alone) */
	uint32_t n;
	uint32_t ip[TN_ALLOW_MAX];
} tn_peer_allow;
/* ip.c: is `ip` allowed by `a`? */
int tn_peer_allowed(const tn_peer_allow *a, uint32_t ip);

/* The peers the list turned away (tn_peer_denied); under dp_mu. */
typedef struct {
	uint32_t n;
	tn_denied e[TN_DENIED_MAX];
	uint64_t seq[TN_DENIED_MAX]; /* when each entry was last noted: orders entries in one second */
	uint64_t next;
} tn_denied_log;
/* ip.c: note one refused attempt at `now_s`: a known source counts and moves its time, a new
 * one takes a free slot or the oldest one's. */
void tn_denied_note(tn_denied_log *log, uint32_t ip, uint8_t proto, uint16_t port, int64_t now_s);
/* ip.c: forget the entries `a` now lets in. */
void tn_denied_prune(tn_denied_log *log, const tn_peer_allow *a);
/* ip.c: up to max entries into out, most recent first; how many. */
int tn_denied_copy(const tn_denied_log *log, tn_denied *out, int max);

/* ---------- network-down self-suspend (tailnet.c) ----------
 *
 * A socket call that fails the way calls fail while the network goes away (tn_errno_net_down; the
 * PS5's errno 163 for the ~25 s before it rests) makes the node close EVERY socket it holds (the
 * UDP port, every DERP connection, the control connection, the forwarder's flows and connections)
 * and open none until it is resumed: by tn_resume, or (TN_NETPOL_SELF_RESUME, the default) by
 * itself once the network has been healthy for a while. The thread that sees the error only sets
 * an atomic and wakes the two threads; each closes its own sockets at once. */
enum {
	TN_NK_UDP_SEND = 1, /* the WireGuard/disco port, sending */
	TN_NK_UDP_RECV,     /* the same port, receiving */
	TN_NK_STUN,         /* netcheck's STUN requests (same port) */
	TN_NK_DERP,         /* a DERP connection (connect, TLS, frames) */
	TN_NK_CONTROL,      /* the control connection */
	TN_NK_FWD,          /* the forwarder's local sockets */
	TN_NK_POLL,         /* the data plane's poll() itself */
	TN_NK_CONTROL_STREAK, /* control connections failing back to back (TN_NETPOL_CONTROL_STREAK) */
	TN_NK_TEST
};
const char *tn_nk_name(int kind);

#define TN_RESUME_BASE_MS 30000   /* healthy network before a first self-resume */
#define TN_RESUME_MAX_MS 300000   /* ... doubled per suspend in a row, capped */
#define TN_FLAP_WINDOW_MS 600000  /* suspends closer than this count as "in a row" */
#define TN_PROBE_EVERY_MS 2000    /* health probe while suspended */
#define TN_STREAK_WINDOW_MS 10000 /* control errors this close together are a streak */
#define TN_STREAK_ERRS 2

typedef struct {
	int64_t down_at;       /* the last suspend, ms (monotonic) */
	int64_t healthy_since; /* the first healthy probe of the current run, 0 = none */
	uint32_t recent;       /* suspends in a row (each within TN_FLAP_WINDOW_MS of the one before) */
	uint32_t total;
	int64_t base_ms;       /* healthy time a first suspend needs */
} tn_susp;

static inline void tn_susp_init(tn_susp *s, int64_t base_ms)
{
	s->down_at = s->healthy_since = 0;
	s->recent = s->total = 0;
	s->base_ms = base_ms > 0 ? base_ms : TN_RESUME_BASE_MS;
}

static inline void tn_susp_on_down(tn_susp *s, int64_t now)
{
	s->recent = s->total && now - s->down_at < TN_FLAP_WINDOW_MS ? s->recent + 1 : 1;
	s->total++;
	s->down_at = now;
	s->healthy_since = 0;
}

/* How long the network must stay healthy before resuming: doubles with each suspend in a row (no
 * flapping between a network that comes and goes and a reconnect storm), capped. */
static inline int64_t tn_susp_need_ms(const tn_susp *s)
{
	uint32_t sh = s->recent > 1 ? s->recent - 1 : 0;
	if (sh > 8)
		sh = 8;
	int64_t need = s->base_ms << sh;
	int64_t cap = s->base_ms > TN_RESUME_MAX_MS ? s->base_ms : TN_RESUME_MAX_MS;
	return need > cap ? cap : need;
}

/* One health probe while suspended. 1 = resume now. */
static inline int tn_susp_on_probe(tn_susp *s, int healthy, int64_t now)
{
	if (!healthy) {
		s->healthy_since = 0;
		return 0;
	}
	if (!s->healthy_since)
		s->healthy_since = now;
	return now - s->healthy_since >= tn_susp_need_ms(s);
}

/* Control connections failing back to back (TN_NETPOL_CONTROL_STREAK). With the backoff doubling
 * from 1 s, two connection errors within 10 s only happen right after it was reset (a session that
 * ran, a resume): what the PS5 showed in the seconds before it froze for rest. */
typedef struct {
	int64_t first;
	uint32_t n;
} tn_ctl_streak;

/* A control session ended with a connection-class error (conn_err) or another one. 1 = a streak. */
static inline int tn_ctl_streak_note(tn_ctl_streak *k, int conn_err, int64_t now)
{
	if (!conn_err) {
		k->n = 0;
		return 0;
	}
	if (k->n && now - k->first > TN_STREAK_WINDOW_MS)
		k->n = 0;
	if (!k->n)
		k->first = now;
	if (++k->n < TN_STREAK_ERRS)
		return 0;
	k->n = 0;
	return 1;
}

/* Is a control session's result a connection error (not a refusal: a refusal proves the path)? */
int tn_ctl_conn_err(int r, int raw_errno);

/* ---------- the context (tailnet.c, control.c) ---------- */
struct tn_ctx {
	tn_config cfg;
	char state_dir[512], control_url[256], auth_key[256], hostname[64], os[32], app[32];
	char ca_file[512], dump_dir[512], control_addr[64];
	tn_url url;
	tn_keys keys;

	tn_thread thread;
	int thread_started;
	int wake_r, wake_w;
	tn_atomic req_stop, req_pause, req_reauth, req_rewake;
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
	size_t footprint;
	int64_t retry_after_ms;  /* from a 429/503 Retry-After, used once by the backoff */
	int32_t expiry_bucket;   /* last key-expiry warning sent (hours left / 24), -1 none */

	/* control <-> data plane. Lock order: dp_mu, then mu. */
	tn_mutex mu;             /* nms (netmap) and report */
	tn_atomic nm_gen;        /* bumped by control after each applied netmap change */
	tn_report report;        /* endpoints + home DERP from the data plane */
	tn_atomic report_dirty, ctl_reconnect;
	int kick_r, kick_w;      /* data plane -> control thread */

	tn_mutex dp_mu;          /* recursive: all data-plane state */
	tn_thread dp_thread;
	int dp_started;
	int dp_wake_r, dp_wake_w;
	tn_dp *dp;
	tn_serve serve[TN_SERVE_MAX]; /* copied from the config */
	uint32_t n_serve;
	tn_peer_allow allow;     /* tn_set_peer_allow; under dp_mu */
	tn_denied_log denied;    /* what `allow` turned away; under dp_mu */
	tn_fwd *fwd;

	/* network-down self-suspend */
	tn_atomic net_down;      /* 1: every socket closed, none opened until resumed */
	tn_atomic down_kind, down_errno, down_seq; /* the signal that did it (TN_NK_*, platform errno) */
	tn_atomic pol_self_resume, pol_streak;     /* tn_set_net_policy */
	tn_atomic route_ip;      /* the control address last dialed: the health probe's route check */
	tn_atomic resume_base_ms;/* tests: TN_RESUME_BASE_MS */
	int64_t probe_ms;        /* last confirming probe (under mu): at most one a second */
	int probe_res, probe_valid;
	tn_susp susp;            /* control thread */
	tn_ctl_streak streak;    /* control thread */
	long susp_seen;          /* control thread: the down_seq it has accounted for */
};

/* A socket call of `kind` failed with platform errno `raw`. In the "network is going away" class,
 * and (for anything but the PS5's errno 163) confirmed by a probe that the network really is going,
 * the node suspends. Any thread, any lock; never blocks. 1 when the node is (now) down. */
int tn_net_check(tn_ctx *c, int kind, int raw);
/* Suspend now (no confirmation). Any thread, any lock, never blocks. */
void tn_net_down_signal(tn_ctx *c, int kind, int raw);

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
/* Start a renewal: rotate the node key (keeping the old one as OldNodeKey) and forget a
 * pending login URL; the next session registers again. Control thread only. */
void tn_control_reauth(tn_ctx *c);
void tn_control_close(tn_ctx *c);

#endif
