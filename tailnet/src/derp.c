/* DERP client (spec 4), non-blocking: one connection is a state machine driven by
 * the data-plane poll() loop. TCP connect -> TLS 1.2 (BearSSL engine API) ->
 * "GET /derp" upgrade -> ServerKey -> ClientInfo -> ServerInfo -> frames.
 * Frame formats follow tailscale derp/derp.go and derp_client.go (BSD-3-Clause). */
#include "tn.h"
#include "crypto.h"

#include <stdio.h>
#include <string.h>


static const uint8_t DERP_MAGIC[8] = {0x44, 0x45, 0x52, 0x50, 0xf0, 0x9f, 0x94, 0x91};

/* ---- trimmed BearSSL client profile (TLS 1.2, ECDHE + AEAD only) ----
 * Replaces br_ssl_client_init_full so the link pulls only what DERP and control
 * need: P-256/384 ECDHE, ECDSA and RSA signatures, AES-GCM and ChaCha20-Poly1305,
 * SHA-256/384. (spec 12; derp*.tailscale.com negotiate ECDHE-ECDSA-AES128-GCM.) */
void tn_tls_client_setup(br_ssl_client_context *cc, br_x509_minimal_context *xc,
			 const br_x509_trust_anchor *tas, size_t n_tas)
{
	static const uint16_t suites[] = {
		BR_TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,       BR_TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
		BR_TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256, BR_TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
		BR_TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,       BR_TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
	};
	br_ssl_engine_context *e = &cc->eng;
	br_ssl_client_zero(cc);
	br_ssl_engine_set_versions(e, BR_TLS12, BR_TLS12);
	br_ssl_engine_set_suites(e, suites, sizeof suites / sizeof suites[0]);
	br_ssl_engine_set_hash(e, br_sha256_ID, &br_sha256_vtable);
	br_ssl_engine_set_hash(e, br_sha384_ID, &br_sha384_vtable);
	br_ssl_engine_set_prf_sha256(e, br_tls12_sha256_prf);
	br_ssl_engine_set_prf_sha384(e, br_tls12_sha384_prf);
	br_ssl_engine_set_ec(e, &br_ec_prime_i31);
	br_ssl_engine_set_rsavrfy(e, br_rsa_i31_pkcs1_vrfy);
	br_ssl_engine_set_ecdsa(e, br_ecdsa_i31_vrfy_asn1);
	br_ssl_engine_set_gcm(e, &br_sslrec_in_gcm_vtable, &br_sslrec_out_gcm_vtable);
	br_ssl_engine_set_aes_ctr(e, &br_aes_ct64_ctr_vtable);
	br_ssl_engine_set_ghash(e, br_ghash_ctmul64);
	br_ssl_engine_set_chapol(e, &br_sslrec_in_chapol_vtable, &br_sslrec_out_chapol_vtable);
	br_ssl_engine_set_chacha20(e, br_chacha20_ct_run);
	br_ssl_engine_set_poly1305(e, br_poly1305_ctmul_run);
	br_x509_minimal_init(xc, &br_sha256_vtable, tas, n_tas);
	br_x509_minimal_set_hash(xc, br_sha256_ID, &br_sha256_vtable);
	br_x509_minimal_set_hash(xc, br_sha384_ID, &br_sha384_vtable);
	br_x509_minimal_set_rsa(xc, br_rsa_i31_pkcs1_vrfy);
	br_x509_minimal_set_ecdsa(xc, &br_ec_prime_i31, br_ecdsa_i31_vrfy_asn1);
	br_ssl_engine_set_x509(e, &xc->vtable);
}

/* ---- "InsecureForTests" verifier: takes the leaf key without checking anything.
 * Only used when control's DERP map asks for it (Headscale test setups), like Go. */
