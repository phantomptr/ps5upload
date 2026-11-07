/* Context, loop thread, backoff, pause/resume and events. */
#include "tn.h"
#include "crypto.h"

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define BACKOFF_MIN_MS 1000
#define BACKOFF_MAX_MS 30000 /* auto.go:63 */

const char *tn_strerror(int e)
{
	switch (e) {
	case TN_OK: return "ok";
	case TN_E_IO: return "connection error";
	case TN_E_PROTO: return "protocol error";
	case TN_E_TIMEOUT: return "timeout";
	case TN_E_WAKE: return "interrupted";
	case TN_E_NOMEM: return "out of memory";
	case TN_E_CRYPTO: return "authentication failed";
	case TN_E_TLS: return "TLS error";
	case TN_E_HTTP: return "unexpected HTTP status";
	case TN_E_AUTH: return "control refused registration";
	case TN_E_TOOBIG: return "message exceeds a configured cap";
	case TN_E_ARG: return "invalid argument";
	case TN_E_STATE: return "state directory error";
	case TN_E_DNS: return "DNS lookup failed";
	case TN_E_SIGNED_OUT: return "signed out at control";
	default: return "unknown error";
	}
}

const char *tn_state_name(tn_state s)
{
	static const char *n[] = {"stopped", "connecting", "needs-login", "wait-approval",
				  "running", "paused", "backoff", "signed-out"};
	return (unsigned)s < sizeof n / sizeof n[0] ? n[s] : "?";
}

const char *tn_ip4_str(uint32_t ip, char buf[16])
{
	snprintf(buf, 16, "%u.%u.%u.%u", ip >> 24, (ip >> 16) & 255, (ip >> 8) & 255, ip & 255);
	return buf;
}

void tn_emit(tn_ctx *c, tn_event_type type, int err, const char *text)
{
	if (!c->cfg.cb)
		return;
	tn_event ev = {.type = type, .state = c->state, .err = err, .text = text};
	if (type == TN_EV_NETMAP)
		ev.netmap = &c->nms.nm;
	c->cfg.cb(c->cfg.user, &ev);
}

void tn_logf(tn_ctx *c, int level, const char *fmt, ...)
{
	if (level > c->cfg.verbose || !c->cfg.cb)
		return;
	va_list ap;
	va_start(ap, fmt);
	vsnprintf(c->logbuf, sizeof c->logbuf, fmt, ap);
	va_end(ap);
	tn_emit(c, TN_EV_LOG, 0, c->logbuf);
}

void tn_set_state(tn_ctx *c, tn_state st)
{
	if (c->state == st)
		return;
	c->state = st;
	tn_emit(c, TN_EV_STATE, 0, tn_state_name(st));
}

static void copy_opt(char *dst, size_t cap, const char *src, const char *def)
{
	tn_strlcpy(dst, src && *src ? src : def, cap);
}

int tn_create(const tn_config *cfg, tn_ctx **out)
{
	*out = NULL;
	if (!cfg || !cfg->state_dir || !cfg->control_url)
		return TN_E_ARG;
	tn_ctx *c = calloc(1, sizeof *c);
	if (!c)
		return TN_E_NOMEM;
	c->wake_r = c->wake_w = -1;
	c->cfg = *cfg;
	copy_opt(c->state_dir, sizeof c->state_dir, cfg->state_dir, "");
	copy_opt(c->control_url, sizeof c->control_url, cfg->control_url, "");
	/* a trailing slash is not part of the server identity */
	size_t ul = strlen(c->control_url);
	while (ul > 0 && c->control_url[ul - 1] == '/')
		c->control_url[--ul] = 0;
	copy_opt(c->auth_key, sizeof c->auth_key, cfg->auth_key, "");
	char host[64] = "tailnet";
	gethostname(host, sizeof host - 1);
	host[sizeof host - 1] = 0;
	copy_opt(c->hostname, sizeof c->hostname, cfg->hostname, host);
#if defined(__PROSPERO__)
	copy_opt(c->os, sizeof c->os, cfg->os, "ps5");
#elif defined(__APPLE__)
	copy_opt(c->os, sizeof c->os, cfg->os, "macOS");
#else
	copy_opt(c->os, sizeof c->os, cfg->os, "linux");
#endif
	copy_opt(c->app, sizeof c->app, cfg->app, "ps5upload");
	copy_opt(c->ca_file, sizeof c->ca_file, cfg->ca_file, "");
	copy_opt(c->dump_dir, sizeof c->dump_dir, cfg->dump_dir, "");
	/* point cfg strings at our copies; the caller's may go away */
	c->cfg.state_dir = c->state_dir;
	c->cfg.control_url = c->control_url;
	c->cfg.auth_key = c->auth_key;
	c->cfg.hostname = c->hostname;

	c->nms.max_peers = cfg->max_peers ? cfg->max_peers : 256;
	c->nms.max_regions = cfg->max_regions ? cfg->max_regions : 64;
	c->nms.max_rules = cfg->max_rules ? cfg->max_rules : 128;
	c->mapbuf_cap = cfg->map_msg_max ? cfg->map_msg_max : (4u << 20);
	c->mapbuf_cap += 4;
	c->json.cap = cfg->json_tokens_max ? cfg->json_tokens_max : 65536;

	int r = tn_url_parse(&c->url, c->control_url);
	if (r < 0)
		goto fail;
	r = TN_E_NOMEM;
	c->tls = malloc(sizeof *c->tls);
	c->mapbuf = malloc(c->mapbuf_cap);
	c->json.t = calloc(c->json.cap, sizeof *c->json.t);
	c->nms.nm.peers = calloc(c->nms.max_peers, sizeof(tn_peer));
	c->nms.nm.regions = calloc(c->nms.max_regions, sizeof(tn_derp_region));
	c->nms.nm.rules = calloc(c->nms.max_rules, sizeof(tn_filter_rule));
	if (!c->tls || !c->mapbuf || !c->json.t || !c->nms.nm.peers || !c->nms.nm.regions || !c->nms.nm.rules)
		goto fail;
	c->footprint = sizeof *c + sizeof *c->tls + c->mapbuf_cap + c->json.cap * sizeof *c->json.t +
		       c->nms.max_peers * sizeof(tn_peer) + c->nms.max_regions * sizeof(tn_derp_region) +
		       c->nms.max_rules * sizeof(tn_filter_rule);
	if ((r = tn_tls_init(c->tls, c->ca_file)) < 0)
		goto fail;
	int p[2];
	if (pipe(p) != 0) {
		r = TN_E_IO;
		goto fail;
	}
	c->wake_r = p[0];
	c->wake_w = p[1];
	fcntl(c->wake_r, F_SETFL, fcntl(c->wake_r, F_GETFL) | O_NONBLOCK);
	fcntl(c->wake_w, F_SETFL, fcntl(c->wake_w, F_GETFL) | O_NONBLOCK);
	if ((r = tn_state_load(&c->keys, c->state_dir, c->control_url)) < 0)
		goto fail;
	c->tr.fd = -1;
	*out = c;
	return TN_OK;
fail:
	tn_destroy(c);
	return r;
}

