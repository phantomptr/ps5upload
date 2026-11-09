/* Context, loop thread, backoff, pause/resume and events. */
#include "tn.h"
#include "crypto.h"

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

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
	char buf[768]; /* per call: both threads log */
	va_list ap;
	va_start(ap, fmt);
	vsnprintf(buf, sizeof buf, fmt, ap);
	va_end(ap);
	tn_emit(c, TN_EV_LOG, 0, buf);
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

static int make_pipe(int *r, int *w) { return tn_wake_pair(r, w); }

int tn_create(const tn_config *cfg, tn_ctx **out)
{
	*out = NULL;
	if (!cfg || !cfg->state_dir || !cfg->control_url)
		return TN_E_ARG;
	tn_ctx *c = calloc(1, sizeof *c);
	if (!c)
		return TN_E_NOMEM;
	c->wake_r = c->wake_w = c->kick_r = c->kick_w = c->dp_wake_r = c->dp_wake_w = -1;
	c->expiry_bucket = -1;
	c->cfg = *cfg;
	tn_os_init();
	tn_mutex_init(&c->dp_mu);
	tn_mutex_init(&c->mu);
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
	copy_opt(c->control_addr, sizeof c->control_addr, cfg->control_addr, "");
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
	if (make_pipe(&c->wake_r, &c->wake_w) < 0 || make_pipe(&c->kick_r, &c->kick_w) < 0 ||
	    make_pipe(&c->dp_wake_r, &c->dp_wake_w) < 0) {
		r = TN_E_IO;
		goto fail;
	}
	if (cfg->n_serve > TN_SERVE_MAX) {
		r = TN_E_ARG;
		goto fail;
	}
	if (cfg->n_serve) {
		memcpy(c->serve, cfg->serve, cfg->n_serve * sizeof *c->serve);
		c->n_serve = cfg->n_serve;
	}
	c->cfg.serve = c->serve;
	c->allow.all = 1;
	if ((r = tn_dp_create(c)) < 0 || (r = tn_fwd_create(c)) < 0)
		goto fail;
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

size_t tn_abi_size(int which)
{
	switch (which) {
	case 0: return sizeof(tn_config);
	case 1: return sizeof(tn_status);
	case 2: return sizeof(tn_peer_status);
	case 3: return sizeof(tn_event);
	case 4: return sizeof(tn_serve);
	default: return 0;
	}
}

static void wake(tn_ctx *c)
{
	char b = 1;
	(void)b;
	tn_wake_signal(c->wake_w);
}

static void drain(tn_ctx *c) { tn_wake_drain(c->wake_r); }

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
		int r = tn_poll(&p, 1, t);
		if (r > 0)
			return 1;
		if (r < 0 && tn_sock_errno() != TN_ERR_INTR) {
			/* poll() itself failing (rest mode takes the network away) must not end the nap
			 * early: that turned the backoff into an immediate retry. Sleep instead. */
			int64_t left = ms >= 0 ? end - tn_now_ms() : -1;
			tn_sleep_ms(tn_poll_fail_wait_ms(0, left));
		}
	}
}

static void *loop(void *arg)
{
	tn_ctx *c = arg;
	if (c->cfg.thread_cb)
		c->cfg.thread_cb(c->cfg.user, "control");
	while (!tn_atomic_load(&c->req_stop)) {
		drain(c);
		if (tn_atomic_xchg(&c->req_reauth, 0)) {
			tn_logf(c, 1, "renewing the sign-in (same node)");
			tn_control_reauth(c);
			c->backoff_ms = 0;
		}
		if (tn_atomic_load(&c->req_pause)) {
			tn_set_state(c, TN_ST_PAUSED);
			nap(c, -1);
			continue;
		}
		int r = tn_control_session(c);
		if (tn_atomic_load(&c->req_stop))
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
		int64_t wait = c->backoff_ms;
		if (c->retry_after_ms > wait) /* the server's Retry-After wins, capped at 1 h */
			wait = c->retry_after_ms > 3600000 ? 3600000 : c->retry_after_ms;
		c->retry_after_ms = 0;
		if (c->state != TN_ST_SIGNED_OUT && c->state != TN_ST_WAIT_APPROVAL)
			tn_set_state(c, TN_ST_BACKOFF);
		nap(c, wait);
	}
	tn_control_close(c);
	tn_set_state(c, TN_ST_STOPPED);
	return NULL;
}

