/* A node that serves ports AND maps one (the PS5 helper's forward for PS5Upload Play): the
 * replies on its own mapped TCP connection are let in (tn_fwd_tcp_known), and nothing else on a
 * port it does not serve is. Single-threaded: the node is created, never started; the test drives
 * the forwarder by hand. */
#include "tn.h"
#include "tt.h"

#include "lwip/priv/tcp_priv.h"

#include <arpa/inet.h>
#include <netinet/in.h>
#include <poll.h>
#include <sys/socket.h>
#include <unistd.h>

#define SELF 0x64400001u /* 100.64.0.1 */
#define PEER 0x64400002u /* 100.64.0.2 */

static const tn_serve SERVE[] = {{6, 9120, 0}};

static tn_ip_info tcp_seg(uint32_t src, uint16_t sport, uint32_t dst, uint16_t dport)
{
	tn_ip_info ip;
	memset(&ip, 0, sizeof ip);
	ip.proto = TN_IPPROTO_TCP;
	ip.tcp_flags = 0x12; /* SYN-ACK */
	ip.src = src;
	ip.sport = sport;
	ip.dst = dst;
	ip.dport = dport;
	return ip;
}

static void test_serving_node_takes_its_own_replies(void)
{
	char dir[] = "/tmp/tn-fwdmap-XXXXXX";
	CHECK(mkdtemp(dir) != NULL);
	tn_config cfg;
	memset(&cfg, 0, sizeof cfg);
	cfg.state_dir = dir;
	cfg.control_url = "http://127.0.0.1:9";
	cfg.hostname = "fwdmap-test";
	cfg.serve = SERVE;
	cfg.n_serve = 1;
	tn_ctx *c = NULL;
	CHECK(tn_create(&cfg, &c) == TN_OK);
	if (!c)
		return;
	tn_fwd_set_addr(c, SELF);
	uint16_t lport = 0;
	CHECK(tn_fwd_map(c, TN_IPPROTO_TCP, PEER, 8080, 0, &lport) == TN_OK && lport != 0);
	/* the app connects to the local port; the forwarder accepts and dials the peer in lwIP */
	int fd = socket(AF_INET, SOCK_STREAM, 0);
	struct sockaddr_in a;
	memset(&a, 0, sizeof a);
	a.sin_family = AF_INET;
	a.sin_port = htons(lport);
	a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
	CHECK(connect(fd, (struct sockaddr *)&a, sizeof a) == 0);
	struct pollfd pf[64];
	int n = tn_fwd_pollfds(c, pf, 64);
	for (int i = 0; i < n; i++)
		tn_fwd_io(c, pf[i].fd, POLLIN, tn_now_ms());
	/* the dial's pcb: SYN_SENT to the peer's 8080 from an ephemeral port */
	uint16_t eph = 0;
	for (struct tcp_pcb *p = tcp_active_pcbs; p; p = p->next)
		if (p->remote_port == 8080)
			eph = p->local_port;
	CHECK(eph != 0);
	tn_ip_info ip = tcp_seg(PEER, 8080, SELF, eph);
	CHECK(tn_fwd_tcp_known(c, &ip) == 1);
	ip = tcp_seg(PEER, 8081, SELF, eph); /* another port of the peer */
	CHECK(tn_fwd_tcp_known(c, &ip) == 0);
	ip = tcp_seg(0x64400003u, 8080, SELF, eph); /* another peer */
	CHECK(tn_fwd_tcp_known(c, &ip) == 0);
	ip = tcp_seg(PEER, 8080, SELF, (uint16_t)(eph + 1)); /* another of our ports */
	CHECK(tn_fwd_tcp_known(c, &ip) == 0);
	ip = tcp_seg(PEER, 8080, 0x64400009u, eph); /* not our address */
	CHECK(tn_fwd_tcp_known(c, &ip) == 0);
	ip = tcp_seg(PEER, 40000, SELF, 9120); /* a served port's listener is never "known" */
	CHECK(tn_fwd_tcp_known(c, &ip) == 0);
	ip = tcp_seg(PEER, 8080, SELF, eph);
	ip.proto = TN_IPPROTO_UDP;
	CHECK(tn_fwd_tcp_known(c, &ip) == 0);
	/* unmapped: the connection is gone with it */
	tn_fwd_unmap(c, 0, 0);
	close(fd);
	tn_destroy(c);
	char cmd[64];
	snprintf(cmd, sizeof cmd, "rm -rf %s", dir);
	CHECK(system(cmd) == 0);
}

int main(void)
{
	test_serving_node_takes_its_own_replies();
	return tt_done("test_fwdmap");
}
