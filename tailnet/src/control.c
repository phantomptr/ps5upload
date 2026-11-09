/* The control client: /key, the ts2021 upgrade, register and the map long-poll
 * (spec 3). Request/response shapes follow tailscale tailcfg (BSD-3-Clause);
 * the flow follows control/controlclient/direct.go and auto.go. */
#include "tn.h"
#include "crypto.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define CONNECT_TIMEOUT_MS 10000
#define REQUEST_TIMEOUT_MS 30000
#define FOLLOWUP_TIMEOUT_MS (10 * 60 * 1000)
#define MAP_WATCHDOG_MS 120000 /* direct.go:1021 */

static int64_t deadline_in(int64_t ms) { return tn_now_ms() + ms; }

static int ctl_port(const tn_ctx *c, int tls)
{
	if (c->url.port)
		return c->url.port;
	return tls ? 443 : 80;
}

/* GET /key?v=<capver> over TLS for https:// URLs (spec 3.1). */
int tn_fetch_control_key(tn_ctx *c)
{
	char req[512], hdr[4096], body[4096], v[80];
	tn_tr *tr = &c->tr;
	int tls = c->url.https && c->cfg.transport != TN_TRANSPORT_PLAIN;
	int r = tn_tr_dial(tr, c->url.host, c->control_addr, ctl_port(c, tls), tls ? c->tls : NULL, c->wake_r,
			   deadline_in(CONNECT_TIMEOUT_MS));
	if (r < 0)
		return r;
	tr->deadline = deadline_in(REQUEST_TIMEOUT_MS);
	int n = snprintf(req, sizeof req,
			 "GET /key?v=%d HTTP/1.1\r\nHost: %s\r\nUser-Agent: tailnet\r\nAccept: application/json\r\n"
			 "Connection: close\r\n\r\n",
			 TN_CAPVER, c->url.host);
	if ((r = tn_tr_write(tr, req, (size_t)n)) < 0)
		goto out;
	int code = tn_http_read_header(tr, hdr, sizeof hdr);
	if (code < 0) {
		r = code;
		goto out;
	}
	size_t blen = 0;
	for (;;) { /* Connection: close; read to EOF */
		if (blen >= sizeof body - 1) {
			r = TN_E_TOOBIG;
			goto out;
		}
		int m = tn_tr_read(tr, body + blen, sizeof body - 1 - blen);
		if (m == TN_E_IO)
			break;
		if (m < 0) {
			r = m;
			goto out;
		}
		blen += (size_t)m;
	}
	body[blen] = 0;
	if (tn_http_header(hdr, "Transfer-Encoding", v, sizeof v) == 0 && strstr(v, "chunked")) {
		/* de-chunk in place */
		size_t in = 0, o = 0;
		for (;;) {
			unsigned long cl = 0;
			char *e;
			cl = strtoul(body + in, &e, 16);
			if (e == body + in || strncmp(e, "\r\n", 2))
				break;
			in = (size_t)(e - body) + 2;
			if (cl == 0 || cl > blen - in)
				break;
			memmove(body + o, body + in, cl);
			o += cl;
			in += cl + 2;
		}
		blen = o;
		body[blen] = 0;
	}
	if (code != 200) {
		tn_logf(c, 0, "/key: HTTP %d: %.200s", code, body);
		r = TN_E_HTTP;
		goto out;
	}
	tn_jtok toks[16];
	tn_json j = {.t = toks, .cap = 16};
	int pk;
	if (tn_json_parse(&j, body, blen) < 0 || (pk = tn_json_get(&j, 0, "publicKey")) < 0 ||
	    tn_json_type(&j, pk) != TN_J_STR ||
	    tn_key_parse(c->keys.control_key, "mkey:", body + toks[pk].start, toks[pk].end - toks[pk].start) < 0 ||
	    !memcmp(c->keys.control_key, (uint8_t[32]){0}, 32)) {
		r = TN_E_PROTO;
		goto out;
	}
	c->keys.has_control_key = 1;
	tn_key_text(v, "mkey:", c->keys.control_key);
	tn_logf(c, 1, "control key %s", v);
	r = tn_state_save(&c->keys, c->state_dir);
out:
	tn_tr_close(tr);
	return r;
}

/* Dial, POST /ts2021 upgrade with the Noise initiation, finish the handshake,
 * start HTTP/2. */
