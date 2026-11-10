/* The network going away (rest mode on the PS5, a laptop losing Wi-Fi): the errno class, the
 * resume/no-flap rules, and a real node that closes every socket on an injected errno 163 (UDP
 * receive, UDP send, a DERP read), opens nothing while down, and comes back by tn_resume or by
 * itself once the network is healthy. Host only (POSIX sockets in the test itself). */
#include "tt.h"
#include "tn.h"

#include <pthread.h>
#include <stdlib.h>
#include <unistd.h>

/* ---------------- pure rules ---------------- */

static void test_errno_class(void)
{
	CHECK(tn_errno_net_down(163));
	CHECK(tn_errno_is_sony_rest(163));
	CHECK(tn_errno_net_down(ENETDOWN));
	CHECK(tn_errno_net_down(ENETUNREACH));
	CHECK(tn_errno_net_down(EHOSTDOWN));
	CHECK(tn_errno_net_down(EADDRNOTAVAIL));
	CHECK(!tn_errno_is_sony_rest(ENETDOWN));
	/* one absent LAN host (a peer's stale endpoint) is not the network going away */
	CHECK(!tn_errno_net_down(EHOSTUNREACH));
	CHECK(!tn_errno_net_down(ECONNREFUSED));
	CHECK(!tn_errno_net_down(ECONNRESET));
	CHECK(!tn_errno_net_down(EAGAIN));
	CHECK(!tn_errno_net_down(EINTR));
	CHECK(!tn_errno_net_down(EMFILE));
	CHECK(!tn_errno_net_down(ENOBUFS));
	CHECK(!tn_errno_net_down(EBADF));
	CHECK(!tn_errno_net_down(ETIMEDOUT));
	CHECK(!tn_errno_net_down(0));
	CHECK(tn_errno_refused(ECONNREFUSED) && !tn_errno_refused(163));
	/* control results that count toward a streak */
	CHECK(tn_ctl_conn_err(TN_E_IO, 0));
	CHECK(tn_ctl_conn_err(TN_E_IO, 163));
	CHECK(tn_ctl_conn_err(TN_E_TIMEOUT, 0));
	CHECK(tn_ctl_conn_err(TN_E_DNS, 0));
	CHECK(!tn_ctl_conn_err(TN_E_IO, ECONNREFUSED)); /* the server answered */
	CHECK(!tn_ctl_conn_err(TN_E_HTTP, 0));
	CHECK(!tn_ctl_conn_err(TN_E_AUTH, 0));
	CHECK(!tn_ctl_conn_err(TN_E_TLS, 0));
	/* this machine's network works: the probe is healthy, with and without a route lookup */
	CHECK(tn_net_probe(0) == 0);
	CHECK(tn_net_probe(0x7f000001u) == 0);
}

static void test_resume_rule(void)
{
	tn_susp s;
	tn_susp_init(&s, 0);
	CHECK(s.base_ms == TN_RESUME_BASE_MS);
	tn_susp_on_down(&s, 1000);
	CHECK(tn_susp_need_ms(&s) == 30000);
	CHECK(!tn_susp_on_probe(&s, 1, 2000));   /* healthy from 2 s */
	CHECK(!tn_susp_on_probe(&s, 1, 31000));  /* 29 s healthy */
	CHECK(!tn_susp_on_probe(&s, 0, 31500));  /* a failure starts the count over */
	CHECK(!tn_susp_on_probe(&s, 1, 32000));
	CHECK(!tn_susp_on_probe(&s, 1, 61999));
	CHECK(tn_susp_on_probe(&s, 1, 62000));   /* 30 s healthy in a row */
	/* down again soon after: twice as long, then four times, ... capped at 5 min */
	tn_susp_on_down(&s, 70000);
	CHECK(s.recent == 2 && tn_susp_need_ms(&s) == 60000);
	CHECK(!tn_susp_on_probe(&s, 1, 70000));
	CHECK(!tn_susp_on_probe(&s, 1, 129999));
	CHECK(tn_susp_on_probe(&s, 1, 130000));
	tn_susp_on_down(&s, 140000);
	CHECK(tn_susp_need_ms(&s) == 120000);
	tn_susp_on_down(&s, 150000);
	CHECK(tn_susp_need_ms(&s) == 240000);
	tn_susp_on_down(&s, 160000);
	CHECK(tn_susp_need_ms(&s) == TN_RESUME_MAX_MS);
	for (int i = 0; i < 40; i++)
		tn_susp_on_down(&s, 170000 + i);
	CHECK(tn_susp_need_ms(&s) == TN_RESUME_MAX_MS); /* never past the cap, no overflow */
	/* quiet for the flap window: the next one is a first one again */
	tn_susp_on_down(&s, 170040 + TN_FLAP_WINDOW_MS);
	CHECK(s.recent == 1 && tn_susp_need_ms(&s) == 30000);
	/* a test base */
	tn_susp_init(&s, 400);
	tn_susp_on_down(&s, 0);
	CHECK(tn_susp_need_ms(&s) == 400);
	tn_susp_on_down(&s, 10);
	CHECK(tn_susp_need_ms(&s) == 800);
}

