/* tnctl: drive the tailnet library from a terminal.
 *
 *   tnctl login --control URL --state DIR [--authkey K] [--hostname H] [--ca-file PEM]
 *               [--transport auto|plain|tls] [--once] [--timeout SEC] [--dump DIR] [-v|-vv]
 *               [--port UDP] [--no-direct]
 *               [--ping 100.x.y.z] [--udp-echo 100.x.y.z:PORT] [--count N] [--interval MS]
 *               [--wait-direct SEC] [--serve tcp:PORT[:LOCAL],udp:PORT[:LOCAL],...]
 *
 * --map tcp:LOCAL:IP:PORT,udp:LOCAL:IP:PORT maps 127.0.0.1:LOCAL to a peer's tailnet
 * address (the engine's role): ordinary sockets reach the peer through the tunnel.
 * --serve makes this node a forwarder like the PS5 helper: only the listed tailnet
 * ports are accepted, relayed (UDP) or spliced (TCP) to 127.0.0.1:LOCAL.
 *
 * Prints the login URL (interactive) or registers with the key, then streams a
 * netmap summary each time it changes. --once exits after the first netmap.
 * --ping sends ICMP echo requests through the tunnel, --udp-echo UDP datagrams to an
 * echo server; both print each RTT with the path in use and a summary, then exit.
 * SIGUSR1 pauses the node (rest mode), SIGUSR2 resumes it.
 */
#include <arpa/inet.h>
#include <pthread.h>
#include <sys/socket.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "tailnet.h"

static volatile sig_atomic_t g_quit;
static volatile sig_atomic_t g_got_netmap;
static volatile sig_atomic_t g_pause, g_resume;
static int g_once;

/* probe state (written by the packet callback, read by main) */
static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static uint32_t g_target;      /* host order */
static uint16_t g_target_port; /* 0 = ICMP */
static uint16_t g_id = 0x7e57;
static uint16_t g_got_seq;
static int g_got;

static double now_ms(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return ts.tv_sec * 1000.0 + ts.tv_nsec / 1e6;
}

static uint16_t be16(const uint8_t *p) { return (uint16_t)(p[0] << 8 | p[1]); }
static uint32_t be32(const uint8_t *p) { return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 | p[3]; }
static void put16(uint8_t *p, uint16_t v) { p[0] = (uint8_t)(v >> 8); p[1] = (uint8_t)v; }
static void put32(uint8_t *p, uint32_t v) { put16(p, (uint16_t)(v >> 16)); put16(p + 2, (uint16_t)v); }

static uint16_t csum(const uint8_t *p, size_t n, uint32_t sum)
{
	for (; n > 1; n -= 2, p += 2)
		sum += (uint32_t)p[0] << 8 | p[1];
	if (n)
		sum += (uint32_t)p[0] << 8;
	while (sum >> 16)
		sum = (sum & 0xffff) + (sum >> 16);
	return (uint16_t)~sum;
}

/* Build an ICMP echo request or a UDP datagram (payload carries the sequence). */
static size_t build_probe(uint8_t *pkt, uint32_t src, uint16_t seq)
{
	size_t plen = 32, l4 = g_target_port ? 8 + plen : 8 + plen, total = 20 + l4;
	memset(pkt, 0, total);
	pkt[0] = 0x45;
	put16(pkt + 2, (uint16_t)total);
	put16(pkt + 4, seq);
	pkt[8] = 64;
	pkt[9] = g_target_port ? 17 : 1;
	put32(pkt + 12, src);
	put32(pkt + 16, g_target);
	put16(pkt + 10, csum(pkt, 20, 0));
	uint8_t *t = pkt + 20;
	snprintf((char *)t + 8, plen, "tnctl probe %u", seq);
	put16(t + 8 + 28, seq);
	if (g_target_port) {
		put16(t, 40000);
		put16(t + 2, g_target_port);
		put16(t + 4, (uint16_t)l4);
		uint32_t ps = (src >> 16) + (src & 0xffff) + (g_target >> 16) + (g_target & 0xffff) + 17 + (uint32_t)l4;
		uint16_t c = csum(t, l4, ps);
		put16(t + 6, c ? c : 0xffff);
	} else {
		t[0] = 8;
		put16(t + 4, g_id);
		put16(t + 6, seq);
		put16(t + 2, csum(t, l4, 0));
	}
	return total;
}