static int ctl_connect_one(tn_ctx *c, int tls)
{
	tn_noise_hs hs;
	uint8_t init[TN_NOISE_INIT_LEN], resp[TN_NOISE_RESP_LEN];
	char b64[160], req[768], hdr[4096], up[64];
	tn_tr *tr = &c->tr;
	int port = ctl_port(c, tls), r;

	tn_logf(c, 1, "dialing %s:%d (%s)", c->url.host, port, tls ? "tls" : "plain");
	if ((r = tn_tr_dial(tr, c->url.host, c->control_addr, port, tls ? c->tls : NULL, c->wake_r, deadline_in(CONNECT_TIMEOUT_MS))) < 0)
		return r;
	tr->deadline = deadline_in(REQUEST_TIMEOUT_MS);
	if ((r = tn_noise_initiate(&hs, init, TN_CAPVER, c->keys.machine_priv, c->keys.control_key, NULL)) < 0)
		goto fail;
	tn_b64(b64, sizeof b64, init, sizeof init);
	int n = snprintf(req, sizeof req,
			 "POST /ts2021 HTTP/1.1\r\nHost: %s\r\nUpgrade: tailscale-control-protocol\r\n"
			 "Connection: upgrade\r\nX-Tailscale-Handshake: %s\r\nContent-Length: 0\r\n\r\n",
			 c->url.host, b64);
	if ((r = tn_tr_write(tr, req, (size_t)n)) < 0)
		goto fail;
	int code = tn_http_read_header(tr, hdr, sizeof hdr);
	if (code != 101 || tn_http_header(hdr, "Upgrade", up, sizeof up) < 0 ||
	    strcmp(up, "tailscale-control-protocol") != 0) {
		tn_logf(c, 0, "ts2021 upgrade: HTTP %d", code);
		r = code < 0 ? code : TN_E_HTTP;
		goto fail;
	}
	/* Response or error message (controlbase messages.go:13-19,71-90). */
	if ((r = tn_tr_read_full(tr, resp, 3)) < 0)
		goto fail;
	if (resp[0] == 0x03) {
		char msg[256];
		size_t len = tn_be16(resp + 1), m = len < sizeof msg - 1 ? len : sizeof msg - 1;
		if (tn_tr_read_full(tr, msg, m) == 0) {
			msg[m] = 0;
			tn_logf(c, 0, "control refused the handshake: %s", msg);
		}
		/* A key error: the cached control key may be stale; refetch next time. */
		c->keys.has_control_key = 0;
		r = TN_E_AUTH;
		goto fail;
	}
	if ((r = tn_tr_read_full(tr, resp + 3, sizeof resp - 3)) < 0)
		goto fail;
	c->nc.tr = tr;
	if ((r = tn_noise_finish(&hs, resp, c->keys.machine_priv, &c->nc)) < 0) {
		c->keys.has_control_key = 0;
		goto fail;
	}
	if ((r = tn_h2_start(&c->h2, &c->nc)) < 0)
		goto fail;
	return TN_OK;
fail:
	tn_wipe(&hs, sizeof hs);
	tn_tr_close(tr);
	return r;
}

static int ctl_connect(tn_ctx *c)
{
	switch (c->cfg.transport) {
	case TN_TRANSPORT_PLAIN:
		return ctl_connect_one(c, 0);
	case TN_TRANSPORT_TLS:
		return ctl_connect_one(c, 1);
	default:
		break;
	}
	if (!c->url.https)
		return ctl_connect_one(c, 0); /* http:// never tries TLS (ts2021/client.go:131-156) */
	if (c->url.port)
		return ctl_connect_one(c, 1); /* explicit https port: TLS only */
	/* https without a port: plain port 80 first (the Noise layer is the crypto),
	 * TLS 443 as the fallback (controlhttp/client.go:240-340). */
	int r = ctl_connect_one(c, 0);
	if (r == TN_OK || r == TN_E_WAKE || r == TN_E_AUTH)
		return r;
	tn_logf(c, 1, "port 80 failed (%s); trying TLS 443", tn_strerror(r));
	return ctl_connect_one(c, 1);
}

void tn_control_close(tn_ctx *c)
{
	tn_tr_close(&c->tr);
	tn_wipe(&c->nc.tx_key, sizeof c->nc.tx_key);
	tn_wipe(&c->nc.rx_key, sizeof c->nc.rx_key);
}

