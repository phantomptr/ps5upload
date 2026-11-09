/* The data plane: one thread, one poll() loop (spec 5-8, 13).
 *
 *   UDP socket (one port): STUN replies, disco, WireGuard
 *   DERP connections: the home region (always) and peers' regions (on demand)
 *   a 250 ms tick for timers: netcheck/re-STUN, WireGuard retransmits and keepalives,
 *   disco heartbeats, DERP liveness, wall-clock jumps (rest mode)
 *
 * Path policy follows Go's magicsock (endpoint.go): once a disco Pong proves a UDP
 * address, send there alone for 6.5 s after each pong (heartbeat every 3 s while
 * active); otherwise send on the last UDP address *and* DERP.
 *
 * All state here is guarded by c->dp_mu (recursive, so packet_cb may call
 * tn_send_ip). The netmap is copied in from control under c->mu. */
#include "tn.h"
#include "crypto.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define DEFAULT_UDP_PORT 41641
#define TICK_MS 250
#define EP_MAX 16
#define DERP_CONNS 4
#define STAGE_MAX 32
#define PROBES_MAX 48
#define PKT_BUF (TN_MTU + 256)

/* Go magicsock timers (spec 7). */
#define HEARTBEAT_MS 3000
#define TRUST_UDP_MS 6500
#define SESSION_ACTIVE_MS 45000
#define UPGRADE_INTERVAL_MS 60000
#define GOOD_ENOUGH_US 5000
#define DISCO_PING_INTERVAL_MS 5000
#define PING_TIMEOUT_MS 5000
#define RESTUN_MS 23000
#define NETCHECK_DEADLINE_MS 3000
#define DERP_IDLE_CLOSE_MS 60000
#define DERP_DEAD_MS 130000
#define DERP_CONNECT_TIMEOUT_MS 10000
#define WALL_CHECK_MS 5000
#define WALL_JUMP_MS 20000

enum { EPSRC_NETMAP = 1, EPSRC_CMM = 2, EPSRC_LEARNED = 4 };

typedef struct {
	tn_ep4 ep;
	uint8_t src;
	uint8_t txid[12];
	int64_t ping_sent, last_ping, last_pong;
	int64_t ping_us; /* microsecond send time for the RTT */
	int32_t lat_us;
} dp_ep;

typedef struct {
	uint8_t used, present, wg_ok, has_disco;
	uint8_t key[32], disco[32], disco_shared[32];
	char name[TN_NAME_MAX];
	uint32_t addr4;
	uint8_t n_allowed;
	tn_prefix4 allowed[TN_PEER_ALLOWED_MAX];
	int8_t online;
	int32_t home_derp, heard_region;
	uint8_t n_ep;
	dp_ep ep[EP_MAX];
	int best;
	int32_t best_lat_us;
	int64_t trust_until, last_full_ping, last_cmm, last_active, last_heartbeat, last_rx;
	tn_wg_peer wg;
	uint64_t tx_bytes, rx_bytes;
	tn_path reported;
	tn_ep4 reported_ep;
} dp_peer;

typedef struct {
	int32_t region;
	uint32_t ip;
	uint16_t port;
	uint8_t txid[12];
	int64_t first_sent, sent, sent_us;
	int32_t rtt_us;
	tn_ep4 mapped;
	uint8_t tries, done;
} dp_probe;

typedef struct {
	uint8_t derp;
	int32_t region;
	tn_ep4 ep;
} dp_src;

struct tn_dp {
	int udp;
	uint16_t port;
	tn_wg_self wg;
	tn_derp_keys dkeys;
	uint8_t disco_priv[32], disco_pub[32];
	dp_peer *peers;
	uint32_t n_peers, cap;
	int last_peer;             /* cache for the receive path */
	tn_derp_region *regions;
	uint32_t n_regions, cap_regions;
	uint32_t self_addr4;
	char self_name[TN_NAME_MAX];
	tn_derpc conns[DERP_CONNS];
	int32_t home, home_lat_us;
	/* netcheck */
	dp_probe probes[PROBES_MAX];
	uint32_t n_probes;
	uint8_t nc_running;
	int64_t nc_start, nc_next;
	tn_ep4 eps[TN_REPORT_EPS];
	uint32_t n_eps;
	int64_t eps_at;
	/* packets waiting for a handshake */
	struct { int peer; uint16_t len; uint8_t data[TN_MTU]; } stage[STAGE_MAX];
	uint8_t buf[PKT_BUF], buf2[PKT_BUF], sbuf[PKT_BUF]; /* receive, seal, staged send */
	/* the packet filter from control (inbound, spec 9) */
	tn_filter_rule *rules;
	uint32_t n_rules, cap_rules;
	uint8_t filter_known;
	int paused;
	int64_t udp_retry_at, last_wall_check, wall_off, last_tick;
	int64_t last_poll_fail_log; /* rate-limits the "poll failed" line */
	uint32_t nm_gen;
	int64_t now;
};

static int64_t wall_ms(void) { return tn_wall_ns() / 1000000; }
static int64_t mono_us(void) { return tn_mono_us(); }
static int64_t wall_ns(void) { return tn_wall_ns(); }

static void kick_control(tn_ctx *c)
{
	tn_wake_signal(c->kick_w);
}

void tn_dp_wake(tn_ctx *c)
{
	if (c->dp_wake_w >= 0)
		tn_wake_signal(c->dp_wake_w);
}

static int ep_eq(tn_ep4 a, tn_ep4 b) { return a.ip == b.ip && a.port == b.port; }

static const char *ep_str(tn_ep4 e, char buf[24])
{
	char ip[16];
	snprintf(buf, 24, "%s:%u", tn_ip4_str(e.ip, ip), e.port);
	return buf;
}

/* ---------------- UDP socket ---------------- */

static void udp_close(tn_dp *d)
{
	tn_sock_close(d->udp);
	d->udp = -1;
}

static int udp_open(tn_ctx *c, tn_dp *d)
{
	udp_close(d);
	int fd = tn_sock_open(SOCK_DGRAM);
	if (fd < 0)
		return TN_E_IO;
	tn_sock_nonblock(fd);
	tn_sock_setint(fd, SOL_SOCKET, SO_RCVBUF, 1 << 20);
	tn_sock_setint(fd, SOL_SOCKET, SO_SNDBUF, 1 << 20);
	uint16_t want = d->port ? d->port : (c->cfg.udp_port ? c->cfg.udp_port : DEFAULT_UDP_PORT);
	if (tn_sock_bind(fd, 0, want) != 0 && tn_sock_bind(fd, 0, 0) != 0) {
		/* (port taken: any port works, only the type-4 endpoint guess suffers) */
		tn_sock_close(fd);
		return TN_E_IO;
	}
	d->port = tn_sock_local_port(fd);
	d->udp = fd;
	tn_logf(c, 1, "udp: bound port %u", d->port);
	return TN_OK;
}

static void udp_send(tn_ctx *c, tn_dp *d, tn_ep4 ep, const uint8_t *p, size_t n)
{
	if (d->udp < 0 || !ep.ip || !ep.port)
		return;
	if (tn_sock_sendto(d->udp, p, n, ep.ip, ep.port) < 0) {
		int e = tn_sock_raw_errno(), k = tn_sock_errno();
		if (k != TN_ERR_OTHER)
			return; /* one unreachable endpoint, or a full buffer: a lost datagram */
		/* Anything else (ENETDOWN, the PS5's errno 163, EBADF): rebuild the socket
		 * later; never busy-retry. */
		tn_logf(c, 1, "udp: send failed (errno %d); rebinding", e);
		udp_close(d);
		d->udp_retry_at = d->now + 1000;
	}
}

/* ---------------- peers ---------------- */

static dp_peer *peer_by_key(tn_dp *d, const uint8_t key[32])
{
	if (d->last_peer >= 0 && d->peers[d->last_peer].used && !memcmp(d->peers[d->last_peer].key, key, 32))
		return &d->peers[d->last_peer];
	for (uint32_t i = 0; i < d->n_peers; i++)
		if (d->peers[i].used && !memcmp(d->peers[i].key, key, 32)) {
			d->last_peer = (int)i;
			return &d->peers[i];
		}
	return NULL;
}

static dp_peer *peer_by_disco(tn_dp *d, const uint8_t disco[32])
{
	for (uint32_t i = 0; i < d->n_peers; i++)
		if (d->peers[i].used && d->peers[i].has_disco && !memcmp(d->peers[i].disco, disco, 32))
			return &d->peers[i];
	return NULL;
}

static int prefix_has(const tn_prefix4 *p, uint32_t ip)
{
	uint32_t mask = p->bits ? 0xffffffffu << (32 - p->bits) : 0;
	return (ip & mask) == (p->addr & mask);
}

