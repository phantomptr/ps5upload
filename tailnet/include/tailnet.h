/*
 * tailnet: a small, gaming-only Tailscale node (control, DERP, disco, WireGuard; IPv4).
 *
 * This header is the whole public API. One tn_ctx holds all state; there is no
 * other global mutable state. All memory is allocated once in tn_create(), sized
 * by the caps in tn_config, and freed in tn_destroy().
 *
 * Threading: tn_start() runs the node on one internal thread (the "loop thread"),
 * which blocks in poll() and never spins. The event callback is called on that
 * thread; a tn_netmap pointer passed to it is only valid during the call.
 * tn_stop(), tn_pause() and tn_resume() may be called from any thread.
 *
 * Two internal threads: the control thread (blocking I/O: /key, ts2021 + HTTP/2,
 * register, the map long-poll, lite updates) and the data-plane thread (one poll()
 * loop: the UDP socket for WireGuard/disco/STUN, DERP connections over TLS, timers).
 * Event callbacks may come from either thread, but never concurrently with
 * tn_destroy(); a callback must not call tn_stop()/tn_destroy().
 *
 * The node is a layer-3 endpoint: tn_send_ip() takes an IPv4 packet for a peer, and
 * inbound packets for us arrive at tn_config.packet_cb. ICMP echo requests to our
 * address are answered by the core.
 */
#ifndef TAILNET_H
#define TAILNET_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The capability version we claim to control (spec 3.7). One constant, used in the
 * Noise prologue and header, /key?v=, RegisterRequest and MapRequest. */
#define TN_CAPVER 130

/* Fixed per-record sizes (the counts of peers, regions, rules are tn_config caps). */
#define TN_NAME_MAX 64       /* peer/self DNS name, truncated */
#define TN_HOST_MAX 64       /* DERP host name */
#define TN_PEER_ALLOWED_MAX 8
#define TN_PEER_EP_MAX 12
#define TN_DERP_NODES_MAX 6
#define TN_FILTER_SRC_MAX 16
#define TN_FILTER_DST_MAX 16
#define TN_FILTER_PROTO_MAX 6

/* Error codes (negative). */
enum {
	TN_OK = 0,
	TN_E_IO = -1,       /* socket error or peer closed */
	TN_E_PROTO = -2,    /* malformed or unexpected data from the network */
	TN_E_TIMEOUT = -3,
	TN_E_WAKE = -4,     /* interrupted by stop/pause (internal) */
	TN_E_NOMEM = -5,
	TN_E_CRYPTO = -6,   /* authentication failed */
	TN_E_TLS = -7,
	TN_E_HTTP = -8,     /* unexpected HTTP status */
	TN_E_AUTH = -9,     /* control refused registration */
	TN_E_TOOBIG = -10,  /* a message exceeds a configured cap */
	TN_E_ARG = -11,
	TN_E_STATE = -12,   /* state dir unreadable/unwritable */
	TN_E_DNS = -13,
	TN_E_SIGNED_OUT = -14 /* node deleted or expired at control */
};

typedef enum {
	TN_ST_STOPPED = 0,
	TN_ST_CONNECTING,   /* dialing control / handshaking */
	TN_ST_NEEDS_LOGIN,  /* a login URL was issued; waiting for the user */
	TN_ST_WAIT_APPROVAL,/* registered, but control has not authorized the machine */
	TN_ST_RUNNING,      /* map poll is up */
	TN_ST_PAUSED,       /* tn_pause() (rest mode) or network down */
	TN_ST_BACKOFF,      /* waiting before a retry */
	TN_ST_SIGNED_OUT    /* node expired or deleted; a new login is needed */
} tn_state;

typedef struct { uint32_t addr; uint8_t bits; } tn_prefix4;   /* addr in host order */
typedef struct { uint32_t ip; uint16_t port; } tn_ep4;        /* host order */