static void any_start_chain(const br_x509_class **ctx, const char *server_name)
{
	(void)server_name;
	tn_x509_any *x = (tn_x509_any *)(void *)ctx;
	x->idx = 0;
}
static void any_start_cert(const br_x509_class **ctx, uint32_t length)
{
	(void)length;
	tn_x509_any *x = (tn_x509_any *)(void *)ctx;
	if (x->idx == 0)
		br_x509_decoder_init(&x->dc, NULL, NULL);
}
static void any_append(const br_x509_class **ctx, const unsigned char *buf, size_t len)
{
	tn_x509_any *x = (tn_x509_any *)(void *)ctx;
	if (x->idx == 0)
		br_x509_decoder_push(&x->dc, buf, len);
}
static void any_end_cert(const br_x509_class **ctx)
{
	tn_x509_any *x = (tn_x509_any *)(void *)ctx;
	x->idx++;
}
static unsigned any_end_chain(const br_x509_class **ctx)
{
	tn_x509_any *x = (tn_x509_any *)(void *)ctx;
	return br_x509_decoder_get_pkey(&x->dc) ? 0 : BR_ERR_X509_BAD_SERVER_NAME;
}
static const br_x509_pkey *any_get_pkey(const br_x509_class *const *ctx, unsigned *usages)
{
	tn_x509_any *x = (tn_x509_any *)(void *)ctx;
	if (usages)
		*usages = BR_KEYTYPE_KEYX | BR_KEYTYPE_SIGN;
	return br_x509_decoder_get_pkey(&x->dc);
}
static const br_x509_class any_vtable = {sizeof(tn_x509_any), any_start_chain, any_start_cert, any_append,
					 any_end_cert,        any_end_chain,   any_get_pkey};

/* ---- connection ---- */

void tn_derpc_close(tn_derpc *d)
{
	tn_sock_close(d->fd);
	d->fd = -1;
	d->st = TN_DC_IDLE;
	d->in_len = d->out_len = 0;
	d->skip = 0;
}

int tn_derpc_open(tn_derpc *d, const tn_derp_node *node, int32_t region, const tn_derp_keys *keys,
		  const br_x509_trust_anchor *tas, size_t n_tas, int64_t now)
{
	tn_derpc_close(d);
	d->region = region;
	d->keys = keys;
	d->ip = node->ipv4;
	d->port = (uint16_t)(node->derp_port > 0 ? node->derp_port : 443);
	tn_strlcpy(d->host, node->host, sizeof d->host);
	d->t_start = d->last_rx = now;
	d->fd = tn_sock_open(SOCK_STREAM);
	if (d->fd < 0)
		return TN_E_IO;
	tn_sock_setint(d->fd, IPPROTO_TCP, TCP_NODELAY, 1);
	tn_sock_nosigpipe(d->fd);
	tn_sock_nonblock(d->fd);
	if (tn_sock_connect(d->fd, d->ip, d->port) < 0) {
		tn_derpc_close(d);
		return TN_E_IO;
	}
	d->st = TN_DC_CONNECTING;
	/* TLS is always on for DERP (spec 4.1); the verifier depends on the map. */
	tn_tls_client_setup(&d->sc, &d->xc, tas, n_tas);
	if (node->insecure_for_tests) {
		d->xa.vtable = &any_vtable;
		br_ssl_engine_set_x509(&d->sc.eng, &d->xa.vtable);
	}
	br_ssl_engine_set_buffer(&d->sc.eng, d->iobuf, sizeof d->iobuf, 1);
	uint8_t seed[32];
	tn_random(seed, sizeof seed);
	br_ssl_engine_inject_entropy(&d->sc.eng, seed, sizeof seed);
	const char *sni = node->cert_name[0] && strncmp(node->cert_name, "sha256-raw:", 11) ? node->cert_name : node->host;
	if (!br_ssl_client_reset(&d->sc, sni, 0)) {
		tn_derpc_close(d);
		return TN_E_TLS;
	}
	return TN_OK;
}

short tn_derpc_events(const tn_derpc *d)
{
	if (d->fd < 0)
		return 0;
	if (d->st == TN_DC_CONNECTING)
		return POLLOUT;
	unsigned s = br_ssl_engine_current_state(&d->sc.eng);
	short ev = POLLIN;
	if (s & BR_SSL_SENDREC)
		ev |= POLLOUT;
	return ev;
}

static int sock_err(void)
{
	int e = tn_sock_errno();
	return (e == TN_ERR_AGAIN || e == TN_ERR_INTR) ? 0 : TN_E_IO;
}

/* Move bytes between the socket, the TLS engine and our in/out buffers until
 * nothing moves. Returns 1 if anything moved, 0 if not, TN_E_* on error. */