static int peer_allows(const dp_peer *p, uint32_t ip)
{
	if (p->addr4 == ip)
		return 1;
	for (uint8_t i = 0; i < p->n_allowed; i++)
		if (prefix_has(&p->allowed[i], ip))
			return 1;
	return 0;
}

static dp_peer *peer_by_ip(tn_dp *d, uint32_t ip)
{
	for (uint32_t i = 0; i < d->n_peers; i++)
		if (d->peers[i].used && d->peers[i].wg_ok && peer_allows(&d->peers[i], ip))
			return &d->peers[i];
	return NULL;
}

static dp_peer *peer_by_index(tn_dp *d, uint32_t idx, tn_wg_keypair **kp)
{
	for (uint32_t i = 0; i < d->n_peers; i++) {
		dp_peer *p = &d->peers[i];
		if (!p->used)
			continue;
		tn_wg_keypair *k[3] = {&p->wg.cur, &p->wg.next, &p->wg.prev};
		for (int j = 0; j < 3; j++)
			if (k[j]->valid && k[j]->local_idx == idx) {
				if (kp)
					*kp = k[j];
				return p;
			}
		if (!kp && p->wg.hs_active && p->wg.hs_local_idx == idx)
			return p;
	}
	return NULL;
}

static uint32_t new_index(tn_dp *d)
{
	for (;;) {
		uint32_t v;
		tn_random(&v, sizeof v);
		if (v && !peer_by_index(d, v, NULL))
			return v;
	}
}

static int ep_add(dp_peer *p, tn_ep4 ep, uint8_t src)
{
	if (!ep.ip || !ep.port)
		return -1;
	for (uint8_t i = 0; i < p->n_ep; i++)
		if (ep_eq(p->ep[i].ep, ep)) {
			p->ep[i].src |= src;
			return i;
		}
	if (p->n_ep >= EP_MAX) {
		/* Full: replace a learned/CMM endpoint that is not the best path. */
		for (uint8_t i = 0; i < p->n_ep; i++)
			if (!(p->ep[i].src & EPSRC_NETMAP) && (int)i != p->best) {
				memset(&p->ep[i], 0, sizeof p->ep[i]);
				p->ep[i].ep = ep;
				p->ep[i].src = src;
				p->ep[i].lat_us = -1;
				return i;
			}
		return -1;
	}
	dp_ep *e = &p->ep[p->n_ep];
	memset(e, 0, sizeof *e);
	e->ep = ep;
	e->src = src;
	e->lat_us = -1;
	return p->n_ep++;
}

/* Drop endpoints with no source left, keeping p->best valid. */
static void ep_compact(dp_peer *p)
{
	uint8_t o = 0;
	int best = -1;
	for (uint8_t i = 0; i < p->n_ep; i++) {
		if (!p->ep[i].src)
			continue;
		if ((int)i == p->best)
			best = o;
		p->ep[o++] = p->ep[i];
	}
	p->n_ep = o;
	p->best = best;
}

static void disco_reset(dp_peer *p)
{
	for (uint8_t i = 0; i < p->n_ep; i++) {
		p->ep[i].src &= EPSRC_NETMAP;
		p->ep[i].last_ping = p->ep[i].ping_sent = p->ep[i].last_pong = 0;
		p->ep[i].lat_us = -1;
	}
	p->best = -1;
	p->best_lat_us = -1;
	p->trust_until = 0;
	p->last_full_ping = p->last_cmm = p->last_heartbeat = 0;
	ep_compact(p);
}

static void stage_drop(tn_dp *d, int peer)
{
	for (int i = 0; i < STAGE_MAX; i++)
		if (d->stage[i].len && (peer < 0 || d->stage[i].peer == peer))
			d->stage[i].len = 0;
}

/* Copy the netmap in (control publishes nm_gen after each change). */
static void sync_netmap(tn_ctx *c, tn_dp *d)
{
	tn_mutex_lock(&c->mu);
	const tn_netmap *nm = &c->nms.nm;
	d->nm_gen = (uint32_t)tn_atomic_load(&c->nm_gen);
	d->self_addr4 = nm->self_addr4;
	tn_strlcpy(d->self_name, nm->self_name, sizeof d->self_name);
	uint32_t nr = nm->n_regions < d->cap_regions ? nm->n_regions : d->cap_regions;
	memcpy(d->regions, nm->regions, nr * sizeof *d->regions);
	d->n_regions = nr;
	nr = nm->n_rules < d->cap_rules ? nm->n_rules : d->cap_rules;
	memcpy(d->rules, nm->rules, nr * sizeof *d->rules);
	d->n_rules = nr;
	d->filter_known = nm->filter_known;
	tn_fwd_set_addr(c, d->self_addr4);
	for (uint32_t i = 0; i < d->n_peers; i++)
		d->peers[i].present = 0;
	for (uint32_t i = 0; i < nm->n_peers; i++) {
		const tn_peer *np = &nm->peers[i];
		if (np->wg_only)
			continue;
		dp_peer *p = peer_by_key(d, np->key);
		if (!p) {
			uint32_t slot = 0;
			while (slot < d->n_peers && d->peers[slot].used)
				slot++;
			if (slot >= d->cap)
				continue;
			if (slot == d->n_peers)
				d->n_peers++;
			p = &d->peers[slot];
			memset(p, 0, sizeof *p);
			p->used = 1;
			memcpy(p->key, np->key, 32);
			p->wg_ok = tn_wg_peer_init(&d->wg, &p->wg, np->key) == 0;
			p->best = -1;
			p->best_lat_us = -1;
		}
		p->present = 1;
		tn_strlcpy(p->name, np->name, sizeof p->name);
		p->addr4 = np->addr4;
		p->n_allowed = np->n_allowed;
		memcpy(p->allowed, np->allowed, sizeof p->allowed);
		p->online = np->online;
		p->home_derp = np->home_derp;
		if (!np->has_disco) {
			p->has_disco = 0;
		} else if (!p->has_disco || memcmp(p->disco, np->disco, 32)) {
			/* New disco key = the peer restarted: forget its paths. */
			memcpy(p->disco, np->disco, 32);
			p->has_disco = tn_box_beforenm(p->disco_shared, np->disco, d->disco_priv) == 0;
			disco_reset(p);
		}
		/* Netmap endpoints: replace the netmap-sourced set. */
		for (uint8_t e = 0; e < p->n_ep; e++)
			p->ep[e].src &= (uint8_t)~EPSRC_NETMAP;
		for (uint8_t e = 0; e < np->n_ep; e++)
			ep_add(p, np->ep[e], EPSRC_NETMAP);
		ep_compact(p);
	}
	for (uint32_t i = 0; i < d->n_peers; i++)
		if (d->peers[i].used && !d->peers[i].present) {
			stage_drop(d, (int)i);
			tn_wipe(&d->peers[i], sizeof d->peers[i]);
		}
	while (d->n_peers && !d->peers[d->n_peers - 1].used)
		d->n_peers--;
	d->last_peer = -1;
	tn_mutex_unlock(&c->mu);
}

/* ---------------- DERP ---------------- */

static const tn_derp_region *region_by_id(tn_dp *d, int32_t id)
{
	for (uint32_t i = 0; i < d->n_regions; i++)
		if (d->regions[i].id == id)
			return &d->regions[i];
	return NULL;
}

static const tn_derp_node *region_node(const tn_derp_region *r)
{
	for (uint8_t i = 0; r && i < r->n_nodes; i++)
		if (!r->nodes[i].stun_only)
			return &r->nodes[i];
	return NULL;
}

static uint32_t node_ip(const tn_derp_node *n)
{
	if (n->ipv4)
		return n->ipv4;
	/* No IPv4 in the map (some Headscale setups): one blocking lookup. */
	struct addrinfo hints, *res = NULL;
	memset(&hints, 0, sizeof hints);
	hints.ai_family = AF_INET;
	uint32_t ip = 0;
	if (getaddrinfo(n->host, NULL, &hints, &res) == 0 && res) {
		ip = ntohl(((struct sockaddr_in *)res->ai_addr)->sin_addr.s_addr);
		freeaddrinfo(res);
	}
	return ip;
}

static tn_derpc *conn_for(tn_dp *d, int32_t region)
{
	for (int i = 0; i < DERP_CONNS; i++)
		if (d->conns[i].region == region && (d->conns[i].fd >= 0 || d->conns[i].retry_at))
			return &d->conns[i];
	return NULL;
}