typedef struct {
	int64_t id;
	char name[TN_NAME_MAX];
	uint8_t key[32];        /* node key: WireGuard static + DERP identity */
	uint8_t disco[32];
	uint8_t has_disco;
	uint8_t wg_only;        /* IsWireGuardOnly: not usable by us */
	int8_t online;          /* 1, 0, or -1 unknown */
	uint8_t expired;
	uint32_t addr4;         /* the peer's tailnet IPv4 (0 if none) */
	uint8_t n_allowed;
	tn_prefix4 allowed[TN_PEER_ALLOWED_MAX]; /* IPv4 AllowedIPs (Addresses if absent) */
	uint8_t n_ep;
	tn_ep4 ep[TN_PEER_EP_MAX];               /* IPv4 UDP endpoints */
	int32_t home_derp;      /* region id, 0 = none */
	int64_t key_expiry;     /* unix seconds, 0 = never */
} tn_peer;

typedef struct {
	char name[16];
	char host[TN_HOST_MAX];
	char cert_name[TN_HOST_MAX];
	uint32_t ipv4;          /* 0 = resolve host */
	int32_t stun_port;      /* -1 = no STUN */
	int32_t derp_port;
	uint8_t stun_only, insecure_for_tests, can_port80;
} tn_derp_node;

typedef struct {
	int32_t id;
	char code[16];
	char name[48];
	uint8_t avoid, no_measure_no_home;
	uint8_t n_nodes;
	tn_derp_node nodes[TN_DERP_NODES_MAX];
} tn_derp_region;

typedef struct { uint32_t lo, hi; } tn_range4;

typedef struct {
	uint8_t set;            /* index of the named PacketFilters set this rule came from */
	uint8_t n_src, n_dst, n_proto;
	tn_range4 src[TN_FILTER_SRC_MAX];        /* IPv4 ranges; IPv6 and cap: sources skipped */
	struct { tn_range4 ip; uint16_t port_lo, port_hi; } dst[TN_FILTER_DST_MAX];
	uint8_t proto[TN_FILTER_PROTO_MAX];      /* empty = TCP, UDP, ICMP */
} tn_filter_rule;

typedef struct {
	/* self */
	int64_t self_id;
	char self_name[TN_NAME_MAX];
	uint32_t self_addr4;
	int32_t self_home_derp;
	int64_t self_key_expiry;
	uint8_t self_expired;
	/* peers, DERP map, inbound packet filter */
	uint32_t n_peers;
	tn_peer *peers;
	uint32_t n_regions;
	tn_derp_region *regions;
	uint32_t n_rules;
	tn_filter_rule *rules;
	uint8_t filter_known;   /* 0 until control sent a filter */
	int64_t control_time;   /* unix seconds from control, 0 if never sent */
	uint32_t updates;       /* MapResponses applied (keep-alives excluded) */
} tn_netmap;

typedef enum {
	TN_EV_STATE = 1,    /* state changed: ev.state */
	TN_EV_LOGIN_URL,    /* ev.text = URL the user must open */
	TN_EV_NETMAP,       /* ev.netmap updated */
	TN_EV_LOG,          /* ev.text = diagnostic line */
	TN_EV_ERROR,        /* ev.err + ev.text; the node retries by itself */
	TN_EV_PATH,         /* a peer's path changed (direct/relayed); ev.text describes it */
	TN_EV_WARN,         /* something the user should know, e.g. the key expires soon */
	TN_EV_NET_DOWN,     /* the network is going away: every socket is closed (tn_net_down); ev.err =
			     * the platform errno that said so (0 for a control-error streak), ev.text
			     * names it. Sent once per episode, after the sockets are closed. */
	TN_EV_NET_UP        /* resumed after TN_EV_NET_DOWN (by itself or by tn_resume); ev.text says why */
} tn_event_type;

typedef struct {
	tn_event_type type;
	tn_state state;
	int err;
	const char *text;
	const tn_netmap *netmap;
} tn_event;

typedef void (*tn_event_cb)(void *user, const tn_event *ev);