static void hostinfo(tn_ctx *c, tn_jw *w)
{
	tn_jw_key(w, "Hostinfo");
	tn_jw_obj(w);
	tn_jw_kstr(w, "IPNVersion", "tailnet-0.1");
	tn_jw_kstr(w, "OS", c->os);
	tn_jw_kstr(w, "Hostname", c->hostname);
	tn_jw_kstr(w, "App", c->app);
	/* Our home DERP and its latency, from the data plane's netcheck (spec 4.4): peers
	 * learn where to reach us from this. */
	tn_mutex_lock(&c->mu);
	tn_report r = c->report;
	tn_mutex_unlock(&c->mu);
	if (r.preferred_derp) {
		char k[32], v[32];
		tn_jw_key(w, "NetInfo");
		tn_jw_obj(w);
		tn_jw_kint(w, "PreferredDERP", r.preferred_derp);
		tn_jw_key(w, "DERPLatency");
		tn_jw_obj(w);
		for (uint32_t i = 0; i < r.n_lat; i++) {
			snprintf(k, sizeof k, "%d-v4", (int)r.lat[i].region);
			snprintf(v, sizeof v, "%d.%06d", (int)(r.lat[i].us / 1000000), (int)(r.lat[i].us % 1000000));
			tn_jw_kraw(w, k, v);
		}
		tn_jw_end(w);
		tn_jw_raw(w, ",");
		tn_jw_end(w);
		tn_jw_raw(w, ",");
	}
	tn_jw_end(w);
	tn_jw_raw(w, ",");
}

/* Run frames until stream `slot` ends; frames for other streams are handled too. */
static int wait_stream(tn_ctx *c, int slot, int64_t timeout_ms)
{
	c->tr.deadline = deadline_in(timeout_ms);
	for (;;) {
		tn_h2_stream *st = &c->h2.st[slot];
		if (st->ended || st->reset)
			return st->reset ? TN_E_IO : TN_OK;
		int touched, r = tn_h2_step(&c->h2, &touched);
		if (r < 0)
			return r;
		if (c->h2.goaway && !c->h2.st[slot].ended)
			return TN_E_IO;
	}
}

static int post_json(tn_ctx *c, const char *path, tn_jw *w, uint8_t *sink, size_t cap)
{
	if (w->err)
		return TN_E_TOOBIG;
	return tn_h2_post(&c->h2, c->url.host, path, w->buf, w->len, sink, cap);
}

static void rotate_node_key(tn_ctx *c)
{
	memcpy(c->keys.old_node_pub, c->keys.node_pub, 32);
	c->keys.has_old_node = 1;
	tn_key_new(c->keys.node_priv, c->keys.node_pub);
	c->keys.logged_in = 0;
	tn_state_save(&c->keys, c->state_dir);
}

void tn_control_reauth(tn_ctx *c)
{
	rotate_node_key(c);
	c->followup[0] = 0;
	c->last_url[0] = 0;
}