static void conn_open(tn_ctx *c, tn_dp *d, tn_derpc *k, int32_t region)
{
	const tn_derp_region *r = region_by_id(d, region);
	const tn_derp_node *n = region_node(r);
	k->region = region;
	k->retry_at = 0;
	if (!n) {
		k->retry_at = d->now + 30000;
		return;
	}
	tn_derp_node node = *n;
	node.ipv4 = node_ip(n);
	if (!node.ipv4) {
		tn_logf(c, 0, "derp %d: no address for %s", region, n->host);
		k->retry_at = d->now + 30000;
		return;
	}
	int r2 = tn_derpc_open(k, &node, region, &d->dkeys, c->tls->tas, c->tls->n_tas, d->now);
	char ip[16];
	tn_logf(c, 1, "derp %d: connecting to %s (%s:%d)%s", region, node.host, tn_ip4_str(node.ipv4, ip),
		node.derp_port > 0 ? node.derp_port : 443, node.insecure_for_tests ? " [insecure_for_tests]" : "");
	k->last_use = d->now;
	if (r2 < 0) {
		k->backoff_ms = k->backoff_ms ? k->backoff_ms * 2 : 1000;
		if (k->backoff_ms > 30000)
			k->backoff_ms = 30000;
		k->retry_at = d->now + k->backoff_ms;
	}
}

static void conn_fail(tn_ctx *c, tn_dp *d, tn_derpc *k, int err)
{
	tn_logf(c, 1, "derp %d: closed (%s)", k->region, tn_strerror(err));
	tn_derpc_close(k);
	k->backoff_ms = k->backoff_ms ? k->backoff_ms * 2 : 1000;
	if (k->backoff_ms > 30000)
		k->backoff_ms = 30000;
	k->retry_at = d->now + k->backoff_ms;
}

/* The connection for a region, opening one (evicting the idlest non-home) if needed. */
static tn_derpc *conn_get(tn_ctx *c, tn_dp *d, int32_t region)
{
	tn_derpc *k = conn_for(d, region);
	if (k)
		return k;
	int pick = -1;
	for (int i = 0; i < DERP_CONNS; i++)
		if (d->conns[i].fd < 0 && !d->conns[i].retry_at) {
			pick = i;
			break;
		}
	if (pick < 0)
		for (int i = 0; i < DERP_CONNS; i++)
			if (d->conns[i].region != d->home &&
			    (pick < 0 || d->conns[i].last_use < d->conns[pick].last_use))
				pick = i;
	if (pick < 0)
		return NULL;
	tn_derpc_close(&d->conns[pick]);
	d->conns[pick].backoff_ms = 0;
	conn_open(c, d, &d->conns[pick], region);
	return &d->conns[pick];
}

static int32_t peer_region(const dp_peer *p) { return p->home_derp ? p->home_derp : p->heard_region; }

static void derp_send(tn_ctx *c, tn_dp *d, int32_t region, const uint8_t key[32], const uint8_t *pkt, size_t n)
{
	if (!region)
		return;
	tn_derpc *k = conn_get(c, d, region);
	if (!k)
		return;
	k->last_use = d->now;
	if (k->st == TN_DC_READY && tn_derpc_send(k, key, pkt, n) == TN_E_IO)
		conn_fail(c, d, k, TN_E_IO);
}

/* ---------------- disco ---------------- */

static void initiate(tn_ctx *c, tn_dp *d, dp_peer *p);

static void disco_send(tn_ctx *c, tn_dp *d, dp_peer *p, const dp_src *to, const uint8_t *pt, size_t n)
{
	uint8_t pkt[TN_DISCO_HDR + 16 + 2 + 12 + 32 + EP_MAX * 18];
	if (!p->has_disco || n > sizeof pkt - TN_DISCO_HDR - 16)
		return;
	size_t m = tn_disco_seal(pkt, d->disco_pub, p->disco_shared, pt, n);
	if (to->derp)
		derp_send(c, d, to->region, p->key, pkt, m);
	else
		udp_send(c, d, to->ep, pkt, m);
}

static void send_ping(tn_ctx *c, tn_dp *d, dp_peer *p, dp_ep *e)
{
	uint8_t pt[2 + 12 + 32];
	pt[0] = TN_DISCO_PING;
	pt[1] = 0;
	tn_random(e->txid, 12);
	memcpy(pt + 2, e->txid, 12);
	memcpy(pt + 14, d->dkeys.node_pub, 32);
	e->ping_sent = e->last_ping = d->now;
	e->ping_us = mono_us();
	dp_src to = {.ep = e->ep};
	disco_send(c, d, p, &to, pt, sizeof pt);
}

static void send_cmm(tn_ctx *c, tn_dp *d, dp_peer *p)
{
	uint8_t pt[2 + TN_REPORT_EPS * 18];
	pt[0] = TN_DISCO_CMM;
	pt[1] = 0;
	for (uint32_t i = 0; i < d->n_eps; i++)
		tn_disco_put_ep(pt + 2 + 18 * i, d->eps[i]);
	dp_src to = {.derp = 1, .region = peer_region(p)};
	p->last_cmm = d->now;
	disco_send(c, d, p, &to, pt, 2 + 18 * d->n_eps);
}

/* Ping every candidate endpoint (rate-limited per endpoint), then CallMeMaybe over
 * DERP so the peer pings us back at the same time (endpoint.go:1428-1462). */
static void discover(tn_ctx *c, tn_dp *d, dp_peer *p)
{
	if (!p->has_disco || c->cfg.no_direct || d->udp < 0)
		return;
	int sent = 0;
	for (uint8_t i = 0; i < p->n_ep; i++)
		if (d->now - p->ep[i].last_ping >= DISCO_PING_INTERVAL_MS) {
			send_ping(c, d, p, &p->ep[i]);
			sent = 1;
		}
	p->last_full_ping = d->now;
	if ((sent || !p->n_ep) && d->n_eps && d->now - p->last_cmm >= DISCO_PING_INTERVAL_MS)
		send_cmm(c, d, p);
}

static int trusted(const tn_dp *d, const dp_peer *p) { return p->best >= 0 && d->now < p->trust_until; }

static void handle_disco(tn_ctx *c, tn_dp *d, const uint8_t *pkt, size_t n, const dp_src *from)
{
	uint8_t pt[1500];
	if (n > sizeof pt + TN_DISCO_HDR)
		return;
	dp_peer *p = peer_by_disco(d, pkt + 6);
	if (!p)
		return; /* not a peer in our netmap: drop silently (spec 6.2) */
	int m = tn_disco_open(pt, pkt, n, p->disco_shared);
	if (m < 2)
		return;
	char es[24];
	/* A disco message proves the peer has us in its netmap now. Stock tailscaled
	 * configures WireGuard peers lazily and drops an initiation that arrives before
	 * that; resend a pending one now instead of after RekeyTimeout (5 s). */
	if (p->wg.hs_active && d->now - p->wg.hs_sent >= 200 && !p->wg.cur.valid)
		initiate(c, d, p);
	switch (pt[0]) {
	case TN_DISCO_PING: {
		if (m < 2 + 12 + 32)
			return;
		uint8_t pong[2 + 12 + 18];
		pong[0] = TN_DISCO_PONG;
		pong[1] = 0;
		memcpy(pong + 2, pt + 2, 12);
		tn_ep4 seen = from->ep;
		if (from->derp) /* the DERP "address" 127.3.3.40:<region> (tailcfg.go:3079) */
			seen = (tn_ep4){.ip = 0x7f030328, .port = (uint16_t)from->region};
		else if (!c->cfg.no_direct)
			ep_add(p, from->ep, EPSRC_LEARNED); /* a candidate path (magicsock.go:2645-2655) */
		tn_disco_put_ep(pong + 14, seen);
		disco_send(c, d, p, from, pong, sizeof pong);
		tn_logf(c, 2, "disco: ping from %s via %s", p->name, from->derp ? "derp" : ep_str(from->ep, es));
		break;
	}
	case TN_DISCO_PONG: {
		if (m < 2 + 12 + 18 || from->derp)
			return;
		for (uint8_t i = 0; i < p->n_ep; i++) {
			dp_ep *e = &p->ep[i];
			if (!ep_eq(e->ep, from->ep) || !e->ping_sent || memcmp(e->txid, pt + 2, 12))
				continue;
			if (d->now - e->ping_sent > PING_TIMEOUT_MS)
				return;
			int64_t rtt_us = mono_us() - e->ping_us;
			if (rtt_us < 1)
				rtt_us = 1;
			e->lat_us = (int32_t)rtt_us;
			e->last_pong = d->now;
			e->ping_sent = 0;
			/* betterAddr, simplified: lower RTT by more than 1%, or the current
			 * best has gone quiet (endpoint.go:2064-2150). */
			int adopt = p->best < 0 || (int)i == p->best || !trusted(d, p) ||
				    (int64_t)e->lat_us * 100 < (int64_t)p->best_lat_us * 99;
			if (adopt) {
				p->best = i;
				p->best_lat_us = e->lat_us;
				p->trust_until = d->now + TRUST_UDP_MS;
			}
			tn_logf(c, 2, "disco: pong from %s at %s, %.2f ms", p->name, ep_str(e->ep, es), rtt_us / 1000.0);
			return;
		}
		break;
	}
	case TN_DISCO_CMM: {
		if (c->cfg.no_direct)
			return;
		for (uint8_t i = 0; i < p->n_ep; i++)
			p->ep[i].src &= (uint8_t)~EPSRC_CMM;
		for (int off = 2; off + 18 <= m; off += 18) {
			tn_ep4 ep;
			if (tn_disco_get_ep(pt + off, &ep) == 0) {
				int k = ep_add(p, ep, EPSRC_CMM);
				if (k >= 0)
					p->ep[k].last_ping = 0; /* reset the rate limit (endpoint.go:2156-2215) */
			}
		}
		ep_compact(p);
		tn_logf(c, 2, "disco: call-me-maybe from %s (%d endpoints)", p->name, (m - 2) / 18);
		p->last_active = d->now;
		discover(c, d, p);
		break;
	}
	default:
		break;
	}
}