static void on_packet(void *user, const uint8_t *pkt, size_t len)
{
	(void)user;
	if (len < 28 || (pkt[0] >> 4) != 4)
		return;
	size_t hl = (size_t)(pkt[0] & 15) * 4;
	if (be32(pkt + 12) != g_target || len < hl + 8)
		return;
	const uint8_t *t = pkt + hl;
	pthread_mutex_lock(&g_mu);
	if (!g_target_port && pkt[9] == 1 && t[0] == 0 && be16(t + 4) == g_id) {
		g_got_seq = be16(t + 6);
		g_got = 1;
	} else if (g_target_port && pkt[9] == 17 && be16(t) == g_target_port && len >= hl + 8 + 30) {
		g_got_seq = be16(t + 8 + 28);
		g_got = 1;
	}
	pthread_mutex_unlock(&g_mu);
}

/* SIGUSR1/SIGUSR2 simulate rest mode and wake (tn_pause/tn_resume). */
static void handle_signals(tn_ctx *ctx)
{
	if (g_pause) {
		g_pause = 0;
		printf("tnctl: pause\n");
		fflush(stdout);
		tn_pause(ctx);
	}
	if (g_resume) {
		g_resume = 0;
		printf("tnctl: resume\n");
		fflush(stdout);
		tn_resume(ctx);
	}
}

static const char *path_of(tn_ctx *ctx, char *buf, size_t cap, int32_t *lat)
{
	tn_status st;
	tn_peer_status ps[64];
	char ip[16];
	tn_get_status(ctx, &st, ps, 64);
	*lat = -1;
	for (uint32_t i = 0; i < st.n_peers && i < 64; i++) {
		if (ps[i].addr4 != g_target)
			continue;
		*lat = ps[i].latency_us;
		if (ps[i].path == TN_PATH_DIRECT)
			snprintf(buf, cap, "direct %s:%u", tn_ip4_str(ps[i].direct.ip, ip), ps[i].direct.port);
		else if (ps[i].path == TN_PATH_DERP)
			snprintf(buf, cap, "derp(%d)", ps[i].derp_region);
		else
			snprintf(buf, cap, "none");
		return buf;
	}
	snprintf(buf, cap, "no-peer");
	return buf;
}