/* Inbound IPv4 packet addressed to us (already decrypted and checked: version,
 * checksums, our destination, source within the sending peer's AllowedIPs).
 * Called on the data-plane thread; may call tn_send_ip(). */
typedef void (*tn_packet_cb)(void *user, const uint8_t *pkt, size_t len);

/* Serve a tailnet port from a local one (the PS5 helper: Remote Play and the helper's
 * own port). With any serve rule set, the node accepts inbound traffic ONLY for these
 * (protocol, port) pairs, AND only what the tailnet's packet filter from control allows
 * (new TCP connections and new UDP flows; replies pass). UDP is relayed datagram by
 * datagram to 127.0.0.1:local_port; TCP is terminated in lwIP and spliced to a TCP
 * connection to 127.0.0.1:local_port. ICMP echo is answered if the filter allows the
 * source. Everything else is dropped, and packet_cb sees nothing. */
#define TN_SERVE_MAX 8
typedef struct {
	uint8_t proto;          /* 6 = TCP, 17 = UDP */
	uint16_t port;          /* tailnet port */
	uint16_t local_port;    /* 127.0.0.1 port; 0 = the same */
} tn_serve;

/* Optional hook run first on each internal thread ("control", "data"), e.g. to name it. */
typedef void (*tn_thread_cb)(void *user, const char *role);

typedef enum {
	TN_TRANSPORT_AUTO = 0, /* http:// URL: plain; https:// URL: port 80 first, then TLS 443 */
	TN_TRANSPORT_PLAIN,
	TN_TRANSPORT_TLS
} tn_transport;

typedef struct {
	const char *state_dir;   /* required; keys and the control key cache live here */
	const char *control_url; /* e.g. "https://controlplane.tailscale.com" */
	const char *auth_key;    /* optional pre-auth key; NULL for the interactive login URL */
	const char *hostname;    /* Hostinfo.Hostname */
	const char *os;          /* Hostinfo.OS, e.g. "ps5", "macOS" */
	const char *app;         /* Hostinfo.App, default "ps5upload" */
	const char *ca_file;     /* optional extra PEM trust anchors (private control servers) */
	const char *control_addr;/* optional IPv4 literal for the control host when DNS fails (PS5) */
	tn_transport transport;
	int ephemeral;
	int verbose;             /* TN_EV_LOG detail: 0 quiet, 1 info, 2 debug */
	tn_event_cb cb;
	void *user;
	/* Memory caps; 0 = default. */
	uint32_t max_peers;      /* default 256 */
	uint32_t max_regions;    /* default 64 */
	uint32_t max_rules;      /* default 128 */
	uint32_t map_msg_max;    /* largest MapResponse in bytes, default 4 MiB */
	uint32_t json_tokens_max;/* JSON tokens per message, default 65536 (16 bytes each) */
	/* Test hook: dump each raw MapResponse JSON to this directory (NULL = off). */
	const char *dump_dir;
	/* Data plane. */
	uint16_t udp_port;       /* local UDP port for WireGuard/disco/STUN; 0 = 41641 */
	tn_packet_cb packet_cb;  /* inbound packets; NULL = dropped (pings still answered) */
	int no_direct;           /* debug: never use direct UDP paths (DERP only) */
	const tn_serve *serve;   /* inbound allow-list + forwarding; copied by tn_create */
	uint32_t n_serve;        /* 0 = no forwarder, no inbound filter (a client) */
	tn_thread_cb thread_cb;
	size_t thread_stack;     /* bytes for each internal thread; 0 = 256 KiB */
} tn_config;

typedef struct tn_ctx tn_ctx;

/* Largest IPv4 packet in or out (Tailscale's tunnel MTU, spec 10). */
#define TN_MTU 1280


/* Allocate everything; load or create keys in state_dir. */
int tn_create(const tn_config *cfg, tn_ctx **out);
/* Run the node on its own thread. */
int tn_start(tn_ctx *ctx);
/* Stop the loop thread and wait for it. Safe to call twice. */
void tn_stop(tn_ctx *ctx);
/* Rest mode: drop all connections and stay idle until tn_resume(). Keys and the
 * netmap stay in memory. Resume runs the wake sequence (new control session). */