/* ---------------- WireGuard ---------------- */

/* Send a WireGuard message to the peer on its current path (endpoint.go:635-653). */
static void path_send(tn_ctx *c, tn_dp *d, dp_peer *p, const uint8_t *pkt, size_t n)
{
	int direct = !c->cfg.no_direct && p->best >= 0;
	if (direct)
		udp_send(c, d, p->ep[p->best].ep, pkt, n);
	if (!direct || !trusted(d, p))
		derp_send(c, d, peer_region(p), p->key, pkt, n);
	p->tx_bytes += n;
}

static void reply_send(tn_ctx *c, tn_dp *d, dp_peer *p, const dp_src *to, const uint8_t *pkt, size_t n)
{
	if (to->derp)
		derp_send(c, d, to->region, p->key, pkt, n);
	else
		udp_send(c, d, to->ep, pkt, n);
}

static void initiate(tn_ctx *c, tn_dp *d, dp_peer *p)
{
	uint8_t msg[TN_WG_INIT_LEN];
	if (!p->wg.hs_active)
		p->wg.hs_started = d->now;
	tn_wg_create_init(&d->wg, &p->wg, msg, new_index(d), wall_ns(), d->now);
	/* RekeyTimeout 5 s plus up to 333 ms jitter (timers.go). */
	uint16_t j;
	tn_random(&j, sizeof j);
	p->wg.hs_sent = d->now + (j % 334);
	tn_logf(c, 2, "wg: initiation to %s", p->name);
	path_send(c, d, p, msg, sizeof msg);
}

/* A sealed packet of the largest plaintext we send (TN_MTU, padded to 16) fits the seal buffer. */
_Static_assert(16 + ((TN_MTU + 15) & ~15) + 16 <= PKT_BUF, "buf2 holds a sealed TN_MTU packet");

size_t tn_dp_echo_reply(uint8_t *out, const tn_ip_info *ip)
{
	/* At most TN_MTU, whatever a peer sent: a request near PKT_BUF would seal past buf2 (16 bytes
	 * of header, padding and a 16-byte tag on top) and could not be staged either. Bigger: no
	 * answer (the request is dropped). */
	return tn_ip_build_icmp_echo(out, TN_MTU, ip->dst, ip->src, 0, ip->sport, ip->dport, ip->payload, ip->plen);
}

static int wg_send(tn_ctx *c, tn_dp *d, dp_peer *p, const uint8_t *pkt, size_t n)
{
	if (n > TN_MTU)
		return TN_E_TOOBIG; /* never past buf2 when sealed, or past a staging slot */
	tn_wg_keypair *kp = tn_wg_send_keypair(&p->wg, d->now);
	p->last_active = d->now;
	if (!trusted(d, p) && d->now - p->last_full_ping >= DISCO_PING_INTERVAL_MS)
		discover(c, d, p);
	if (!kp) {
		if (n) { /* stage it behind the handshake; oldest dropped when full */
			int slot = -1;
			for (int i = 0; i < STAGE_MAX && slot < 0; i++)
				if (!d->stage[i].len)
					slot = i;
			if (slot < 0)
				slot = (int)(d->now % STAGE_MAX);
			d->stage[slot].peer = (int)(p - d->peers);
			d->stage[slot].len = (uint16_t)n;
			memcpy(d->stage[slot].data, pkt, n);
		}
		if (!p->wg.hs_active)
			initiate(c, d, p);
		return TN_OK;
	}
	if (kp->initiator && d->now - kp->created >= TN_WG_REKEY_AFTER_MS && !p->wg.hs_active)
		initiate(c, d, p);
	size_t m = tn_wg_seal(kp, d->buf2, pkt, n);
	p->wg.keepalive_due = 0;
	path_send(c, d, p, d->buf2, m);
	return TN_OK;
}

static void flush_stage(tn_ctx *c, tn_dp *d, dp_peer *p)
{
	int idx = (int)(p - d->peers), sent = 0;
	for (int i = 0; i < STAGE_MAX; i++)
		if (d->stage[i].len && d->stage[i].peer == idx) {
			uint16_t len = d->stage[i].len;
			d->stage[i].len = 0;
			memcpy(d->sbuf, d->stage[i].data, len);
			wg_send(c, d, p, d->sbuf, len);
			sent = 1;
		}
	if (!sent && p->wg.cur.valid && p->wg.cur.initiator)
		wg_send(c, d, p, NULL, 0); /* confirm the session to the responder (receive.go:409) */
}

static int range_has(const tn_range4 *r, uint32_t ip) { return ip >= r->lo && ip <= r->hi; }

/* Does the tailnet's packet filter let `src` open `proto`/`port` on us? (spec 9; Go
 * wgengine/filter: IPv4 sources and destinations, port ranges, protocol lists.) */
static int filter_allows(const tn_dp *d, uint32_t src, uint8_t proto, uint16_t port)
{
	if (!d->filter_known)
		return 0; /* no filter from control yet: allow nothing (Go: NewAllowNone) */
	for (uint32_t i = 0; i < d->n_rules; i++) {
		const tn_filter_rule *r = &d->rules[i];
		int ok = 0;
		if (r->n_proto) {
			for (uint8_t k = 0; k < r->n_proto && !ok; k++)
				ok = r->proto[k] == proto;
		} else {
			ok = proto == TN_IPPROTO_TCP || proto == TN_IPPROTO_UDP || proto == TN_IPPROTO_ICMP;
		}
		if (!ok)
			continue;
		ok = 0;
		for (uint8_t k = 0; k < r->n_src && !ok; k++)
			ok = range_has(&r->src[k], src);
		if (!ok)
			continue;
		for (uint8_t k = 0; k < r->n_dst; k++)
			if (range_has(&r->dst[k].ip, d->self_addr4) &&
			    (proto == TN_IPPROTO_ICMP || (port >= r->dst[k].port_lo && port <= r->dst[k].port_hi)))
				return 1;
	}
	return 0;
}

/* With serve rules: the hard allow-list AND the tailnet filter for new flows. */
/* A new connection or flow to a served port: the tailnet's filter and the app's peer list both
 * let it in. One the filter allows but the list does not is noted (tn_peer_denied): the app can
 * offer to add that device. Nothing is logged per packet. */
static int new_flow_ok(tn_ctx *c, const tn_dp *d, const tn_ip_info *ip)
{
	if (!filter_allows(d, ip->src, ip->proto, ip->dport))
		return 0;
	if (tn_peer_allowed(&c->allow, ip->src))
		return 1;
	tn_denied_note(&c->denied, ip->src, ip->proto, ip->dport, tn_wall_s());
	return 0;
}

static int inbound_ok(tn_ctx *c, const tn_dp *d, const tn_ip_info *ip)
{
	if (!c->n_serve)
		return 1;
	if (ip->proto == TN_IPPROTO_ICMP)
		return ip->icmp_type == 8 && tn_peer_allowed(&c->allow, ip->src) && filter_allows(d, ip->src, ip->proto, 0);
	int listed = 0;
	for (uint32_t i = 0; i < c->n_serve && !listed; i++)
		listed = c->serve[i].proto == ip->proto && c->serve[i].port == ip->dport;
	if (!listed)
		return 0;
	/* A new flow also needs the app's own peer list (tn_set_peer_allow). */
	if (ip->proto == TN_IPPROTO_TCP) /* only a SYN opens a connection */
		return (ip->tcp_flags & 0x12) != 0x02 || new_flow_ok(c, d, ip);
	if (ip->proto == TN_IPPROTO_UDP)
		return tn_fwd_has_udp_flow(c, ip->src, ip->sport, ip->dport) || new_flow_ok(c, d, ip);
	return 0;
}

