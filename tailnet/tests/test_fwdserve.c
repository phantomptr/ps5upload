/* A serving node's TCP splice with and without a PROXY protocol v1 line (TN_SERVE_PROXY_V1), and
 * what a serving node lets in: its served ports, and the replies on its own maps (TCP and UDP),
 * nothing else. Single-threaded: the node is created, never started; the test drives the
 * forwarder by hand and plays the peer by building its packets. lwIP is global, so the nodes are
 * made one after another. */
#include "tn.h"
#include "tt.h"

#include "lwip/priv/tcp_priv.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <sys/socket.h>
#include <unistd.h>

#define SELF 0x64400001u /* 100.64.0.1 */
#define PEER 0x64400002u /* 100.64.0.2 */
#define OTHER 0x64400003u

static char g_dir[64];

static tn_ctx *node(const tn_serve *serve, uint32_t n)
{
	snprintf(g_dir, sizeof g_dir, "/tmp/tn-fwdserve-XXXXXX");
	CHECK(mkdtemp(g_dir) != NULL);
	tn_config cfg;
	memset(&cfg, 0, sizeof cfg);
	cfg.state_dir = g_dir;
	cfg.control_url = "http://127.0.0.1:9";
	cfg.hostname = "fwdserve-test";
	cfg.serve = serve;
	cfg.n_serve = n;
	tn_ctx *c = NULL;
	CHECK(tn_create(&cfg, &c) == TN_OK);
	if (c)
		tn_fwd_set_addr(c, SELF);
	return c;
}

static void node_done(tn_ctx *c)
{
	tn_destroy(c);
	char cmd[96];
	snprintf(cmd, sizeof cmd, "rm -rf %s", g_dir);
	CHECK(system(cmd) == 0);
}

/* A listener on 127.0.0.1, any port. */
static int listener(uint16_t *port)
{
	int fd = socket(AF_INET, SOCK_STREAM, 0);
	struct sockaddr_in a;
	memset(&a, 0, sizeof a);
	a.sin_family = AF_INET;
	a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
	CHECK(bind(fd, (struct sockaddr *)&a, sizeof a) == 0);
	CHECK(listen(fd, 4) == 0);
	socklen_t l = sizeof a;
	CHECK(getsockname(fd, (struct sockaddr *)&a, &l) == 0);
	*port = ntohs(a.sin_port);
	return fd;
}

/* An IPv4/TCP segment from the peer to us, checksums filled in. Returns its length. */
static size_t tcp_pkt(uint8_t *pkt, uint32_t src, uint16_t sport, uint16_t dport, uint32_t seq, uint32_t ack,
		      uint8_t flags, const char *data)
{
	size_t dl = data ? strlen(data) : 0, n = 40 + dl;
	memset(pkt, 0, n);
	pkt[0] = 0x45;
	tn_put_be16(pkt + 2, (uint16_t)n);
	pkt[8] = 64;
	pkt[9] = TN_IPPROTO_TCP;
	tn_put_be32(pkt + 12, src);
	tn_put_be32(pkt + 16, SELF);
	tn_put_be16(pkt + 10, tn_ip_csum(pkt, 20));
	uint8_t *h = pkt + 20;
	tn_put_be16(h, sport);
	tn_put_be16(h + 2, dport);
	tn_put_be32(h + 4, seq);
	tn_put_be32(h + 8, ack);
	h[12] = 5 << 4;
	h[13] = flags;
	tn_put_be16(h + 14, 65535);
	if (dl)
		memcpy(h + 20, data, dl);
	tn_put_be16(h + 16, tn_l4_csum(src, SELF, TN_IPPROTO_TCP, h, 20 + dl));
	return n;
}

static void feed(tn_ctx *c, const uint8_t *pkt, size_t n)
{
	tn_ip_info ip;
	CHECK(tn_ip_parse(pkt, n, &ip) == 0);
	tn_fwd_input(c, pkt, &ip, tn_now_ms());
}

/* Run the forwarder's sockets for up to `ms`: what poll says goes to tn_fwd_io. */
static void pump(tn_ctx *c, int ms)
{
	for (int t = 0; t < ms; t += 10) {
		struct pollfd pf[64];
		int n = tn_fwd_pollfds(c, pf, 64);
		if (n > 0 && poll(pf, (nfds_t)n, 10) > 0) {
			for (int i = 0; i < n; i++)
				if (pf[i].revents)
					tn_fwd_io(c, pf[i].fd, pf[i].revents, tn_now_ms());
		} else if (n == 0) {
			usleep(10000);
		}
		tn_fwd_tick(c, tn_now_ms());
	}
}