static void test_streak_rule(void)
{
	tn_ctl_streak k = {0, 0};
	CHECK(!tn_ctl_streak_note(&k, 1, 1000));  /* the session dropped */
	CHECK(tn_ctl_streak_note(&k, 1, 2500));   /* the reconnect failed 1.5 s later: a streak */
	CHECK(!tn_ctl_streak_note(&k, 1, 3000));  /* counted once, starts over */
	CHECK(!tn_ctl_streak_note(&k, 1, 14000)); /* 11 s apart: not a streak (backoff at 8 s and up) */
	CHECK(!tn_ctl_streak_note(&k, 0, 15000)); /* another error kind in between breaks it */
	CHECK(!tn_ctl_streak_note(&k, 1, 16000));
	CHECK(tn_ctl_streak_note(&k, 1, 17000));
	/* a steady failure with the backoff at its cap never is one */
	tn_ctl_streak q = {0, 0};
	int any = 0;
	for (int i = 0; i < 20; i++)
		any |= tn_ctl_streak_note(&q, 1, 100000 + i * 30000);
	CHECK(!any);
}

/* ---------------- a real node ---------------- */

static pthread_mutex_t ev_mu = PTHREAD_MUTEX_INITIALIZER;
static int ev_down, ev_up, ev_down_err;
static int64_t ev_down_at, ev_up_at;
static char ev_down_text[200];

static void on_event(void *user, const tn_event *ev)
{
	(void)user;
	pthread_mutex_lock(&ev_mu);
	if (ev->type == TN_EV_NET_DOWN) {
		ev_down++;
		ev_down_err = ev->err;
		ev_down_at = tn_now_ms();
		snprintf(ev_down_text, sizeof ev_down_text, "%s", ev->text ? ev->text : "");
	} else if (ev->type == TN_EV_NET_UP) {
		ev_up++;
		ev_up_at = tn_now_ms();
	}
	pthread_mutex_unlock(&ev_mu);
	if (getenv("TN_TEST_VERBOSE") && ev->text)
		fprintf(stderr, "  [ev %d] %s\n", ev->type, ev->text);
}

static int counts(int *down, int *up)
{
	pthread_mutex_lock(&ev_mu);
	*down = ev_down;
	*up = ev_up;
	pthread_mutex_unlock(&ev_mu);
	return 0;
}

/* Wait up to ms for cond. */
#define WAIT_FOR(cond, ms)                                                                 \
	do {                                                                               \
		int64_t _end = tn_now_ms() + (ms);                                         \
		while (!(cond) && tn_now_ms() < _end)                                      \
			tn_sleep_ms(10);                                                   \
	} while (0)

static uint16_t local_port_of(int fd)
{
	struct sockaddr_in a;
	socklen_t l = sizeof a;
	memset(&a, 0, sizeof a);
	getsockname(fd, (struct sockaddr *)&a, &l);
	return ntohs(a.sin_port);
}

static int raw_sock(int type, uint16_t *port)
{
	int fd = socket(AF_INET, type, 0);
	struct sockaddr_in a;
	memset(&a, 0, sizeof a);
	a.sin_family = AF_INET;
	a.sin_addr.s_addr = htonl(0x7f000001u);
	if (fd < 0 || bind(fd, (struct sockaddr *)&a, sizeof a) != 0)
		return -1;
	if (type == SOCK_STREAM)
		listen(fd, 4);
	*port = local_port_of(fd);
	return fd;
}

static void send_udp(uint16_t port)
{
	int fd = socket(AF_INET, SOCK_DGRAM, 0);
	struct sockaddr_in a;
	memset(&a, 0, sizeof a);
	a.sin_family = AF_INET;
	a.sin_addr.s_addr = htonl(0x7f000001u);
	a.sin_port = htons(port);
	sendto(fd, "x", 1, 0, (struct sockaddr *)&a, sizeof a);
	close(fd);
}

static void feed_netmap(tn_ctx *c, uint16_t stun_port, uint16_t derp_port)
{
	static char js[1024];
	static tn_jtok toks[256];
	snprintf(js, sizeof js,
		 "{\"DERPMap\":{\"Regions\":{\"901\":{\"RegionID\":901,\"RegionCode\":\"t\",\"RegionName\":\"test\","
		 "\"Nodes\":[{\"Name\":\"901a\",\"RegionID\":901,\"HostName\":\"localhost\",\"IPv4\":\"127.0.0.1\","
		 "\"STUNPort\":%d,\"DERPPort\":%u,\"InsecureForTests\":true}]}}}}",
		 stun_port ? (int)stun_port : -1, derp_port);
	tn_json j = {.t = toks, .cap = 256};
	CHECK(tn_json_parse(&j, js, strlen(js)) > 0);
	tn_map_result res;
	tn_mutex_lock(&c->mu);
	CHECK(tn_netmap_apply(&c->nms, &j, &res) == TN_OK);
	tn_mutex_unlock(&c->mu);
	tn_atomic_inc(&c->nm_gen);
	tn_dp_wake(c);
}