static void deliver(tn_ctx *c, tn_dp *d, dp_peer *p, uint8_t *pkt, size_t n)
{
	tn_ip_info ip;
	if (tn_ip_parse(pkt, n, &ip) < 0 || ip.dst != d->self_addr4 || !peer_allows(p, ip.src))
		return; /* cryptokey routing + our address only (spec 10) */
	if (!inbound_ok(c, d, &ip))
		return;
	if (ip.proto == TN_IPPROTO_ICMP && ip.icmp_type == 8) {
		/* Echo request: answer in the core (a cheap diagnostic). Built in place in d->buf. */
		size_t m = tn_dp_echo_reply(d->buf, &ip);
		if (m)
			wg_send(c, d, p, d->buf, m);
		return;
	}
	if (c->n_serve) {
		tn_fwd_input(c, pkt, &ip, d->now);
		return; /* a forwarder node hands nothing to packet_cb */
	}
	if (tn_fwd_expects(c, &ip)) {
		tn_fwd_input(c, pkt, &ip, d->now);
		return; /* our own mapped TCP connections and UDP flows */
	}
	if (c->cfg.packet_cb)
		c->cfg.packet_cb(c->cfg.user, pkt, ip.len);
}

static void handle_wg(tn_ctx *c, tn_dp *d, uint8_t *pkt, size_t n, const dp_src *from, dp_peer *derp_peer)
{
	if (n < 4)
		return;
	uint32_t type = tn_le32(pkt) & 0x7fffffffu; /* bit 31: Tailscale's retry flag */
	switch (type) {
	case TN_WG_MSG_INIT: {
		tn_wg_rx_init st;
		if (tn_wg_init_open(&d->wg, pkt, n, &st) < 0)
			return;
		dp_peer *p = peer_by_key(d, st.peer_pub);
		if (!p || !p->wg_ok || (derp_peer && derp_peer != p))
			return;
		if (tn_wg_init_finish(&p->wg, &st, pkt, d->now) < 0)
			return;
		uint8_t resp[TN_WG_RESP_LEN];
		tn_wg_create_resp(&p->wg, &st, resp, new_index(d), d->now);
		if (from->derp)
			p->heard_region = from->region;
		p->last_active = p->last_rx = d->now;
		tn_logf(c, 2, "wg: initiation from %s; responding", p->name);
		reply_send(c, d, p, from, resp, sizeof resp);
		break;
	}
	case TN_WG_MSG_RESP: {
		if (n != TN_WG_RESP_LEN)
			return;
		dp_peer *p = peer_by_index(d, tn_le32(pkt + 8), NULL);
		if (!p || (derp_peer && derp_peer != p))
			return;
		if (tn_wg_consume_resp(&d->wg, &p->wg, pkt, n, d->now) < 0)
			return;
		p->last_rx = d->now;
		tn_logf(c, 1, "wg: handshake with %s done (%s)", p->name, from->derp ? "via derp" : "direct");
		flush_stage(c, d, p);
		break;
	}
	case TN_WG_MSG_COOKIE: {
		if (n != TN_WG_COOKIE_LEN)
			return;
		dp_peer *p = peer_by_index(d, tn_le32(pkt + 4), NULL);
		if (p)
			tn_wg_consume_cookie(&p->wg, pkt, n, d->now);
		break;
	}
	case TN_WG_MSG_DATA: {
		tn_wg_keypair *kp = NULL;
		if (n < 32)
			return;
		dp_peer *p = peer_by_index(d, tn_le32(pkt + 4), &kp);
		if (!p || !kp || (derp_peer && derp_peer != p))
			return;
		int m = tn_wg_open(kp, pkt, n, d->now);
		if (m < 0)
			return;
		if (kp == &p->wg.next) { /* the initiator used the new keys: confirmed */
			tn_wg_confirm_next(&p->wg);
			flush_stage(c, d, p);
		}
		p->last_rx = d->now;
		p->rx_bytes += n;
		if (from->derp)
			p->heard_region = from->region;
		tn_wg_keypair *cur = &p->wg.cur;
		if (cur->valid && cur->initiator && !p->wg.hs_active &&
		    d->now - cur->created >= TN_WG_REJECT_AFTER_MS - TN_WG_KEEPALIVE_MS - TN_WG_REKEY_TIMEOUT_MS)
			initiate(c, d, p);
		if (m == 0)
			return; /* keepalive */
		p->last_active = d->now;
		if (!p->wg.keepalive_due)
			p->wg.keepalive_due = d->now + TN_WG_KEEPALIVE_MS;
		deliver(c, d, p, pkt + 16, (size_t)m);
		break;
	}
	default:
		break;
	}
}

/* ---------------- netcheck / STUN ---------------- */

static void netcheck_start(tn_ctx *c, tn_dp *d)
{
	d->n_probes = 0;
	for (uint32_t i = 0; i < d->n_regions && d->n_probes < PROBES_MAX; i++) {
		const tn_derp_region *r = &d->regions[i];
		if (r->avoid || r->no_measure_no_home)
			continue;
		for (uint8_t k = 0; k < r->n_nodes; k++) {
			const tn_derp_node *nd = &r->nodes[k];
			if (nd->stun_port < 0)
				continue;
			uint32_t ip = node_ip(nd);
			if (!ip)
				continue;
			dp_probe *pr = &d->probes[d->n_probes++];
			memset(pr, 0, sizeof *pr);
			pr->region = r->id;
			pr->ip = ip;
			pr->port = (uint16_t)(nd->stun_port ? nd->stun_port : 3478);
			pr->rtt_us = -1;
			break; /* the first STUN node of each region (spec 4.4) */
		}
	}
	d->nc_running = 1;
	d->nc_start = d->now;
	tn_logf(c, 2, "netcheck: probing %u regions", d->n_probes);
}

static void probe_send(tn_ctx *c, tn_dp *d, dp_probe *pr)
{
	uint8_t req[TN_STUN_REQ_LEN];
	tn_random(pr->txid, 12);
	tn_stun_request(req, pr->txid);
	pr->sent = d->now;
	pr->sent_us = mono_us();
	if (!pr->first_sent)
		pr->first_sent = d->now;
	pr->tries++;
	udp_send(c, d, (tn_ep4){.ip = pr->ip, .port = pr->port}, req, sizeof req);
}

static void local_endpoints(tn_dp *d, tn_ep4 *out, uint32_t *n, uint32_t cap)
{
	uint32_t ips[TN_REPORT_EPS];
	int m = tn_os_local_ipv4(ips, TN_REPORT_EPS);
	for (int i = 0; i < m && *n < cap; i++)
		out[(*n)++] = (tn_ep4){.ip = ips[i], .port = d->port};
}

static void publish(tn_ctx *c, tn_dp *d)
{
	tn_mutex_lock(&c->mu);
	tn_report *r = &c->report;
	int changed = r->n_ep != d->n_eps || memcmp(r->ep, d->eps, d->n_eps * sizeof *d->eps) ||
		      r->preferred_derp != d->home;
	r->n_ep = d->n_eps;
	memcpy(r->ep, d->eps, sizeof r->ep);
	r->preferred_derp = d->home;
	r->n_lat = 0;
	for (uint32_t i = 0; i < d->n_probes && r->n_lat < TN_REPORT_LAT; i++)
		if (d->probes[i].rtt_us >= 0) {
			r->lat[r->n_lat].region = d->probes[i].region;
			r->lat[r->n_lat].us = d->probes[i].rtt_us;
			r->n_lat++;
		}
	tn_mutex_unlock(&c->mu);
	if (changed) {
		tn_atomic_store(&c->report_dirty, 1);
		kick_control(c);
	}
}