static int run_probes(tn_ctx *ctx, int count, int interval_ms, int wait_direct_s)
{
	tn_status st;
	char pbuf[64], ip[16];
	int32_t lat;
	/* Wait until we have an address and the peer is in our netmap. */
	double t0 = now_ms();
	for (;;) {
		if (g_quit)
			return 1;
		tn_get_status(ctx, &st, NULL, 0);
		if (st.self_addr4 && strcmp(path_of(ctx, pbuf, sizeof pbuf, &lat), "no-peer"))
			break;
		if (now_ms() - t0 > 30000) {
			printf("probe: peer %s not in the netmap after 30 s\n", tn_ip4_str(g_target, ip));
			return 1;
		}
		usleep(100000);
	}
	const char *kind = g_target_port ? "udp-echo" : "ping";
	double min = 1e9, max = 0, sum = 0;
	int recv = 0, sent = 0;
	uint8_t pkt[128];
	size_t n;
	/* --wait-direct: warm up (uncounted probes) until the path is direct, so the
	 * numbers below are steady state; first contact is reported separately. */
	if (wait_direct_s) {
		double tw = now_ms();
		while (strncmp(path_of(ctx, pbuf, sizeof pbuf, &lat), "direct", 6) && now_ms() - tw < wait_direct_s * 1000.0 &&
		       !g_quit) {
			n = build_probe(pkt, st.self_addr4, 0);
			tn_send_ip(ctx, pkt, n);
			handle_signals(ctx);
			usleep(100000);
		}
		printf("%s: warm-up %.1f s, path %s\n", kind, (now_ms() - tw) / 1000.0, pbuf);
	}
	for (uint16_t seq = 1; seq <= count && !g_quit; seq++) {
		pthread_mutex_lock(&g_mu);
		g_got = 0;
		pthread_mutex_unlock(&g_mu);
		n = build_probe(pkt, st.self_addr4, seq);
		double ts = now_ms();
		int r = tn_send_ip(ctx, pkt, n);
		sent++;
		if (r < 0)
			printf("%s: send seq=%u: %s\n", kind, seq, tn_strerror(r));
		int ok = 0;
		while (now_ms() - ts < 3000 && !g_quit) {
			pthread_mutex_lock(&g_mu);
			ok = g_got && g_got_seq == seq;
			pthread_mutex_unlock(&g_mu);
			if (ok)
				break;
			handle_signals(ctx);
			usleep(200);
		}
		double rtt = now_ms() - ts;
		path_of(ctx, pbuf, sizeof pbuf, &lat);
		if (ok) {
			recv++;
			sum += rtt;
			min = rtt < min ? rtt : min;
			max = rtt > max ? rtt : max;
			printf("%s %s seq=%u time=%.2f ms path=%s\n", kind, tn_ip4_str(g_target, ip), seq, rtt, pbuf);
		} else {
			printf("%s %s seq=%u timeout path=%s\n", kind, tn_ip4_str(g_target, ip), seq, pbuf);
		}
		fflush(stdout);
		double left = interval_ms - (now_ms() - ts);
		while (left > 0 && !g_quit) {
			handle_signals(ctx);
			usleep(left > 50 ? 50000 : (useconds_t)(left * 1000));
			left = interval_ms - (now_ms() - ts);
		}
	}
	path_of(ctx, pbuf, sizeof pbuf, &lat);
	printf("%s summary: %d sent, %d received, min/avg/max %.2f/%.2f/%.2f ms, final path %s", kind, sent, recv,
	       recv ? min : 0.0, recv ? sum / recv : 0.0, recv ? max : 0.0, pbuf);
	if (lat >= 0)
		printf(", disco RTT %.2f ms", lat / 1000.0);
	printf("\n");
	fflush(stdout);
	return recv == sent ? 0 : 1;
}

static void on_signal(int s)
{
	(void)s;
	g_quit = 1;
}

static void on_usr(int s)
{
	if (s == SIGUSR1)
		g_pause = 1;
	else
		g_resume = 1;
}

static void hex8(const uint8_t *k, char out[17])
{
	static const char d[] = "0123456789abcdef";
	for (int i = 0; i < 8; i++) {
		out[2 * i] = d[k[i] >> 4];
		out[2 * i + 1] = d[k[i] & 15];
	}
	out[16] = 0;
}

static void print_netmap(const tn_netmap *nm)
{
	char ip[16], k[17], d[17];
	printf("netmap #%u: self %s %s home-derp %d%s\n", nm->updates, nm->self_name[0] ? nm->self_name : "?",
	       tn_ip4_str(nm->self_addr4, ip), nm->self_home_derp, nm->self_expired ? " EXPIRED" : "");
	printf("  peers %u, derp regions %u, filter rules %u%s\n", nm->n_peers, nm->n_regions, nm->n_rules,
	       nm->filter_known ? "" : " (no filter yet)");
	for (uint32_t i = 0; i < nm->n_peers; i++) {
		const tn_peer *p = &nm->peers[i];
		hex8(p->key, k);
		hex8(p->disco, d);
		printf("  peer %-28s %-15s id %lld nodekey:%s.. discokey:%s.. derp %d %s", p->name,
		       tn_ip4_str(p->addr4, ip), (long long)p->id, k, p->has_disco ? d : "-", p->home_derp,
		       p->online < 0 ? "online=?" : p->online ? "online" : "offline");
		for (uint8_t e = 0; e < p->n_ep; e++)
			printf("%s%s:%u", e ? "," : " ep ", tn_ip4_str(p->ep[e].ip, ip), p->ep[e].port);
		printf("\n");
	}
	for (uint32_t i = 0; i < nm->n_rules && i < 4; i++) {
		const tn_filter_rule *r = &nm->rules[i];
		char a[16], b[16];
		printf("  rule %u: %u src (first %s-%s) -> %u dst (first ports %u-%u)\n", i, r->n_src,
		       tn_ip4_str(r->src[0].lo, a), tn_ip4_str(r->src[0].hi, b), r->n_dst, r->dst[0].port_lo,
		       r->dst[0].port_hi);
	}
	fflush(stdout);
}