void tn_pause(tn_ctx *ctx);
void tn_resume(tn_ctx *ctx);

/* The network going away (a laptop losing Wi-Fi; a PS5 about to rest, when every socket call fails
 * with errno 163): on the first socket error that says so (ENETDOWN, ENETUNREACH, EHOSTDOWN,
 * EADDRNOTAVAIL, the PS5's 163; anything but 163 confirmed by a probe that sends nothing off the
 * machine), the node closes EVERY socket it holds (UDP, DERP, control, the forwarder's flows and
 * connections; tn_map listeners on 127.0.0.1 stay) and opens none until it is resumed. State reads
 * TN_ST_PAUSED meanwhile; TN_EV_NET_DOWN / TN_EV_NET_UP mark the episode. 1 while down. Any thread. */
int tn_net_down(tn_ctx *ctx);
enum {
	/* Resume by itself once the network has been healthy (a loopback probe and a route lookup to the
	 * control server) for 30 s, doubling with each suspend in a row up to 5 min. Default on. Off: only
	 * tn_resume resumes (the PS5 helper, whose rest/wake tracking decides). */
	TN_NETPOL_SELF_RESUME = 1,
	/* Two control connection errors within 10 s count as the network going away too (the PS5: what
	 * it showed in the seconds before it froze for rest). Default off: on a desktop that would drop
	 * working peer paths over a control-server hiccup. */
	TN_NETPOL_CONTROL_STREAK = 2
};
void tn_set_net_policy(tn_ctx *ctx, int flags);
/* Tests: go down as if a socket call of the data plane had failed with errno `raw`. */
void tn_test_net_down(tn_ctx *ctx, int raw);
/* Tests: the healthy time a first self-resume needs, in ms (0 = the default 30 s). */
void tn_test_resume_ms(tn_ctx *ctx, int ms);
/* Free everything (calls tn_stop first). */
void tn_destroy(tn_ctx *ctx);
/* Renew the sign-in while keeping this node: a new node key that names the old one
 * (OldNodeKey), so control keeps the machine, its address and its name, and asks for a
 * login again (TN_EV_LOGIN_URL). Any thread; the running session is dropped for it. */
int tn_reauth(tn_ctx *ctx);

/* Which tailnet peers may open a connection or a UDP flow to a served port, by their
 * tailnet IPv4 (host order). n < 0: every peer the tailnet's own filter allows (the
 * default); n >= 0: only these (0 = nobody). Up to TN_ALLOW_MAX; replies and flows already
 * open pass. Checked alongside the serve list and the tailnet filter. Any thread. */
#define TN_ALLOW_MAX 64
int tn_set_peer_allow(tn_ctx *ctx, const uint32_t *ips, int n);

/* The peers the list above turned away: a new TCP connection or UDP flow to a served port that
 * the tailnet's own filter allowed but the app's peer list did not. One entry per source address
 * (a repeat counts and moves its time; it never takes a second slot), the oldest replaced when
 * all TN_DENIED_MAX are used. In memory only, nothing logged: the app shows "<device> tried to
 * reach this PS5". Cleared by tn_set_peer_allow for an address it now lets in. */
#define TN_DENIED_MAX 8
typedef struct {
	uint32_t ip;     /* the peer's tailnet IPv4, host order */
	uint8_t proto;   /* 6 TCP, 17 UDP */
	uint16_t port;   /* the served port it asked for */
	uint32_t count;  /* attempts since it was first noted */
	int64_t first_s; /* unix seconds */
	int64_t last_s;  /* unix seconds */
} tn_denied;
/* Copy up to max entries, most recent first. Returns how many. Any thread. */
int tn_peer_denied(tn_ctx *ctx, tn_denied *out, int max);
/* Tests: note a refused attempt as the data plane would. Any thread. */
void tn_test_peer_denied(tn_ctx *ctx, uint32_t ip, uint8_t proto, uint16_t port);