static int accept_within(int lfd, int ms)
{
	struct pollfd p = {.fd = lfd, .events = POLLIN};
	if (poll(&p, 1, ms) != 1)
		return -1;
	return accept(lfd, NULL, NULL);
}

/* Everything readable on fd within ms, NUL-terminated. */
static size_t read_all(int fd, char *out, size_t cap, int ms)
{
	size_t got = 0;
	fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK);
	for (int t = 0; t < ms && got + 1 < cap; t += 10) {
		struct pollfd p = {.fd = fd, .events = POLLIN};
		if (poll(&p, 1, 10) != 1)
			continue;
		ssize_t n = read(fd, out + got, cap - 1 - got);
		if (n <= 0)
			break;
		got += (size_t)n;
	}
	out[got] = 0;
	return got;
}

/* The pcb lwIP made for the peer's connection (SYN_RCVD after the SYN). */
static struct tcp_pcb *pcb_for(uint16_t rport)
{
	for (struct tcp_pcb *p = tcp_active_pcbs; p; p = p->next)
		if (p->remote_port == rport)
			return p;
	return NULL;
}

/* A peer opens 100.64.0.1:9120, the forwarder dials the local listener, the handshake completes
 * and the peer sends "hello": what the local server reads, in order. */
static void splice(uint8_t flags, const char *want)
{
	uint16_t lport = 0;
	int lfd = listener(&lport);
	tn_serve serve[] = {{6, 9120, lport, flags}};
	tn_ctx *c = node(serve, 1);
	if (!c)
		return;
	uint8_t pkt[128];
	feed(c, pkt, tcp_pkt(pkt, PEER, 40000, 9120, 1000, 0, 0x02, NULL)); /* SYN */
	struct tcp_pcb *p = NULL;
	for (int i = 0; i < 100 && !(p = pcb_for(40000)); i++)
		pump(c, 10); /* the local dial finishes, then lwIP takes the SYN */
	CHECK(p != NULL);
	int fd = accept_within(lfd, 1000);
	CHECK(fd >= 0);
	if (p) {
		/* the peer's ACK of the SYN-ACK, with its first bytes */
		feed(c, pkt, tcp_pkt(pkt, PEER, 40000, 9120, 1001, p->snd_nxt, 0x18, "hello"));
		pump(c, 50);
	}
	char got[256];
	read_all(fd, got, sizeof got, 300);
	CHECK(strcmp(got, want) == 0);
	if (strcmp(got, want) != 0)
		fprintf(stderr, "  got [%s]\n  want [%s]\n", got, want);
	close(fd);
	close(lfd);
	node_done(c);
}

static void test_proxy_line_first(void)
{
	splice(TN_SERVE_PROXY_V1, "PROXY TCP4 100.64.0.2 100.64.0.1 40000 9120\r\nhello");
}

static void test_no_line_without_the_flag(void)
{
	splice(0, "hello");
}

/* The line cannot be written: the dial counts as failed, lwIP never sees the SYN (the peer gets a
 * RST), and the local server never gets a byte. */
static void test_line_not_written_fails_the_dial(void)
{
	uint16_t lport = 0;
	int lfd = listener(&lport);
	tn_serve serve[] = {{6, 9120, lport, TN_SERVE_PROXY_V1}};
	tn_ctx *c = node(serve, 1);
	if (!c)
		return;
	uint8_t pkt[128];
	tn_test_sock_fault(TN_FAULT_SEND, ECONNRESET);
	feed(c, pkt, tcp_pkt(pkt, PEER, 40001, 9120, 1000, 0, 0x02, NULL));
	pump(c, 200);
	tn_test_sock_fault(0, 0);
	CHECK(pcb_for(40001) == NULL);
	struct pollfd pf[8];
	CHECK(tn_fwd_pollfds(c, pf, 8) == 0); /* no SYN waiting, no connection */
	int fd = accept_within(lfd, 200);
	if (fd >= 0) {
		char got[64];
		CHECK(read_all(fd, got, sizeof got, 100) == 0);
		close(fd);
	}
	close(lfd);
	node_done(c);
}

static tn_ip_info info(uint8_t proto, uint32_t src, uint16_t sport, uint16_t dport, uint8_t tcp_flags)
{
	tn_ip_info ip;
	memset(&ip, 0, sizeof ip);
	ip.proto = proto;
	ip.src = src;
	ip.sport = sport;
	ip.dst = SELF;
	ip.dport = dport;
	ip.tcp_flags = tcp_flags;
	return ip;
}