static int pump(tn_derpc *d)
{
	br_ssl_engine_context *e = &d->sc.eng;
	int any = 0;
	for (;;) {
		unsigned s = br_ssl_engine_current_state(e);
		int moved = 0;
		if (s & BR_SSL_CLOSED)
			return br_ssl_engine_last_error(e) ? TN_E_TLS : TN_E_IO;
		if (s & BR_SSL_SENDREC) {
			size_t len;
			unsigned char *b = br_ssl_engine_sendrec_buf(e, &len);
			ssize_t n = tn_sock_send(d->fd, b, len);
			if (n > 0) {
				br_ssl_engine_sendrec_ack(e, (size_t)n);
				moved = 1;
			} else if (n < 0 && sock_err() < 0) {
				return TN_E_IO;
			}
		} else if ((s & BR_SSL_SENDAPP) && d->out_len) {
			size_t len;
			unsigned char *b = br_ssl_engine_sendapp_buf(e, &len);
			size_t m = len < d->out_len ? len : d->out_len;
			memcpy(b, d->out, m);
			br_ssl_engine_sendapp_ack(e, m);
			br_ssl_engine_flush(e, 0);
			memmove(d->out, d->out + m, d->out_len - m);
			d->out_len -= m;
			moved = 1;
		}
		if (s & BR_SSL_RECVAPP) {
			size_t len, room = sizeof d->in - d->in_len;
			if (room) {
				unsigned char *b = br_ssl_engine_recvapp_buf(e, &len);
				size_t m = len < room ? len : room;
				memcpy(d->in + d->in_len, b, m);
				br_ssl_engine_recvapp_ack(e, m);
				d->in_len += m;
				moved = 1;
			}
		} else if (s & BR_SSL_RECVREC) {
			size_t len;
			unsigned char *b = br_ssl_engine_recvrec_buf(e, &len);
			ssize_t n = tn_sock_recv(d->fd, b, len);
			if (n > 0) {
				br_ssl_engine_recvrec_ack(e, (size_t)n);
				moved = 1;
			} else if (n == 0) {
				return TN_E_IO;
			} else if (sock_err() < 0) {
				return TN_E_IO; /* includes errno 163 on the PS5 in rest mode */
			}
		}
		if (!moved)
			return any;
		any = 1;
	}
}

static int queue(tn_derpc *d, uint8_t type, const uint8_t *a, size_t an, const uint8_t *b, size_t bn)
{
	size_t n = 5 + an + bn;
	if (d->out_len + n > sizeof d->out)
		return TN_E_TOOBIG; /* full: drop, like a congested UDP path */
	uint8_t *o = d->out + d->out_len;
	o[0] = type;
	tn_put_be32(o + 1, (uint32_t)(an + bn));
	if (an)
		memcpy(o + 5, a, an);
	if (bn)
		memcpy(o + 5 + an, b, bn);
	d->out_len += n;
	return TN_OK;
}

int tn_derpc_send(tn_derpc *d, const uint8_t dst[32], const uint8_t *pkt, size_t n)
{
	if (d->st != TN_DC_READY || n > TN_DERP_PKT_MAX)
		return TN_E_STATE;
	int r = queue(d, TN_DERP_SEND, dst, 32, pkt, n);
	if (r == TN_OK && pump(d) < 0)
		return TN_E_IO;
	return r;
}

int tn_derpc_note_preferred(tn_derpc *d, int preferred)
{
	uint8_t b = preferred ? 1 : 0;
	if (d->st != TN_DC_READY)
		return TN_E_STATE;
	return queue(d, TN_DERP_NOTE_PREFERRED, &b, 1, NULL, 0);
}

static int client_info(tn_derpc *d)
{
	static const char info[] = "{\"version\":2,\"CanAckPings\":true}";
	uint8_t k[32], nonce[24], box[sizeof info - 1 + 16];
	if (tn_box_beforenm(k, d->server_key, d->keys->node_priv) < 0)
		return TN_E_CRYPTO;
	tn_random(nonce, sizeof nonce);
	tn_secretbox_seal(box, (const uint8_t *)info, sizeof info - 1, nonce, k);
	tn_wipe(k, sizeof k);
	uint8_t hdr[56];
	memcpy(hdr, d->keys->node_pub, 32);
	memcpy(hdr + 32, nonce, 24);
	return queue(d, TN_DERP_CLIENT_INFO, hdr, sizeof hdr, box, sizeof box);
}