size_t tn_memory_footprint(const tn_ctx *c) { return c->footprint; }

static void wake(tn_ctx *c)
{
	char b = 1;
	if (write(c->wake_w, &b, 1) < 0) { /* full pipe: a wake is already pending */
	}
}

static void drain(tn_ctx *c)
{
	char b[64];
	while (read(c->wake_r, b, sizeof b) > 0)
		;
}

/* Sleep up to ms (or forever if ms < 0) unless woken. Returns 1 if woken. */
static int nap(tn_ctx *c, int64_t ms)
{
	struct pollfd p = {.fd = c->wake_r, .events = POLLIN};
	int64_t end = ms >= 0 ? tn_now_ms() + ms : 0;
	for (;;) {
		int t = -1;
		if (ms >= 0) {
			int64_t left = end - tn_now_ms();
			if (left <= 0)
				return 0;
			t = left > 60000 ? 60000 : (int)left;
		}
		int r = poll(&p, 1, t);
		if (r > 0)
			return 1;
		if (r < 0 && errno != EINTR)
			return 0;
	}
}

static void *loop(void *arg)
{
	tn_ctx *c = arg;
	while (!atomic_load(&c->req_stop)) {
		drain(c);
		if (atomic_load(&c->req_pause)) {
			tn_set_state(c, TN_ST_PAUSED);
			nap(c, -1);
			continue;
		}
		int r = tn_control_session(c);
		if (atomic_load(&c->req_stop))
			break;
		if (r == TN_E_WAKE)
			continue; /* pause or stop: handled at the top */
		if (r == TN_E_SIGNED_OUT) {
			tn_set_state(c, TN_ST_SIGNED_OUT);
			tn_netmap_reset(&c->nms);
		}
		tn_emit(c, TN_EV_ERROR, r, tn_strerror(r));
		/* Exponential backoff, capped (auto.go:63); never a busy retry. */
		c->backoff_ms = c->backoff_ms ? c->backoff_ms * 2 : BACKOFF_MIN_MS;
		if (c->backoff_ms > BACKOFF_MAX_MS)
			c->backoff_ms = BACKOFF_MAX_MS;
		if (c->state != TN_ST_SIGNED_OUT && c->state != TN_ST_WAIT_APPROVAL)
			tn_set_state(c, TN_ST_BACKOFF);
		nap(c, c->backoff_ms);
	}
	tn_control_close(c);
	tn_set_state(c, TN_ST_STOPPED);
	return NULL;
}

int tn_start(tn_ctx *c)
{
	if (c->thread_started)
		return TN_OK;
	atomic_store(&c->req_stop, 0);
	if (pthread_create(&c->thread, NULL, loop, c) != 0)
		return TN_E_NOMEM;
	c->thread_started = 1;
	return TN_OK;
}

void tn_stop(tn_ctx *c)
{
	if (!c || !c->thread_started)
		return;
	atomic_store(&c->req_stop, 1);
	wake(c);
	pthread_join(c->thread, NULL);
	c->thread_started = 0;
}

void tn_pause(tn_ctx *c)
{
	atomic_store(&c->req_pause, 1);
	wake(c);
}

void tn_resume(tn_ctx *c)
{
	/* Wake sequence for M1: a fresh control session (new TCP, Noise, map poll).
	 * M2/M3 add: rebind UDP, drop WireGuard keypairs, reset disco trust, DERP. */
	atomic_store(&c->req_pause, 0);
	c->backoff_ms = 0;
	wake(c);
}

void tn_destroy(tn_ctx *c)
{
	if (!c)
		return;
	tn_stop(c);
	if (c->wake_r >= 0)
		close(c->wake_r);
	if (c->wake_w >= 0)
		close(c->wake_w);
	tn_wipe(&c->keys, sizeof c->keys);
	if (c->tls)
		tn_wipe(c->tls, sizeof *c->tls);
	free(c->tls);
	free(c->mapbuf);
	free(c->json.t);
	free(c->nms.nm.peers);
	free(c->nms.nm.regions);
	free(c->nms.nm.rules);
	tn_wipe(c, sizeof *c);
	free(c);
}