int tn_start(tn_ctx *c)
{
	if (c->thread_started)
		return TN_OK;
	tn_atomic_store(&c->req_stop, 0);
	int r = tn_dp_start(c);
	if (r < 0)
		return r;
	r = tn_thread_start(&c->thread, loop, c, c->cfg.thread_stack ? c->cfg.thread_stack : 256 * 1024);
	if (r != 0) {
		tn_stop(c);
		return TN_E_NOMEM;
	}
	c->thread_started = 1;
	return TN_OK;
}

void tn_stop(tn_ctx *c)
{
	if (!c)
		return;
	tn_atomic_store(&c->req_stop, 1);
	tn_dp_stop(c);
	if (!c->thread_started)
		return;
	wake(c);
	tn_thread_join(c->thread);
	c->thread_started = 0;
}

void tn_pause(tn_ctx *c)
{
	tn_atomic_store(&c->req_pause, 1);
	wake(c);
	tn_dp_wake(c);
}

void tn_resume(tn_ctx *c)
{
	/* Wake sequence (spec 13): a fresh control session (new TCP, Noise, map poll);
	 * the data plane rebinds UDP, drops WireGuard keys and disco trust, reconnects
	 * DERP and runs netcheck (tn_dp_rewake). */
	tn_atomic_store(&c->req_pause, 0);
	c->backoff_ms = 0;
	wake(c);
	tn_dp_wake(c);
}

int tn_reauth(tn_ctx *c)
{
	if (!c)
		return TN_E_ARG;
	tn_atomic_store(&c->req_reauth, 1);
	wake(c);
	return TN_OK;
}

void tn_test_rewake(tn_ctx *c)
{
	if (!c)
		return;
	tn_atomic_store(&c->req_rewake, 1);
	tn_dp_wake(c);
}

int tn_set_peer_allow(tn_ctx *c, const uint32_t *ips, int n)
{
	if (!c || n > TN_ALLOW_MAX || (n > 0 && !ips))
		return TN_E_ARG;
	tn_mutex_lock(&c->dp_mu);
	c->allow.all = n < 0;
	c->allow.n = n < 0 ? 0 : (uint32_t)n;
	for (int i = 0; i < n; i++)
		c->allow.ip[i] = ips[i];
	tn_denied_prune(&c->denied, &c->allow); /* a device let in is no longer "turned away" */
	tn_mutex_unlock(&c->dp_mu);
	return TN_OK;
}

int tn_peer_denied(tn_ctx *c, tn_denied *out, int max)
{
	if (!c || !out || max <= 0)
		return 0;
	tn_mutex_lock(&c->dp_mu);
	int n = tn_denied_copy(&c->denied, out, max);
	tn_mutex_unlock(&c->dp_mu);
	return n;
}

void tn_test_peer_denied(tn_ctx *c, uint32_t ip, uint8_t proto, uint16_t port)
{
	if (!c)
		return;
	tn_mutex_lock(&c->dp_mu);
	if (!tn_peer_allowed(&c->allow, ip))
		tn_denied_note(&c->denied, ip, proto, port, tn_wall_s());
	tn_mutex_unlock(&c->dp_mu);
}

void tn_destroy(tn_ctx *c)
{
	if (!c)
		return;
	tn_stop(c);
	tn_fwd_destroy(c);
	tn_dp_destroy(c);
	tn_wake_close(c->wake_r, c->wake_w);
	tn_wake_close(c->kick_r, c->kick_w);
	tn_wake_close(c->dp_wake_r, c->dp_wake_w);
	tn_mutex_destroy(&c->mu);
	tn_mutex_destroy(&c->dp_mu);
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