/* POST /machine/register until authorized (spec 3.8). */
static int do_register(tn_ctx *c)
{
	char t[80];
	for (int round = 0;; round++) {
		tn_jw w;
		tn_jw_init(&w, c->reqbuf, sizeof c->reqbuf);
		tn_jw_obj(&w);
		tn_jw_kint(&w, "Version", TN_CAPVER);
		tn_key_text(t, "nodekey:", c->keys.node_pub);
		tn_jw_kstr(&w, "NodeKey", t);
		tn_key_text(t, "nodekey:", c->keys.has_old_node ? c->keys.old_node_pub : (const uint8_t[32]){0});
		tn_jw_kstr(&w, "OldNodeKey", t);
		if (c->auth_key[0] && !c->followup[0]) {
			tn_jw_key(&w, "Auth");
			tn_jw_obj(&w);
			tn_jw_kstr(&w, "AuthKey", c->auth_key);
			tn_jw_end(&w);
			tn_jw_raw(&w, ",");
		}
		tn_jw_kstr(&w, "Expiry", "0001-01-01T00:00:00Z");
		tn_jw_kstr(&w, "Followup", c->followup);
		hostinfo(c, &w);
		tn_jw_kbool(&w, "Ephemeral", c->cfg.ephemeral);
		tn_jw_end(&w);

		int slot = post_json(c, "/machine/register", &w, c->smallbuf, sizeof c->smallbuf - 1);
		if (slot < 0)
			return slot;
		int r = wait_stream(c, slot, c->followup[0] ? FOLLOWUP_TIMEOUT_MS : REQUEST_TIMEOUT_MS);
		tn_h2_stream st = c->h2.st[slot];
		tn_h2_release(&c->h2, slot);
		if (r == TN_E_TIMEOUT && c->followup[0])
			continue; /* re-send the followup (auto.go:381-403) */
		if (r < 0)
			return r;
		c->smallbuf[st.len] = 0;
		if (st.status == 429 || st.status == 503) {
			if (st.retry_after_s > 0)
				c->retry_after_ms = (int64_t)st.retry_after_s * 1000;
			tn_logf(c, 0, "register: HTTP %d (retry after %d s)", st.status, st.retry_after_s);
			return TN_E_HTTP;
		}
		if (st.status != 200 || st.overflow) {
			tn_logf(c, 0, "register: HTTP %d: %.200s", st.status, (char *)c->smallbuf);
			if (c->followup[0] && st.status >= 400 && st.status < 500)
				c->followup[0] = 0; /* the URL went stale: start over */
			return TN_E_AUTH;
		}
		tn_jtok toks[96];
		tn_json j = {.t = toks, .cap = 96};
		if (tn_json_parse(&j, (char *)c->smallbuf, st.len) < 0 || tn_json_type(&j, 0) != TN_J_OBJ)
			return TN_E_PROTO;
		char err[256] = "", url[512] = "";
		tn_json_str(&j, tn_json_get(&j, 0, "Error"), err, sizeof err);
		tn_json_str(&j, tn_json_get(&j, 0, "AuthURL"), url, sizeof url);
		int authorized = tn_json_bool(&j, tn_json_get(&j, 0, "MachineAuthorized")) == 1;
		int expired = tn_json_bool(&j, tn_json_get(&j, 0, "NodeKeyExpired")) == 1;
		if (err[0]) {
			c->followup[0] = 0;
			tn_emit(c, TN_EV_ERROR, TN_E_AUTH, err);
			return TN_E_AUTH;
		}
		if (expired && !url[0] && round < 2) {
			tn_logf(c, 1, "node key expired; rotating");
			rotate_node_key(c);
			c->followup[0] = 0;
			continue;
		}
		if (url[0]) {
			if (strcmp(url, c->last_url) != 0) {
				tn_strlcpy(c->last_url, url, sizeof c->last_url);
				tn_set_state(c, TN_ST_NEEDS_LOGIN);
				tn_emit(c, TN_EV_LOGIN_URL, 0, url);
			}
			tn_strlcpy(c->followup, url, sizeof c->followup);
			continue;
		}
		c->followup[0] = 0;
		if (!authorized) {
			/* No URL and not authorized: waiting for an admin to approve. */
			tn_set_state(c, TN_ST_WAIT_APPROVAL);
			return TN_E_AUTH;
		}
		char login[128] = "";
		int u = tn_json_get(&j, 0, "Login");
		tn_json_str(&j, tn_json_get(&j, u, "LoginName"), login, sizeof login);
		tn_logf(c, 1, "registered%s%s", login[0] ? " as " : "", login);
		c->keys.logged_in = 1;
		c->keys.has_old_node = 0;
		return tn_state_save(&c->keys, c->state_dir);
	}
}

static void map_request(tn_ctx *c, tn_jw *w, int stream)
{
	char t[80];
	tn_jw_init(w, c->reqbuf, sizeof c->reqbuf);
	tn_jw_obj(w);
	tn_jw_kint(w, "Version", TN_CAPVER);
	tn_jw_kstr(w, "Compress", ""); /* plain JSON: no zstd in the core (spec 3.9) */
	tn_jw_kbool(w, "KeepAlive", stream);
	tn_jw_kbool(w, "Stream", stream);
	if (!stream)
		tn_jw_kbool(w, "OmitPeers", 1);
	tn_key_text(t, "nodekey:", c->keys.node_pub);
	tn_jw_kstr(w, "NodeKey", t);
	tn_key_text(t, "discokey:", c->keys.disco_pub);
	tn_jw_kstr(w, "DiscoKey", t);
	hostinfo(c, w);
	/* Endpoints from the data plane: local interfaces and STUN (spec 5). */
	tn_mutex_lock(&c->mu);
	tn_report r = c->report;
	tn_mutex_unlock(&c->mu);
	tn_jw_key(w, "Endpoints");
	tn_jw_raw(w, "[");
	for (uint32_t i = 0; i < r.n_ep; i++) {
		char ip[16], ep[24];
		snprintf(ep, sizeof ep, "%s:%u", tn_ip4_str(r.ep[i].ip, ip), r.ep[i].port);
		if (i)
			tn_jw_raw(w, ",");
		tn_jw_str(w, ep);
	}
	tn_jw_raw(w, "],");
	tn_jw_end(w);
}