static void netcheck_finish(tn_ctx *c, tn_dp *d)
{
	d->nc_running = 0;
	d->nc_next = d->now + RESTUN_MS;
	/* Home: the lowest RTT, with Go's hysteresis (netcheck.go:1380-1392). */
	int32_t best = 0, best_us = -1, old_us = -1;
	tn_ep4 mapped[PROBES_MAX];
	uint32_t nm = 0;
	for (uint32_t i = 0; i < d->n_probes; i++) {
		dp_probe *pr = &d->probes[i];
		if (pr->rtt_us < 0)
			continue;
		if (pr->region == d->home)
			old_us = pr->rtt_us;
		if (best_us < 0 || pr->rtt_us < best_us) {
			best = pr->region;
			best_us = pr->rtt_us;
		}
		mapped[nm++] = pr->mapped;
	}
	int32_t home = d->home;
	if (best && (!home || old_us < 0 || (old_us - best_us > 10000 && best_us * 3 < old_us * 2)))
		home = best;
	if (!home) {
		/* UDP blocked: no STUN answers. Keep DERP working: the netmap's home for us,
		 * else the first region that has a DERP node. */
		tn_mutex_lock(&c->mu);
		home = c->nms.nm.self_home_derp;
		tn_mutex_unlock(&c->mu);
		for (uint32_t i = 0; !home && i < d->n_regions; i++)
			if (region_node(&d->regions[i]) && !d->regions[i].avoid)
				home = d->regions[i].id;
	}
	if (home != d->home) {
		tn_logf(c, 1, "netcheck: home DERP region %d (%.2f ms)", home, best == home ? best_us / 1000.0 : -1.0);
		for (int i = 0; i < DERP_CONNS; i++)
			if (d->conns[i].region == d->home && d->conns[i].st == TN_DC_READY)
				tn_derpc_note_preferred(&d->conns[i], 0);
		d->home = home;
		tn_derpc *k = conn_get(c, d, home);
		if (k && k->st == TN_DC_READY)
			tn_derpc_note_preferred(k, 1);
	}
	d->home_lat_us = old_us >= 0 && home == d->home && home != best ? old_us : (home == best ? best_us : -1);
	/* Endpoints: local interfaces with our port, then STUN results (spec 5). */
	tn_ep4 eps[TN_REPORT_EPS];
	uint32_t n = 0;
	int hard = 0;
	for (uint32_t i = 0; i < nm; i++) {
		int dup = 0;
		for (uint32_t k = 0; k < n; k++)
			dup |= ep_eq(eps[k], mapped[i]);
		if (i > 0 && mapped[i].ip == mapped[0].ip && mapped[i].port != mapped[0].port)
			hard = 1;
		if (!dup && n < TN_REPORT_EPS)
			eps[n++] = mapped[i];
	}
	if (hard && nm && n < TN_REPORT_EPS) /* destination-dependent NAT: guess our port (type 4) */
		eps[n++] = (tn_ep4){.ip = mapped[0].ip, .port = d->port};
	tn_ep4 local[TN_REPORT_EPS];
	uint32_t nl = 0;
	local_endpoints(d, local, &nl, TN_REPORT_EPS);
	for (uint32_t i = 0; i < nl && n < TN_REPORT_EPS; i++) {
		int dup = 0;
		for (uint32_t k = 0; k < n; k++)
			dup |= ep_eq(eps[k], local[i]);
		if (!dup)
			eps[n++] = local[i];
	}
	if (c->cfg.no_direct)
		n = 0;
	if (n != d->n_eps || memcmp(eps, d->eps, n * sizeof *eps)) {
		char b[24];
		tn_logf(c, 1, "endpoints: %u (first %s)", n, n ? ep_str(eps[0], b) : "-");
	}
	d->n_eps = n;
	memcpy(d->eps, eps, sizeof eps);
	d->eps_at = d->now;
	publish(c, d);
}

static void handle_stun(tn_ctx *c, tn_dp *d, const uint8_t *p, size_t n)
{
	uint8_t txid[12];
	tn_ep4 ep;
	if (tn_stun_parse(p, n, txid, &ep) < 0)
		return;
	for (uint32_t i = 0; i < d->n_probes; i++) {
		dp_probe *pr = &d->probes[i];
		if (memcmp(pr->txid, txid, 12) != 0 || pr->done)
			continue;
		int64_t us = mono_us() - pr->sent_us;
		pr->rtt_us = (int32_t)(us < 1 ? 1 : us);
		pr->mapped = ep;
		pr->done = 1;
		break;
	}
	(void)c;
}

/* ---------------- receive ---------------- */

static void on_udp(tn_ctx *c, tn_dp *d)
{
	for (int i = 0; i < 256 && d->udp >= 0; i++) {
		uint32_t sip;
		uint16_t sport;
		ssize_t n = tn_sock_recvfrom(d->udp, d->buf, sizeof d->buf, &sip, &sport);
		if (n < 0) {
			int e = tn_sock_raw_errno(), k = tn_sock_errno();
			if (k == TN_ERR_AGAIN || k == TN_ERR_INTR || k == TN_ERR_REFUSED || k == TN_ERR_RESET)
				return;
			tn_logf(c, 1, "udp: receive failed (errno %d); rebinding", e);
			udp_close(d);
			d->udp_retry_at = d->now + 1000;
			return;
		}
		dp_src from = {.ep = {.ip = sip, .port = sport}};
		if (tn_stun_is(d->buf, (size_t)n) && d->buf[1] == 0x01)
			handle_stun(c, d, d->buf, (size_t)n);
		else if (tn_disco_is(d->buf, (size_t)n) && !c->cfg.no_direct) /* DERP-only: no UDP paths at all */
			handle_disco(c, d, d->buf, (size_t)n, &from);
		else if (!c->cfg.no_direct)
			handle_wg(c, d, d->buf, (size_t)n, &from, NULL);
	}
}

static void on_derp_frame(void *u, tn_derpc *k, uint8_t type, const uint8_t *p, size_t n)
{
	tn_ctx *c = u;
	tn_dp *d = c->dp;
	switch (type) {
	case TN_DERP_SERVER_INFO:
		tn_logf(c, 1, "derp %d: connected%s", k->region, k->region == d->home ? " (home)" : "");
		if (k->region == d->home)
			tn_derpc_note_preferred(k, 1);
		/* Initiations and call-me-maybes that waited for this connection. */
		for (uint32_t i = 0; i < d->n_peers; i++) {
			dp_peer *pp = &d->peers[i];
			if (pp->used && pp->wg.hs_active && peer_region(pp) == k->region)
				initiate(c, d, pp);
		}
		break;
	case TN_DERP_RECV: {
		if (n < 32 || n - 32 > sizeof d->buf)
			return;
		dp_peer *pp = peer_by_key(d, p);
		if (!pp)
			return;
		k->last_use = d->now;
		memcpy(d->buf, p + 32, n - 32);
		dp_src from = {.derp = 1, .region = k->region};
		if (tn_disco_is(d->buf, n - 32)) {
			pp->heard_region = k->region;
			handle_disco(c, d, d->buf, n - 32, &from);
		} else {
			handle_wg(c, d, d->buf, n - 32, &from, pp);
		}
		break;
	}
	case TN_DERP_HEALTH:
		if (n)
			tn_logf(c, 0, "derp %d: health: %.*s", k->region, (int)(n > 200 ? 200 : n), (const char *)p);
		break;
	case TN_DERP_RESTARTING: {
		uint32_t in_ms = n >= 4 ? tn_be32(p) : 1000;
		tn_logf(c, 1, "derp %d: server restarting; reconnecting in %u ms", k->region, in_ms);
		tn_derpc_close(k);
		k->retry_at = d->now + (in_ms > 60000 ? 60000 : in_ms);
		break;
	}
	default:
		break; /* PeerGone, Pong, mesh frames */
	}
}

/* ---------------- timers ---------------- */

static void emit_path(tn_ctx *c, tn_dp *d, dp_peer *p)
{
	tn_path now = TN_PATH_NONE;
	tn_ep4 ep = {0};
	int session = tn_wg_send_keypair(&p->wg, d->now) != NULL && p->last_rx && d->now - p->last_rx < 30000;
	if (session && trusted(d, p) && !c->cfg.no_direct) {
		now = TN_PATH_DIRECT;
		ep = p->ep[p->best].ep;
	} else if (session) {
		now = TN_PATH_DERP;
	}
	if (now == p->reported && ep_eq(ep, p->reported_ep))
		return;
	p->reported = now;
	p->reported_ep = ep;
	char text[160], es[24];
	if (now == TN_PATH_DIRECT)
		snprintf(text, sizeof text, "%s: direct %s, %.2f ms", p->name, ep_str(ep, es), p->best_lat_us / 1000.0);
	else if (now == TN_PATH_DERP)
		snprintf(text, sizeof text, "%s: relayed via DERP region %d", p->name, peer_region(p));
	else
		snprintf(text, sizeof text, "%s: idle", p->name);
	tn_emit(c, TN_EV_PATH, 0, text);
}