/* Tests: run the wake sequence a wall-clock jump (rest mode) runs on the data plane,
 * now, as if the machine had just woken. Any thread. */
void tn_test_rewake(tn_ctx *ctx);

/* Map a local port to a peer's tailnet (address, port): the reverse of `serve`, for an
 * app that reaches the console with ordinary sockets. proto 6 (TCP: a listener on
 * 127.0.0.1, each connection dialed through the tunnel) or 17 (UDP: a socket on
 * 127.0.0.1; replies go back to whoever sent last). want_port 0 = any free port.
 * Mapping the same (proto, address, port) again returns the existing local port.
 * Any thread. TN_OK and *local_port set, or TN_E_*. */
int tn_map(tn_ctx *ctx, uint8_t proto, uint32_t remote_ip, uint16_t remote_port, uint16_t want_port,
	   uint16_t *local_port);
/* Close one mapping (proto 0: all of them); open connections through it are closed. */
void tn_unmap(tn_ctx *ctx, uint8_t proto, uint16_t local_port);

/* Send one IPv4 packet (len <= TN_MTU) to the peer whose AllowedIPs contain its
 * destination. Any thread. TN_OK when sent or queued behind a handshake;
 * TN_E_ARG no such peer / not IPv4, TN_E_TOOBIG, TN_E_STATE when not running. */
int tn_send_ip(tn_ctx *ctx, const uint8_t *pkt, size_t len);

typedef enum { TN_PATH_NONE = 0, TN_PATH_DERP, TN_PATH_DIRECT } tn_path;

typedef struct {
	char name[TN_NAME_MAX];
	uint32_t addr4;
	uint8_t online;         /* per control */
	uint8_t handshake;      /* a WireGuard session is up */
	tn_path path;
	tn_ep4 direct;          /* when path == DIRECT */
	int32_t derp_region;    /* the region used when relayed */
	int32_t latency_us;     /* disco RTT on the direct path, -1 unknown */
	int64_t last_rx_ms;     /* ms ago, -1 never */
	uint64_t tx_bytes, rx_bytes;
} tn_peer_status;

typedef struct {
	tn_state state;
	uint32_t self_addr4;
	char self_name[TN_NAME_MAX];
	int32_t home_derp;          /* chosen by netcheck, 0 = none yet */
	int32_t home_latency_us;    /* STUN RTT to the home region, -1 unknown */
	uint8_t derp_connected;     /* the home DERP connection is up */
	uint16_t udp_port;
	uint32_t n_endpoints;
	tn_ep4 endpoints[8];        /* what we advertise (local + STUN) */
	int64_t key_expiry;         /* unix seconds, 0 = never */
	uint32_t n_peers;
	uint32_t fwd_udp_flows;     /* forwarder: open UDP relay flows */
	uint32_t fwd_tcp_conns;     /* forwarder: spliced TCP connections */
	char login_url[256];        /* while state == NEEDS_LOGIN */
} tn_status;

/* Snapshot of the node; peers (may be NULL) gets up to cap entries. */
int tn_get_status(tn_ctx *ctx, tn_status *st, tn_peer_status *peers, uint32_t cap);

/* sizeof of the API structs, for FFI layout checks: 0 tn_config, 1 tn_status,
 * 2 tn_peer_status, 3 tn_event, 4 tn_serve; 0 for anything else. */
size_t tn_abi_size(int which);

/* Bytes allocated by tn_create for this config (for budgeting). */
size_t tn_memory_footprint(const tn_ctx *ctx);
const char *tn_strerror(int err);
const char *tn_state_name(tn_state st);
/* "a.b.c.d" for a host-order IPv4 address; buf must hold 16 bytes. */
const char *tn_ip4_str(uint32_t ip, char buf[16]);

#ifdef __cplusplus
}
#endif
#endif