static void test_node(void)
{
	char dir[] = "/tmp/tn-netdown-XXXXXX";
	CHECK(mkdtemp(dir) != NULL);
	/* control: a closed port (connection refused at once, so the control thread backs off) */
	uint16_t cport = 0;
	int cfd = raw_sock(SOCK_STREAM, &cport);
	close(cfd);
	char url[64];
	snprintf(url, sizeof url, "http://127.0.0.1:%u", cport);
	tn_config cfg;
	memset(&cfg, 0, sizeof cfg);
	cfg.state_dir = dir;
	cfg.control_url = url;
	cfg.hostname = "netdown-test";
	cfg.cb = on_event;
	cfg.verbose = 1;
	tn_ctx *c = NULL;
	CHECK(tn_create(&cfg, &c) == TN_OK);
	if (!c)
		return;
	CHECK(tn_start(c) == TN_OK);
	tn_status st;

	/* 1. errno 163 on a UDP receive, no self-resume (the PS5 helper's policy) */
	tn_set_net_policy(c, 0);
	WAIT_FOR((tn_get_status(c, &st, NULL, 0), st.udp_port != 0), 3000);
	CHECK(st.udp_port != 0);
	CHECK(tn_test_sock_live() >= 1);
	tn_test_sock_fault(TN_FAULT_RECV, 163);
	send_udp(st.udp_port);
	int down = 0, up = 0;
	WAIT_FOR((counts(&down, &up), down == 1 && tn_test_sock_live() == 0), 3000);
	CHECK(tn_net_down(c));
	CHECK(down == 1);
	CHECK(tn_test_sock_live() == 0); /* UDP, control: every socket closed */
	CHECK(ev_down_err == 163 && strstr(ev_down_text, "udp receive") != NULL);
	tn_get_status(c, &st, NULL, 0);
	CHECK(st.state == TN_ST_PAUSED);
	/* nothing opens while down, even with the fault gone (no self-resume here) */
	tn_test_sock_fault(0, 0);
	long opens = tn_test_sock_opens();
	tn_sleep_ms(1500);
	CHECK(tn_test_sock_opens() == opens);
	CHECK(tn_test_sock_live() == 0);
	CHECK(tn_net_down(c));
	counts(&down, &up);
	CHECK(down == 1 && up == 0);
	tn_test_net_down(c, 163); /* a second signal while down: same episode */
	tn_sleep_ms(100);
	counts(&down, &up);
	CHECK(down == 1);
	/* the app (the helper's rest/wake tracking) resumes it: UDP comes back */
	tn_resume(c);
	WAIT_FOR((counts(&down, &up), up == 1 && tn_test_sock_live() >= 1), 3000);
	CHECK(!tn_net_down(c));
	CHECK(up == 1);
	CHECK(tn_test_sock_live() >= 1);

	/* 2. errno 163 on a UDP send (netcheck's STUN), self-resume on with a short healthy period */
	tn_set_net_policy(c, TN_NETPOL_SELF_RESUME);
	tn_test_resume_ms(c, 300);
	uint16_t sport = 0, dport = 0;
	int sfd = raw_sock(SOCK_DGRAM, &sport); /* the "STUN server": never answers */
	int lfd = raw_sock(SOCK_STREAM, &dport); /* the "DERP server" */
	CHECK(sfd >= 0 && lfd >= 0);
	tn_test_sock_fault(TN_FAULT_SEND, 163);
	feed_netmap(c, sport, dport);
	WAIT_FOR((counts(&down, &up), down == 2 && tn_test_sock_live() == 0), 3000);
	CHECK(down == 2);
	CHECK(tn_test_sock_live() == 0);
	CHECK(strstr(ev_down_text, "stun") != NULL);
	int64_t t_down = ev_down_at;
	tn_test_sock_fault(0, 0);
	/* second episode in a row: 2 x 300 ms healthy before it resumes by itself */
	WAIT_FOR((counts(&down, &up), up == 2), 6000);
	CHECK(up == 2);
	CHECK(ev_up_at - t_down >= 600);
	CHECK(!tn_net_down(c));
	WAIT_FOR(tn_test_sock_live() >= 1, 2000);
	CHECK(tn_test_sock_live() >= 1);

	/* 3. errno 163 on a DERP read: the node dials the region's DERP once netcheck gives up on
	 * STUN (3 s); the "server" sends bytes, the read fails, everything closes */
	struct pollfd p = {.fd = lfd, .events = POLLIN};
	CHECK(poll(&p, 1, 8000) == 1);
	int afd = accept(lfd, NULL, NULL);
	CHECK(afd >= 0);
	tn_test_sock_fault(TN_FAULT_RECV, 163);
	CHECK(afd >= 0 && write(afd, "\x16\x03\x03\x00\x02\x02\x28", 7) == 7); /* bytes to read */
	WAIT_FOR((counts(&down, &up), down == 3 && tn_test_sock_live() == 0), 4000);
	CHECK(down == 3);
	CHECK(tn_test_sock_live() == 0);
	CHECK(strstr(ev_down_text, "derp") != NULL);
	t_down = ev_down_at;
	tn_test_sock_fault(0, 0);
	/* third in a row: 4 x 300 ms */
	WAIT_FOR((counts(&down, &up), up == 3), 8000);
	CHECK(up == 3);
	CHECK(ev_up_at - t_down >= 1200);
	if (afd >= 0)
		close(afd);

	/* 4. a pause holds a self-resume back; tn_resume ends both */
	tn_test_net_down(c, ENETDOWN);
	WAIT_FOR((counts(&down, &up), down == 4 && tn_test_sock_live() == 0), 3000);
	tn_pause(c);
	tn_sleep_ms(3500); /* past the 8 x 300 ms a fourth one needs */
	CHECK(tn_test_sock_live() == 0);
	tn_resume(c);
	WAIT_FOR((counts(&down, &up), up == 4 && tn_test_sock_live() >= 1), 3000);
	CHECK(up == 4 && !tn_net_down(c));

	tn_destroy(c);
	CHECK(tn_test_sock_live() == 0); /* nothing leaked */
	close(sfd);
	close(lfd);
	char path[128];
	snprintf(path, sizeof path, "%s/tailnet-state.json", dir);
	unlink(path);
	rmdir(dir);
}