static void tick(tn_ctx *c, tn_dp *d)
{
	/* Rest mode without tn_pause: the wall clock jumped while monotonic time did
	 * not (spec 13, [mem] helper-survives-rest). Run the wake sequence. */
	if (d->now - d->last_wall_check >= WALL_CHECK_MS) {
		int64_t off = wall_ms() - d->now;
		if (d->last_wall_check && (off - d->wall_off > WALL_JUMP_MS || d->wall_off - off > WALL_JUMP_MS)) {
			tn_logf(c, 0, "clock jumped %+lld s: re-establishing paths", (long long)((off - d->wall_off) / 1000));
			tn_dp_rewake(c, d);
		}
		d->wall_off = off;
		d->last_wall_check = d->now;
	}
	if (d->udp < 0 && d->now >= d->udp_retry_at && udp_open(c, d) < 0)
		d->udp_retry_at = d->now + 2000;
	/* netcheck */
	if (!d->nc_running && d->now >= d->nc_next && d->n_regions)
		netcheck_start(c, d);
	if (d->nc_running) {
		int pending = 0;
		for (uint32_t i = 0; i < d->n_probes; i++) {
			dp_probe *pr = &d->probes[i];
			if (pr->done)
				continue;
			pending = 1;
			if (!pr->tries || (pr->tries < 4 && d->now - pr->sent >= 100 * (1 << pr->tries)))
				probe_send(c, d, pr);
		}
		if (!pending || d->now - d->nc_start >= NETCHECK_DEADLINE_MS)
			netcheck_finish(c, d);
	}
	/* DERP: keep home up; close idle and dead ones; time out connects. */
	for (int i = 0; i < DERP_CONNS; i++) {
		tn_derpc *k = &d->conns[i];
		if (k->fd < 0) {
			if (k->retry_at && d->now >= k->retry_at) {
				if (k->region == d->home || d->now - k->last_use < DERP_IDLE_CLOSE_MS)
					conn_open(c, d, k, k->region);
				else
					k->retry_at = 0;
			}
			continue;
		}
		if (k->st != TN_DC_READY && d->now - k->t_start > DERP_CONNECT_TIMEOUT_MS)
			conn_fail(c, d, k, TN_E_TIMEOUT);
		else if (k->st == TN_DC_READY && d->now - k->last_rx > DERP_DEAD_MS)
			conn_fail(c, d, k, TN_E_TIMEOUT);
		else if (k->region != d->home && d->now - k->last_use > DERP_IDLE_CLOSE_MS) {
			tn_logf(c, 2, "derp %d: idle; closing", k->region);
			tn_derpc_close(k);
			k->retry_at = 0;
			k->region = 0;
		}
	}
	if (d->home && !conn_for(d, d->home))
		conn_get(c, d, d->home);
	/* peers */
	for (uint32_t i = 0; i < d->n_peers; i++) {
		dp_peer *p = &d->peers[i];
		if (!p->used || !p->wg_ok)
			continue;
		if (p->wg.hs_active && d->now - p->wg.hs_sent >= TN_WG_REKEY_TIMEOUT_MS) {
			if (d->now - p->wg.hs_started >= TN_WG_REKEY_ATTEMPT_MS) {
				tn_logf(c, 1, "wg: no handshake with %s after %d s; giving up", p->name,
					TN_WG_REKEY_ATTEMPT_MS / 1000);
				p->wg.hs_active = 0;
				stage_drop(d, (int)i);
			} else {
				initiate(c, d, p);
			}
		}
		if (p->wg.keepalive_due && d->now >= p->wg.keepalive_due) {
			p->wg.keepalive_due = 0;
			if (tn_wg_send_keypair(&p->wg, d->now))
				wg_send(c, d, p, NULL, 0);
		}
		int active = d->now - p->last_active < SESSION_ACTIVE_MS;
		if (active && p->has_disco && !c->cfg.no_direct) {
			if (p->best >= 0 && d->now - p->last_heartbeat >= HEARTBEAT_MS) {
				send_ping(c, d, p, &p->ep[p->best]);
				p->last_heartbeat = d->now;
			}
			int want_full = !trusted(d, p) ? d->now - p->last_full_ping >= DISCO_PING_INTERVAL_MS
						       : (d->now - p->last_full_ping >= UPGRADE_INTERVAL_MS &&
							  p->best_lat_us > GOOD_ENOUGH_US);
			if (want_full)
				discover(c, d, p);
		}
		emit_path(c, d, p);
	}
}

/* ---------------- wake / pause ---------------- */

static void drop_sessions(tn_ctx *c, tn_dp *d)
{
	tn_fwd_reset(c);
	for (uint32_t i = 0; i < d->n_peers; i++)
		if (d->peers[i].used) {
			tn_wg_reset(&d->peers[i].wg);
			disco_reset(&d->peers[i]);
			d->peers[i].heard_region = 0;
		}
	stage_drop(d, -1);
}

static void close_all(tn_dp *d)
{
	udp_close(d);
	for (int i = 0; i < DERP_CONNS; i++) {
		tn_derpc_close(&d->conns[i]);
		d->conns[i].retry_at = 0;
		d->conns[i].backoff_ms = 0;
		d->conns[i].region = 0;
	}
}

/* The wake sequence (spec 13): rebind UDP on the same port, drop WireGuard keys and
 * disco trust, reconnect DERP, netcheck now, and a fresh control session. */
void tn_dp_rewake(tn_ctx *c, tn_dp *d)
{
	close_all(d);
	drop_sessions(c, d);
	d->udp_retry_at = 0;
	d->nc_running = 0;
	d->nc_next = 0;
	d->home = 0;
	tn_atomic_store(&c->ctl_reconnect, 1);
	kick_control(c);
}

/* ---------------- the loop ---------------- */

static void drain(int fd) { tn_wake_drain(fd); }

static void *dp_loop(void *arg)
{
	tn_ctx *c = arg;
	tn_dp *d = c->dp;
	if (c->cfg.thread_cb)
		c->cfg.thread_cb(c->cfg.user, "data");
	enum { DP_POLL_MAX = 2 + DERP_CONNS + 64 };
	struct pollfd pf[DP_POLL_MAX];
	tn_derpc *pk[DP_POLL_MAX];
	int kind[DP_POLL_MAX]; /* 0 wake, 1 udp, 2 derp, 3 forwarder */
	tn_mutex_lock(&c->dp_mu);
	d->now = tn_now_ms();
	while (!tn_atomic_load(&c->req_stop)) {
		d->now = tn_now_ms();
		if (tn_atomic_load(&c->req_pause)) {
			if (!d->paused) {
				tn_logf(c, 1, "data plane paused");
				close_all(d);
				drop_sessions(c, d);
				d->paused = 1;
			}
		} else if (d->paused) {
			d->paused = 0;
			tn_dp_rewake(c, d);
		} else if (tn_atomic_xchg(&c->req_rewake, 0)) {
			tn_logf(c, 1, "wake sequence (test)");
			tn_dp_rewake(c, d);
		}
		if ((uint32_t)tn_atomic_load(&c->nm_gen) != d->nm_gen) {
			sync_netmap(c, d);
			/* A peer gets each netmap change at the same time we do. A fresh node's first
			 * initiation can reach a peer that has no DERP region or endpoint for us yet
			 * (tailscaled: "no UDP or DERP addr"); resend pending ones now. */
			for (uint32_t i = 0; i < d->n_peers; i++) {
				dp_peer *p = &d->peers[i];
				if (p->used && p->wg.hs_active && !p->wg.cur.valid && d->now - p->wg.hs_sent >= 200)
					initiate(c, d, p);
			}
		}
		int timeout = TICK_MS;
		if (!d->paused) {
			if (d->now - d->last_tick >= TICK_MS) {
				tick(c, d);
				d->last_tick = d->now;
			}
			tn_fwd_tick(c, d->now);
			int fs = tn_fwd_sleep_ms(c);
			int64_t to_tick = TICK_MS - (tn_now_ms() - d->last_tick);
			timeout = to_tick < 1 ? 1 : (int)to_tick;
			if (fs >= 0 && fs < timeout)
				timeout = fs < 1 ? 1 : fs;
		}
		int n = 0;
		pf[n] = (struct pollfd){.fd = c->dp_wake_r, .events = POLLIN};
		kind[n] = 0;
		pk[n++] = NULL;
		if (!d->paused && d->udp >= 0) {
			pf[n] = (struct pollfd){.fd = d->udp, .events = POLLIN};
			kind[n] = 1;
			pk[n++] = NULL;
		}
		for (int i = 0; !d->paused && i < DERP_CONNS; i++)
			if (d->conns[i].fd >= 0) {
				pf[n] = (struct pollfd){.fd = d->conns[i].fd, .events = tn_derpc_events(&d->conns[i])};
				kind[n] = 2;
				pk[n++] = &d->conns[i];
			}
		if (!d->paused) {
			int m = tn_fwd_pollfds(c, pf + n, DP_POLL_MAX - n);
			for (int i = 0; i < m; i++) {
				kind[n + i] = 3;
				pk[n + i] = NULL;
			}
			n += m;
		}
		tn_mutex_unlock(&c->dp_mu);
		int r = tn_poll(pf, (unsigned)n, d->paused ? -1 : timeout);
		int poll_errno = r < 0 ? tn_sock_raw_errno() : 0;
		int interrupted = r < 0 && tn_sock_errno() == TN_ERR_INTR;
		tn_mutex_lock(&c->dp_mu);
		d->now = tn_now_ms();
		if (r < 0 && !interrupted) {
			/* poll() itself failing (the network going away for rest mode): never spin. Wait a
			 * little with the lock released, and say so at most every 10 s. */
			if (!d->last_poll_fail_log || d->now - d->last_poll_fail_log >= 10000) {
				tn_logf(c, 0, "data plane: poll failed (errno %d); backing off", poll_errno);
				d->last_poll_fail_log = d->now;
			}
			tn_mutex_unlock(&c->dp_mu);
			tn_sleep_ms(tn_poll_fail_wait_ms(0, -1));
			tn_mutex_lock(&c->dp_mu);
			d->now = tn_now_ms();
			continue;
		}
		if (r <= 0)
			continue;
		for (int i = 0; i < n; i++) {
			if (!pf[i].revents)
				continue;
			if (kind[i] == 0) {
				drain(c->dp_wake_r);
			} else if (kind[i] == 1) {
				if ((int)pf[i].fd == d->udp)
					on_udp(c, d);
			} else if (kind[i] == 3) {
				tn_fwd_io(c, (int)pf[i].fd, pf[i].revents, d->now);
			} else if (pk[i]->fd == (int)pf[i].fd) {
				int e = tn_derpc_io(pk[i], pf[i].revents, on_derp_frame, c, d->now);
				if (e < 0)
					conn_fail(c, d, pk[i], e);
			}
		}
	}
	close_all(d);
	tn_mutex_unlock(&c->dp_mu);
	return NULL;
}

