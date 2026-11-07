/* tnctl: drive the tailnet library from a terminal.
 *
 *   tnctl login --control URL --state DIR [--authkey K] [--hostname H] [--ca-file PEM]
 *               [--transport auto|plain|tls] [--once] [--timeout SEC] [--dump DIR] [-v|-vv]
 *
 * Prints the login URL (interactive) or registers with the key, then streams a
 * netmap summary each time it changes. --once exits after the first netmap.
 * SIGUSR1 pauses the node (rest mode), SIGUSR2 resumes it.
 */
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
	}
	fflush(stdout);
}

static int usage(void)
{
	fprintf(stderr, "usage: tnctl login --control URL --state DIR [--authkey KEY] [--hostname NAME]\n"
			"                   [--ca-file PEM] [--transport auto|plain|tls] [--once]\n"
			"                   [--timeout SEC] [--dump DIR] [-v|-vv]\n");
	return 2;
}

int main(int argc, char **argv)
{
	tn_config cfg = {0};
	int timeout = 0;
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
	time_t start = time(NULL);
	while (!g_quit && (!timeout || time(NULL) - start < timeout)) {
		struct timespec ts = {0, 100 * 1000 * 1000};
		nanosleep(&ts, NULL);
		/* SIGUSR1/SIGUSR2 simulate rest mode and wake (tn_pause/tn_resume). */
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
	tn_destroy(ctx);
	return g_got_netmap ? 0 : 1;
}