/* Handle buffered input. Returns 1 if a frame or header was consumed. */
static int consume(tn_derpc *d, tn_derp_frame_cb cb, void *u, int64_t now)
{
	int any = 0;
	if (d->st == TN_DC_HTTP) {
		void *end = NULL;
		for (size_t i = 0; i + 4 <= d->in_len; i++)
			if (!memcmp(d->in + i, "\r\n\r\n", 4)) {
				end = d->in + i + 4;
				break;
			}
		if (!end)
			return d->in_len == sizeof d->in ? TN_E_PROTO : 0;
		int code = 0;
		if (sscanf((const char *)d->in, "HTTP/1.%*d %d", &code) != 1 || code != 101)
			return TN_E_HTTP;
		size_t hl = (size_t)((uint8_t *)end - d->in);
		memmove(d->in, d->in + hl, d->in_len - hl);
		d->in_len -= hl;
		d->st = TN_DC_SERVERKEY;
		any = 1;
	}
	for (;;) {
		if (d->skip) {
			size_t m = d->skip < d->in_len ? d->skip : d->in_len;
			memmove(d->in, d->in + m, d->in_len - m);
			d->in_len -= m;
			d->skip -= (uint32_t)m;
			if (d->skip)
				return any;
		}
		if (d->in_len < 5)
			return any;
		uint8_t type = d->in[0];
		uint32_t len = tn_be32(d->in + 1);
		if (len > sizeof d->in - 5) { /* too big for us (we never need it): skip */
			memmove(d->in, d->in + 5, d->in_len - 5);
			d->in_len -= 5;
			d->skip = len;
			continue;
		}
		if (d->in_len < 5 + (size_t)len)
			return any;
		const uint8_t *p = d->in + 5;
		d->last_rx = now;
		any = 1;
		int r = TN_OK;
		switch (type) {
		case TN_DERP_SERVER_KEY:
			if (d->st != TN_DC_SERVERKEY || len < 40 || memcmp(p, DERP_MAGIC, 8))
				return TN_E_PROTO;
			memcpy(d->server_key, p + 8, 32);
			if ((r = client_info(d)) < 0)
				return r;
			d->st = TN_DC_SERVERINFO;
			break;
		case TN_DERP_SERVER_INFO:
			if (d->st == TN_DC_SERVERINFO) {
				d->st = TN_DC_READY;
				d->backoff_ms = 0;
				cb(u, d, type, p, len);
			}
			break;
		case TN_DERP_PING:
			if (len >= 8)
				queue(d, TN_DERP_PONG, p, 8, NULL, 0);
			break;
		case TN_DERP_KEEPALIVE:
			break;
		default:
			if (d->st == TN_DC_READY)
				cb(u, d, type, p, len);
			break;
		}
		memmove(d->in, d->in + 5 + len, d->in_len - 5 - len);
		d->in_len -= 5 + len;
	}
}

int tn_derpc_io(tn_derpc *d, short revents, tn_derp_frame_cb cb, void *u, int64_t now)
{
	if (d->fd < 0)
		return TN_OK;
	if (d->st == TN_DC_CONNECTING) {
		if (!(revents & (POLLOUT | POLLERR | POLLHUP)))
			return TN_OK;
		if (tn_sock_error(d->fd) != 0)
			return TN_E_IO;
		d->st = TN_DC_TLS;
	}
	for (int rounds = 0; rounds < 64; rounds++) {
		int p = pump(d);
		if (p < 0)
			return p;
		if (d->st == TN_DC_TLS && (br_ssl_engine_current_state(&d->sc.eng) & BR_SSL_SENDAPP)) {
			/* Handshake done: send the upgrade request (derphttp_client.go:509-563). */
			char req[256];
			int n = snprintf(req, sizeof req,
					 "GET /derp HTTP/1.1\r\nHost: %s\r\nUpgrade: DERP\r\nConnection: Upgrade\r\n"
					 "User-Agent: tailnet\r\n\r\n",
					 d->host);
			memcpy(d->out, req, (size_t)n);
			d->out_len = (size_t)n;
			d->st = TN_DC_HTTP;
			continue;
		}
		int c = d->st >= TN_DC_HTTP ? consume(d, cb, u, now) : 0;
		if (c < 0)
			return c;
		if (!p && !c)
			break;
	}
	return TN_OK;
}