/* ---------------- API ---------------- */

int tn_dp_create(tn_ctx *c)
{
	tn_dp *d = calloc(1, sizeof *d);
	if (!d)
		return TN_E_NOMEM;
	d->cap = c->nms.max_peers;
	d->cap_regions = c->nms.max_regions;
	d->cap_rules = c->nms.max_rules;
	d->peers = calloc(d->cap, sizeof *d->peers);
	d->regions = calloc(d->cap_regions, sizeof *d->regions);
	d->rules = calloc(d->cap_rules, sizeof *d->rules);
	if (!d->peers || !d->regions || !d->rules) {
		free(d->peers);
		free(d->regions);
		free(d->rules);
		free(d);
		return TN_E_NOMEM;
	}
	d->udp = -1;
	d->last_peer = -1;
	d->home_lat_us = -1;
	for (int i = 0; i < DERP_CONNS; i++)
		d->conns[i].fd = -1;
	c->dp = d;
	c->footprint += sizeof *d + d->cap * sizeof *d->peers + d->cap_regions * sizeof *d->regions +
			d->cap_rules * sizeof *d->rules;
	return TN_OK;
}

void tn_dp_destroy(tn_ctx *c)
{
	tn_dp *d = c->dp;
	if (!d)
		return;
	free(d->peers);
	free(d->regions);
	free(d->rules);
	tn_wipe(d, sizeof *d);
	free(d);
	c->dp = NULL;
}

int tn_dp_start(tn_ctx *c)
{
	tn_dp *d = c->dp;
	/* Keys: the node key is the WireGuard static and the DERP identity. */
	tn_wg_self_init(&d->wg, c->keys.node_priv, c->keys.node_pub);
	memcpy(d->dkeys.node_priv, c->keys.node_priv, 32);
	memcpy(d->dkeys.node_pub, c->keys.node_pub, 32);
	memcpy(d->disco_priv, c->keys.disco_priv, 32);
	memcpy(d->disco_pub, c->keys.disco_pub, 32);
	int r = tn_thread_start(&c->dp_thread, dp_loop, c, c->cfg.thread_stack ? c->cfg.thread_stack : 256 * 1024);
	if (r != 0)
		return TN_E_NOMEM;
	c->dp_started = 1;
	return TN_OK;
}

void tn_dp_stop(tn_ctx *c)
{
	if (!c->dp_started)
		return;
	tn_dp_wake(c);
	tn_thread_join(c->dp_thread);
	c->dp_started = 0;
}

int tn_dp_output(tn_ctx *c, const uint8_t *pkt, size_t n)
{
	tn_dp *d = c->dp;
	if (n > TN_MTU || n < 20)
		return TN_E_TOOBIG;
	dp_peer *p = peer_by_ip(d, tn_be32(pkt + 16));
	return p ? wg_send(c, d, p, pkt, n) : TN_E_ARG;
}

int tn_map(tn_ctx *c, uint8_t proto, uint32_t remote_ip, uint16_t remote_port, uint16_t want_port,
	   uint16_t *local_port)
{
	tn_mutex_lock(&c->dp_mu);
	int r = tn_fwd_map(c, proto, remote_ip, remote_port, want_port, local_port);
	tn_mutex_unlock(&c->dp_mu);
	tn_dp_wake(c); /* poll the new socket */
	return r;
}

void tn_unmap(tn_ctx *c, uint8_t proto, uint16_t local_port)
{
	tn_mutex_lock(&c->dp_mu);
	tn_fwd_unmap(c, proto, local_port);
	tn_mutex_unlock(&c->dp_mu);
	tn_dp_wake(c);
}

int tn_send_ip(tn_ctx *c, const uint8_t *pkt, size_t len)
{
	if (len > TN_MTU)
		return TN_E_TOOBIG;
	if (len < 20 || (pkt[0] >> 4) != 4)
		return TN_E_ARG;
	tn_mutex_lock(&c->dp_mu);
	tn_dp *d = c->dp;
	int r = TN_E_STATE;
	if (d && c->dp_started && !d->paused && !tn_atomic_load(&c->req_stop)) {
		d->now = tn_now_ms();
		dp_peer *p = peer_by_ip(d, tn_be32(pkt + 16));
		r = p ? wg_send(c, d, p, pkt, len) : TN_E_ARG;
	}
	tn_mutex_unlock(&c->dp_mu);
	return r;
}

int tn_get_status(tn_ctx *c, tn_status *st, tn_peer_status *ps, uint32_t cap)
{
	memset(st, 0, sizeof *st);
	tn_mutex_lock(&c->dp_mu);
	tn_dp *d = c->dp;
	int64_t now = tn_now_ms();
	st->state = c->state;
	st->home_latency_us = -1;
	if (d) {
		st->self_addr4 = d->self_addr4;
		tn_strlcpy(st->self_name, d->self_name, sizeof st->self_name);
		st->home_derp = d->home;
		st->home_latency_us = d->home_lat_us;
		tn_derpc *k = conn_for(d, d->home);
		st->derp_connected = k && k->st == TN_DC_READY;
		st->udp_port = d->port;
		st->n_endpoints = d->n_eps;
		memcpy(st->endpoints, d->eps, sizeof st->endpoints);
		uint32_t n = 0;
		for (uint32_t i = 0; i < d->n_peers; i++) {
			dp_peer *p = &d->peers[i];
			if (!p->used)
				continue;
			if (ps && n < cap) {
				tn_peer_status *o = &ps[n];
				memset(o, 0, sizeof *o);
				tn_strlcpy(o->name, p->name, sizeof o->name);
				o->addr4 = p->addr4;
				o->online = p->online > 0;
				o->handshake = p->wg.cur.valid && now - p->wg.cur.created < TN_WG_REJECT_AFTER_MS;
				o->path = p->reported;
				if (p->best >= 0)
					o->direct = p->ep[p->best].ep;
				o->derp_region = peer_region(p);
				o->latency_us = p->reported == TN_PATH_DIRECT ? p->best_lat_us : -1;
				o->last_rx_ms = p->last_rx ? now - p->last_rx : -1;
				o->tx_bytes = p->tx_bytes;
				o->rx_bytes = p->rx_bytes;
			}
			n++;
		}
		st->n_peers = n;
		tn_fwd_stats(c, &st->fwd_udp_flows, &st->fwd_tcp_conns);
	}
	tn_mutex_unlock(&c->dp_mu);
	tn_mutex_lock(&c->mu);
	if (c->state == TN_ST_NEEDS_LOGIN)
		tn_strlcpy(st->login_url, c->last_url, sizeof st->login_url);
	st->key_expiry = c->nms.nm.self_key_expiry;
	if (!st->self_addr4) {
		st->self_addr4 = c->nms.nm.self_addr4;
		tn_strlcpy(st->self_name, c->nms.nm.self_name, sizeof st->self_name);
	}
	tn_mutex_unlock(&c->mu);
	return TN_OK;
}