static void on_event(void *user, const tn_event *ev)
{
	(void)user;
	switch (ev->type) {
	case TN_EV_STATE:
		printf("state: %s\n", ev->text);
		break;
	case TN_EV_LOGIN_URL:
		printf("login URL: %s\n", ev->text);
		break;
	case TN_EV_NETMAP:
		print_netmap(ev->netmap);
		g_got_netmap = 1;
		if (g_once)
			g_quit = 1;
		break;
	case TN_EV_LOG:
		printf("log: %s\n", ev->text);
		break;
	case TN_EV_ERROR:
		printf("error: %s (%d)\n", ev->text, ev->err);
		break;
	case TN_EV_PATH:
		printf("path: %s\n", ev->text);
		break;
	case TN_EV_WARN:
		printf("warning: %s\n", ev->text);
		break;
	case TN_EV_NET_DOWN:
		printf("network down: %s\n", ev->text);
		break;
	case TN_EV_NET_UP:
		printf("network up: %s\n", ev->text);
		break;
	}
	fflush(stdout);
}

static int usage(void)
{
	fprintf(stderr, "usage: tnctl login --control URL --state DIR [--authkey KEY] [--hostname NAME]\n"
			"                   [--ca-file PEM] [--transport auto|plain|tls] [--once]\n"
			"                   [--timeout SEC] [--dump DIR] [-v|-vv] [--port UDP] [--no-direct]\n"
			"                   [--ping IP | --udp-echo IP:PORT] [--count N] [--interval MS]\n"
			"                   [--wait-direct SEC] [--serve tcp:PORT[:LOCAL],udp:PORT[:LOCAL]]\n"
			"                   [--map tcp:LOCAL:IP:PORT,udp:LOCAL:IP:PORT]\n");
	return 2;
}