/* A node that serves a TCP port (the engine's guest port) and maps a console's UDP ports (Remote
 * Play, discovery): the replies on those maps get in, and nothing else that is not served. */
static void test_serving_node_takes_its_udp_map_replies(void)
{
	tn_serve serve[] = {{6, 19113, 40123, TN_SERVE_PROXY_V1}};
	tn_ctx *c = node(serve, 1);
	if (!c)
		return;
	uint16_t lport = 0;
	CHECK(tn_fwd_map(c, TN_IPPROTO_UDP, PEER, 9296, 0, &lport) == TN_OK && lport != 0);
	/* the map's own tailnet port (internal: found by asking) */
	uint16_t tport = 0;
	for (uint32_t p = 1; p < 65536 && !tport; p++) {
		tn_ip_info ip = info(TN_IPPROTO_UDP, PEER, 9296, (uint16_t)p, 0);
		if (tn_fwd_udp_mapped(c, &ip))
			tport = (uint16_t)p;
	}
	CHECK(tport != 0);
	tn_ip_info ip = info(TN_IPPROTO_UDP, PEER, 9296, tport, 0);
	CHECK(tn_dp_inbound_ok(c, &ip) == 1); /* the reply */
	ip = info(TN_IPPROTO_UDP, PEER, 9297, tport, 0); /* another port of the peer */
	CHECK(tn_dp_inbound_ok(c, &ip) == 0);
	ip = info(TN_IPPROTO_UDP, OTHER, 9296, tport, 0); /* another peer */
	CHECK(tn_dp_inbound_ok(c, &ip) == 0);
	ip = info(TN_IPPROTO_UDP, PEER, 9296, (uint16_t)(tport + 1), 0); /* another of our ports */
	CHECK(tn_dp_inbound_ok(c, &ip) == 0);
	ip = info(TN_IPPROTO_UDP, PEER, 9296, 5555, 0); /* an unlisted UDP port */
	CHECK(tn_dp_inbound_ok(c, &ip) == 0);
	ip = info(TN_IPPROTO_TCP, PEER, 40000, 8080, 0x02); /* an unlisted TCP port, a SYN */
	CHECK(tn_dp_inbound_ok(c, &ip) == 0);
	ip = info(TN_IPPROTO_TCP, PEER, 40000, 8080, 0x10); /* ... and an ACK lwIP does not know */
	CHECK(tn_dp_inbound_ok(c, &ip) == 0);
	ip = info(TN_IPPROTO_TCP, PEER, 9296, tport, 0x10); /* the UDP map's tuple, as TCP */
	CHECK(tn_dp_inbound_ok(c, &ip) == 0);
	ip = info(TN_IPPROTO_UDP, PEER, 40000, 19113, 0); /* the served port is TCP only */
	CHECK(tn_dp_inbound_ok(c, &ip) == 0);
	/* the served port: a new connection needs the tailnet's filter (none from control yet) */
	ip = info(TN_IPPROTO_TCP, PEER, 40000, 19113, 0x02);
	CHECK(tn_dp_inbound_ok(c, &ip) == 0);
	ip = info(TN_IPPROTO_TCP, PEER, 40000, 19113, 0x10); /* segments of an open one pass */
	CHECK(tn_dp_inbound_ok(c, &ip) == 1);
	/* unmapped: its replies are dropped again */
	tn_fwd_unmap(c, 0, 0);
	ip = info(TN_IPPROTO_UDP, PEER, 9296, tport, 0);
	CHECK(tn_dp_inbound_ok(c, &ip) == 0);
	node_done(c);
}

/* A client node (serves nothing) lets everything through to its own handling, as before. */
static void test_client_node_unchanged(void)
{
	tn_ctx *c = node(NULL, 0);
	if (!c)
		return;
	tn_ip_info ip = info(TN_IPPROTO_UDP, PEER, 9296, 5555, 0);
	CHECK(tn_dp_inbound_ok(c, &ip) == 1);
	ip = info(TN_IPPROTO_TCP, PEER, 40000, 8080, 0x02);
	CHECK(tn_dp_inbound_ok(c, &ip) == 1);
	node_done(c);
}

int main(void)
{
	CHECK(tn_abi_size(4) == sizeof(tn_serve));
	test_proxy_line_first();
	test_no_line_without_the_flag();
	test_line_not_written_fails_the_dial();
	test_serving_node_takes_its_udp_map_replies();
	test_client_node_unchanged();
	return tt_done("test_fwdserve");
}