static void dump_map(tn_ctx *c, const uint8_t *p, size_t n)
{
	char path[600];
	if (!c->dump_dir[0])
		return;
	tn_mkdir(c->dump_dir);
	snprintf(path, sizeof path, "%s/map-%04u.json", c->dump_dir, c->dumped++);
	(void)tn_file_write_atomic(path, p, n); /* best effort test hook */
}

/* Warn once per day bucket when our node key expires within 7 days (Tailscale's
 * default expiry is 180 days; Headscale's none). */
static void expiry_check(tn_ctx *c)
{
	int64_t exp = c->nms.nm.self_key_expiry;
	if (exp <= 0)
		return;
	int64_t left = exp - tn_wall_s();
	if (left > 7 * 86400) {
		c->expiry_bucket = -1;
		return;
	}
	int32_t bucket = left <= 0 ? 0 : (int32_t)((left + 43200) / 86400); /* days, rounded */
	if (bucket == c->expiry_bucket)
		return;
	c->expiry_bucket = bucket;
	char msg[160];
	if (left <= 0)
		snprintf(msg, sizeof msg, "this device's Tailscale key has expired; sign in again");
	else if (left < 43200)
		snprintf(msg, sizeof msg, "this device's Tailscale key expires in %lld hours; sign in again to renew it",
			 (long long)(left / 3600) + 1);
	else
		snprintf(msg, sizeof msg, "this device's Tailscale key expires in %d day%s; sign in again to renew it",
			 (int)bucket, bucket == 1 ? "" : "s");
	tn_emit(c, TN_EV_WARN, 0, msg);
}

/* Handle every complete [le32 len][json] message in the map stream buffer. */
static int consume_map(tn_ctx *c, tn_h2_stream *st, int64_t *last_msg)
{
	size_t off = 0;
	int r = TN_OK;
	if (st->overflow)
		return TN_E_TOOBIG;
	while (st->len - off >= 4) {
		uint32_t n = tn_le32(st->body + off);
		if (n > c->mapbuf_cap - 4)
			return TN_E_TOOBIG;
		if (st->len - off - 4 < n)
			break;
		const char *msg = (const char *)st->body + off + 4;
		*last_msg = tn_now_ms();
		dump_map(c, (const uint8_t *)msg, n);
		int nt = tn_json_parse(&c->json, msg, n);
		if (nt < 0) {
			tn_logf(c, 0, "map: %s JSON (%u bytes)", nt == -2 ? "oversized" : "malformed", n);
			return nt == -2 ? TN_E_TOOBIG : TN_E_PROTO;
		}
		tn_map_result res;
		tn_mutex_lock(&c->mu);
		r = tn_netmap_apply(&c->nms, &c->json, &res);
		tn_mutex_unlock(&c->mu);
		if (r < 0)
			return r;
		if (res.changed) {
			tn_atomic_inc(&c->nm_gen);
			tn_dp_wake(c);
			expiry_check(c);
		}
		off += 4 + (size_t)n;
		if (res.pop_url[0] && strcmp(res.pop_url, c->last_url) != 0) {
			tn_strlcpy(c->last_url, res.pop_url, sizeof c->last_url);
			tn_emit(c, TN_EV_LOGIN_URL, 0, res.pop_url);
		}
		if (res.self_expired) {
			c->keys.logged_in = 0;
			tn_state_save(&c->keys, c->state_dir);
			r = TN_E_SIGNED_OUT;
			break;
		}
		if (c->state != TN_ST_RUNNING)
			tn_set_state(c, TN_ST_RUNNING);
		c->backoff_ms = 0;
		if (res.changed)
			tn_emit(c, TN_EV_NETMAP, 0, NULL);
		else if (res.keepalive_only)
			tn_logf(c, 2, "map keep-alive");
	}
	/* Compact the unconsumed tail (at most one partial message). */
	memmove(st->body, st->body + off, st->len - off);
	st->len -= off;
	return r;
}

/* Bytes already read and not yet consumed: h2_step will not block for them. */
static int ctl_buffered(tn_ctx *c)
{
	if (c->tr.pre_pos < c->tr.pre_len || c->nc.pt_pos < c->nc.pt_len || c->h2.npend)
		return 1;
	return c->tr.tls && (br_ssl_engine_current_state(&c->tr.tls->sc.eng) & BR_SSL_RECVAPP);
}