int main(int argc, char **argv)
{
	tn_config cfg = {0};
	int timeout = 0, count = 5, interval = 1000, wait_direct = 0, probe = 0;
	static tn_serve serve[TN_SERVE_MAX];
	struct { uint8_t proto; uint16_t lport, rport; uint32_t rip; } maps[8];
	int n_maps = 0;
	if (argc < 2 || strcmp(argv[1], "login") != 0)
		return usage();
	for (int i = 2; i < argc; i++) {
		const char *a = argv[i];
		const char *v = i + 1 < argc ? argv[i + 1] : NULL;
		if (!strcmp(a, "--once")) {
			g_once = 1;
			continue;
		}
		if (!strcmp(a, "-v")) {
			cfg.verbose = 1;
			continue;
		}
		if (!strcmp(a, "-vv")) {
			cfg.verbose = 2;
			continue;
		}
		if (!strcmp(a, "--no-direct")) {
			cfg.no_direct = 1;
			continue;
		}
		if (!v)
			return usage();
		i++;
		if (!strcmp(a, "--control")) cfg.control_url = v;
		else if (!strcmp(a, "--state")) cfg.state_dir = v;
		else if (!strcmp(a, "--authkey")) cfg.auth_key = v;
		else if (!strcmp(a, "--hostname")) cfg.hostname = v;
		else if (!strcmp(a, "--ca-file")) cfg.ca_file = v;
		else if (!strcmp(a, "--dump")) cfg.dump_dir = v;
		else if (!strcmp(a, "--timeout")) timeout = atoi(v);
		else if (!strcmp(a, "--port")) cfg.udp_port = (uint16_t)atoi(v);
		else if (!strcmp(a, "--count")) count = atoi(v);
		else if (!strcmp(a, "--interval")) interval = atoi(v);
		else if (!strcmp(a, "--wait-direct")) wait_direct = atoi(v);
		else if (!strcmp(a, "--map")) {
			char spec[256];
			snprintf(spec, sizeof spec, "%s", v);
			for (char *tok = strtok(spec, ","); tok; tok = strtok(NULL, ",")) {
				char proto[8] = "", ip[32] = "";
				unsigned lport = 0, rport = 0;
				struct in_addr ia;
				if (n_maps >= 8 || sscanf(tok, "%7[a-z]:%u:%31[0-9.]:%u", proto, &lport, ip, &rport) != 4 ||
				    inet_pton(AF_INET, ip, &ia) != 1 || lport > 65535 || !rport || rport > 65535 ||
				    (strcmp(proto, "tcp") && strcmp(proto, "udp")))
					return usage();
				maps[n_maps].proto = !strcmp(proto, "tcp") ? 6 : 17;
				maps[n_maps].lport = (uint16_t)lport;
				maps[n_maps].rport = (uint16_t)rport;
				maps[n_maps].rip = ntohl(ia.s_addr);
				n_maps++;
			}
		}
		else if (!strcmp(a, "--serve")) {
			char spec[256];
			snprintf(spec, sizeof spec, "%s", v);
			for (char *tok = strtok(spec, ","); tok; tok = strtok(NULL, ",")) {
				unsigned port = 0, local = 0;
				char proto[8] = "";
				if (sscanf(tok, "%7[a-z]:%u:%u", proto, &port, &local) < 2 || !port || port > 65535 ||
				    local > 65535 || cfg.n_serve >= TN_SERVE_MAX || (strcmp(proto, "tcp") && strcmp(proto, "udp")))
					return usage();
				serve[cfg.n_serve].proto = !strcmp(proto, "tcp") ? 6 : 17;
				serve[cfg.n_serve].port = (uint16_t)port;
				serve[cfg.n_serve].local_port = (uint16_t)local;
				cfg.n_serve++;
			}
			cfg.serve = serve;
		}
		else if (!strcmp(a, "--ping") || !strcmp(a, "--udp-echo")) {
			char host[64];
			snprintf(host, sizeof host, "%s", v);
			char *colon = strchr(host, ':');
			if (colon) {
				*colon = 0;
				g_target_port = (uint16_t)atoi(colon + 1);
			}
			struct in_addr ia;
			if (inet_pton(AF_INET, host, &ia) != 1 || (!strcmp(a, "--udp-echo") && !g_target_port))
				return usage();
			g_target = ntohl(ia.s_addr);
			probe = 1;
		}
		else if (!strcmp(a, "--transport")) {
			if (!strcmp(v, "plain")) cfg.transport = TN_TRANSPORT_PLAIN;
			else if (!strcmp(v, "tls")) cfg.transport = TN_TRANSPORT_TLS;
			else if (strcmp(v, "auto")) return usage();
		} else
			return usage();
	}
	if (!cfg.control_url || !cfg.state_dir)
		return usage();
	cfg.cb = on_event;
	cfg.packet_cb = on_packet;

	tn_ctx *ctx;
	int r = tn_create(&cfg, &ctx);
	if (r < 0) {
		fprintf(stderr, "tnctl: %s\n", tn_strerror(r));
		return 1;
	}
	printf("tailnet: capver %d, %zu KiB allocated\n", TN_CAPVER, tn_memory_footprint(ctx) / 1024);
	signal(SIGINT, on_signal);
	signal(SIGTERM, on_signal);
	signal(SIGPIPE, SIG_IGN);
	signal(SIGUSR1, on_usr);
	signal(SIGUSR2, on_usr);
	if ((r = tn_start(ctx)) < 0) {
		fprintf(stderr, "tnctl: %s\n", tn_strerror(r));
		tn_destroy(ctx);
		return 1;
	}
	for (int i = 0; i < n_maps; i++) {
		uint16_t got = 0;
		char ip[16];
		int mr = tn_map(ctx, maps[i].proto, maps[i].rip, maps[i].rport, maps[i].lport, &got);
		printf("map: %s 127.0.0.1:%u -> %s:%u %s\n", maps[i].proto == 6 ? "tcp" : "udp", got,
		       tn_ip4_str(maps[i].rip, ip), maps[i].rport, mr == 0 ? "ok" : tn_strerror(mr));
		fflush(stdout);
	}
	if (probe) {
		r = run_probes(ctx, count, interval, wait_direct);
		tn_destroy(ctx);
		return r;
	}
	time_t start = time(NULL);
	while (!g_quit && (!timeout || time(NULL) - start < timeout)) {
		struct timespec ts = {0, 100 * 1000 * 1000};
		nanosleep(&ts, NULL);
		handle_signals(ctx);
	}
	tn_destroy(ctx);
	return g_got_netmap ? 0 : 1;
}