/* A "control server" that accepts and hangs up at once: every control attempt ends in a connection
 * error with no errno (EOF), the way the PS5's did in the seconds before it froze for rest. */
static volatile int hangup_stop;
static void *hangup_server(void *arg)
{
	int lfd = *(int *)arg;
	while (!hangup_stop) {
		struct pollfd p = {.fd = lfd, .events = POLLIN};
		if (poll(&p, 1, 50) == 1) {
			int a = accept(lfd, NULL, NULL);
			if (a >= 0)
				close(a);
		}
	}
	return NULL;
}

static void test_control_streak(void)
{
	char dir[] = "/tmp/tn-streak-XXXXXX";
	CHECK(mkdtemp(dir) != NULL);
	uint16_t cport = 0;
	int lfd = raw_sock(SOCK_STREAM, &cport);
	CHECK(lfd >= 0);
	pthread_t th;
	hangup_stop = 0;
	pthread_create(&th, NULL, hangup_server, &lfd);
	char url[64];
	snprintf(url, sizeof url, "http://127.0.0.1:%u", cport);
	tn_config cfg;
	memset(&cfg, 0, sizeof cfg);
	cfg.state_dir = dir;
	cfg.control_url = url;
	cfg.cb = on_event;
	tn_ctx *c = NULL;
	CHECK(tn_create(&cfg, &c) == TN_OK);
	if (!c)
		return;
	int down0 = 0, up0 = 0, down = 0, up = 0;
	counts(&down0, &up0);
	/* default policy (a desktop): a streak is not a down signal */
	CHECK(tn_start(c) == TN_OK);
	tn_sleep_ms(2500); /* errors at ~0 s and ~1 s */
	counts(&down, &up);
	CHECK(down == down0 && !tn_net_down(c));
	/* the PS5 helper's policy: the next two errors close everything */
	tn_set_net_policy(c, TN_NETPOL_CONTROL_STREAK);
	tn_resume(c); /* backoff from 1 s again */
	WAIT_FOR((counts(&down, &up), down == down0 + 1), 6000);
	CHECK(down == down0 + 1 && tn_net_down(c));
	CHECK(strstr(ev_down_text, "back to back") != NULL); /* errno 0 (EOF) or ECONNRESET */
	WAIT_FOR(tn_test_sock_live() == 0, 2000);
	CHECK(tn_test_sock_live() == 0);
	tn_destroy(c);
	hangup_stop = 1;
	pthread_join(th, NULL);
	close(lfd);
	char path[128];
	snprintf(path, sizeof path, "%s/tailnet-state.json", dir);
	unlink(path);
	rmdir(dir);
}

int main(void)
{
	test_errno_class();
	test_resume_rule();
	test_streak_rule();
	test_node();
	test_control_streak();
	return tt_done("test_netdown");
}