static int map_poll(tn_ctx *c)
{
	tn_jw w;
	map_request(c, &w, 1);
	int map = post_json(c, "/machine/map", &w, c->mapbuf, c->mapbuf_cap);
	if (map < 0)
		return map;
	/* A lite update on a second stream while the poll is open (spec 3.9). */
	tn_atomic_store(&c->report_dirty, 0);
	map_request(c, &w, 0);
	int lite = post_json(c, "/machine/map", &w, c->litebuf, sizeof c->litebuf);
	if (lite < 0)
		return lite;
	int64_t last = tn_now_ms();
	for (;;) {
		c->tr.deadline = last + MAP_WATCHDOG_MS;
		/* Between frames, also listen to the data plane: new endpoints or home DERP
		 * mean a lite update; a clock jump means a fresh session (spec 3.9, 13). */
		if (!ctl_buffered(c)) {
			struct pollfd pf[3] = {{.fd = c->tr.fd, .events = POLLIN},
					       {.fd = c->wake_r, .events = POLLIN},
					       {.fd = c->kick_r, .events = POLLIN}};
			int64_t left = c->tr.deadline - tn_now_ms();
			int pr = tn_poll(pf, 3, left <= 0 ? 0 : left > 60000 ? 60000 : (int)left);
			if (pr == 0 && left > 60000)
				continue;
			if (pf[1].revents)
				return TN_E_WAKE;
			if (pf[2].revents) {
				tn_wake_drain(c->kick_r);
				if (tn_atomic_xchg(&c->ctl_reconnect, 0)) {
					tn_logf(c, 1, "control: reconnecting (wake)");
					return TN_E_IO;
				}
			}
			if (lite < 0 && tn_atomic_xchg(&c->report_dirty, 0)) {
				map_request(c, &w, 0);
				lite = post_json(c, "/machine/map", &w, c->litebuf, sizeof c->litebuf);
				if (lite < 0)
					return lite;
				tn_logf(c, 2, "lite map update sent (endpoints/home DERP)");
			}
			if (tn_map_poll_keep_waiting(pr, pf[0].revents, left))
				continue;
		}
		int touched, r = tn_h2_step(&c->h2, &touched);
		if (r == TN_E_TIMEOUT)
			tn_logf(c, 0, "map poll: no message for %d s", MAP_WATCHDOG_MS / 1000);
		if (r < 0)
			return r;
		if (lite >= 0 && touched == lite && (c->h2.st[lite].ended || c->h2.st[lite].reset)) {
			tn_logf(c, 1, "lite map update: HTTP %d", c->h2.st[lite].status);
			tn_h2_release(&c->h2, lite);
			lite = -1;
		}
		if (touched != map)
			continue;
		tn_h2_stream *st = &c->h2.st[map];
		if (st->status && st->status != 200) {
			tn_logf(c, 0, "map poll: HTTP %d", st->status);
			if (st->retry_after_s > 0)
				c->retry_after_ms = (int64_t)st->retry_after_s * 1000;
			if (st->status == 404 || st->status == 401 || st->status == 403) {
				c->keys.logged_in = 0; /* node deleted at control (direct.go:1219-1222) */
				tn_state_save(&c->keys, c->state_dir);
				return TN_E_SIGNED_OUT;
			}
			return TN_E_HTTP;
		}
		if ((r = consume_map(c, st, &last)) < 0)
			return r;
		if (st->ended || st->reset)
			return TN_E_IO; /* the server ended the poll: reconnect */
	}
}

int tn_control_session(tn_ctx *c)
{
	int r;
	/* A wake's "reconnect control" is answered by this fresh session: left set, it would end the
	 * session's first good map poll and add another backoff after the wake. */
	tn_atomic_store(&c->ctl_reconnect, 0);
	tn_wake_drain(c->kick_r);
	tn_set_state(c, c->followup[0] ? TN_ST_NEEDS_LOGIN : TN_ST_CONNECTING);
	if (!c->keys.has_control_key && (r = tn_fetch_control_key(c)) < 0)
		return r;
	if ((r = ctl_connect(c)) < 0)
		return r;
	if (!c->keys.logged_in && (r = do_register(c)) < 0)
		goto out;
	r = map_poll(c);
out:
	tn_control_close(c);
	return r;
}
