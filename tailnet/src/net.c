/* TCP transport with optional TLS (BearSSL), a wake fd and deadlines; URL and
 * HTTP/1.1 header helpers. */
#include "tn.h"
#include "crypto.h"
#include "isrg_roots.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>


int tn_url_parse(tn_url *u, const char *url)
{
	memset(u, 0, sizeof *u);
	const char *p;
	if (!strncmp(url, "https://", 8)) {
		u->https = 1;
		p = url + 8;
	} else if (!strncmp(url, "http://", 7)) {
		p = url + 7;
	} else {
		return TN_E_ARG;
	}
	size_t n = strcspn(p, ":/");
	if (n == 0 || n >= sizeof u->host)
		return TN_E_ARG;
	memcpy(u->host, p, n);
	u->host[n] = 0;
	if (p[n] == ':') {
		char *end;
		long port = strtol(p + n + 1, &end, 10);
		if (port <= 0 || port > 65535 || (*end && *end != '/'))
			return TN_E_ARG;
		u->port = (int)port;
	}
	return TN_OK;
}

/* ---- trust anchors from a PEM file (private control servers) ---- */

static void *ta_alloc(tn_tls *t, const void *src, size_t n)
{
	if (t->ta_used + n > sizeof t->ta_mem)
		return NULL;
	void *p = t->ta_mem + t->ta_used;
	memcpy(p, src, n);
	t->ta_used += n;
	return p;
}

typedef struct {
	unsigned char buf[1024];
	size_t len;
	int over;
} dn_acc;

static void dn_append(void *ctx, const void *src, size_t n)
{
	dn_acc *a = ctx;
	if (a->len + n > sizeof a->buf) {
		a->over = 1;
		return;
	}
	memcpy(a->buf + a->len, src, n);
	a->len += n;
}

static int add_anchor(tn_tls *t, const unsigned char *der, size_t n)
{
	br_x509_decoder_context dc;
	dn_acc dn = {.len = 0};
	if (t->n_tas >= sizeof t->tas / sizeof t->tas[0])
		return -1;
	br_x509_decoder_init(&dc, dn_append, &dn);
	br_x509_decoder_push(&dc, der, n);
	br_x509_pkey *pk = br_x509_decoder_get_pkey(&dc);
	if (!pk || dn.over)
		return -1;
	br_x509_trust_anchor *ta = &t->tas[t->n_tas];
	memset(ta, 0, sizeof *ta);
	ta->flags = br_x509_decoder_isCA(&dc) ? BR_X509_TA_CA : 0;
	if (!(ta->dn.data = ta_alloc(t, dn.buf, dn.len)))
		return -1;
	ta->dn.len = dn.len;
	ta->pkey.key_type = pk->key_type;
	if (pk->key_type == BR_KEYTYPE_RSA) {
		ta->pkey.key.rsa.n = ta_alloc(t, pk->key.rsa.n, pk->key.rsa.nlen);
		ta->pkey.key.rsa.e = ta_alloc(t, pk->key.rsa.e, pk->key.rsa.elen);
		ta->pkey.key.rsa.nlen = pk->key.rsa.nlen;
		ta->pkey.key.rsa.elen = pk->key.rsa.elen;
		if (!ta->pkey.key.rsa.n || !ta->pkey.key.rsa.e)
			return -1;
	} else if (pk->key_type == BR_KEYTYPE_EC) {
		ta->pkey.key.ec.curve = pk->key.ec.curve;
		ta->pkey.key.ec.q = ta_alloc(t, pk->key.ec.q, pk->key.ec.qlen);
		ta->pkey.key.ec.qlen = pk->key.ec.qlen;
		if (!ta->pkey.key.ec.q)
			return -1;
	} else {
		return -1;
	}
	t->n_tas++;
	return 0;
}

typedef struct {
	unsigned char *buf;
	size_t len, cap;
	int over;
} sink;

static void sink_append(void *ctx, const void *src, size_t n)
{
	sink *s = ctx;
	if (s->len + n > s->cap) {
		s->over = 1;
		return;
	}
	memcpy(s->buf + s->len, src, n);
	s->len += n;
}

static int load_ca_file(tn_tls *t, const char *path)
{
	enum { FILE_MAX = 65536, DER_MAX = 8192 };
	/* Init-time only: a temporary heap buffer for the file. */
	unsigned char *file = malloc(FILE_MAX + DER_MAX);
	if (!file)
		return TN_E_NOMEM;
	unsigned char *der = file + FILE_MAX;
	int added = 0;
	int n = tn_file_read(path, file, FILE_MAX);
	if (n <= 0) {
		free(file);
		return TN_E_STATE;
	}
	br_pem_decoder_context pc;
	br_pem_decoder_init(&pc);
	sink s = {.buf = der, .cap = DER_MAX};
	int in_cert = 0;
	size_t off = 0;
	while (off < (size_t)n) {
		off += br_pem_decoder_push(&pc, file + off, (size_t)n - off);
		switch (br_pem_decoder_event(&pc)) {
		case BR_PEM_BEGIN_OBJ:
			in_cert = !strcmp(br_pem_decoder_name(&pc), "CERTIFICATE");
			s.len = 0;
			s.over = 0;
			br_pem_decoder_setdest(&pc, in_cert ? sink_append : NULL, &s);
			break;
		case BR_PEM_END_OBJ:
			if (in_cert && !s.over && add_anchor(t, der, s.len) == 0)
				added++;
			in_cert = 0;
			break;
		case BR_PEM_ERROR:
			off = (size_t)n;
			break;
		}
	}
	free(file);
	return added ? TN_OK : TN_E_TLS;
}

int tn_tls_init(tn_tls *t, const char *ca_file)
{
	memset(t, 0, sizeof *t);
	for (size_t i = 0; i < TN_ISRG_TAS_NUM; i++)
		t->tas[t->n_tas++] = tn_isrg_tas[i];
	if (ca_file && *ca_file) {
		int r = load_ca_file(t, ca_file);
		if (r < 0)
			return r;
	}
	return TN_OK;
}

/* ---- sockets ---- */

static int wait_fd(tn_tr *tr, short ev)
{
	for (;;) {
		struct pollfd p[2] = {{.fd = tr->fd, .events = ev}, {.fd = tr->wake_fd, .events = POLLIN}};
		int timeout = -1;
		if (tr->deadline) {
			int64_t left = tr->deadline - tn_now_ms();
			if (left <= 0)
				return TN_E_TIMEOUT;
			timeout = left > 60000 ? 60000 : (int)left;
		}
		int r = tn_poll(p, tr->wake_fd >= 0 ? 2 : 1, timeout);
		if (r < 0) {
			if (tn_sock_errno() == TN_ERR_INTR)
				continue;
			tr->sys_err = tn_sock_raw_errno();
			return TN_E_IO;
		}
		if (tr->wake_fd >= 0 && (p[1].revents & POLLIN))
			return TN_E_WAKE;
		if (p[0].revents)
			return TN_OK;
	}
}

static int sock_read(tn_tr *tr, void *buf, size_t n)
{
	for (;;) {
		ssize_t r = tn_sock_recv(tr->fd, buf, n);
		if (r > 0)
			return (int)(r > 0x7fffffff ? 0x7fffffff : r);
		if (r == 0)
			return TN_E_IO;
		int e = tn_sock_errno();
		if (e == TN_ERR_INTR)
			continue;
		if (e != TN_ERR_AGAIN) {
			tr->sys_err = tn_sock_raw_errno(); /* the PS5's errno 163 in rest mode, among others */
			return TN_E_IO;
		}
		int w = wait_fd(tr, POLLIN);
		if (w < 0)
			return w;
	}
}

static int sock_write(tn_tr *tr, const void *buf, size_t n)
{
	for (;;) {
		ssize_t r = tn_sock_send(tr->fd, buf, n);
		if (r >= 0)
			return (int)r;
		int e = tn_sock_errno();
		if (e == TN_ERR_INTR)
			continue;
		if (e != TN_ERR_AGAIN) {
			tr->sys_err = tn_sock_raw_errno();
			return TN_E_IO;
		}
		int w = wait_fd(tr, POLLOUT);
		if (w < 0)
			return w;
	}
}

static int low_read(void *ctx, unsigned char *buf, size_t len)
{
	tn_tr *tr = ctx;
	int r = sock_read(tr, buf, len);
	if (r < 0) {
		tr->err = r;
		return -1;
	}
	return r;
}

static int low_write(void *ctx, const unsigned char *buf, size_t len)
{
	tn_tr *tr = ctx;
	int r = sock_write(tr, buf, len);
	if (r < 0) {
		tr->err = r;
		return -1;
	}
	return r;
}

int tn_tr_dial(tn_tr *tr, const char *host, const char *addr, int port, tn_tls *tls, int wake_fd,
	       int64_t deadline)
{
	struct sockaddr_in sa = {.sin_family = AF_INET, .sin_port = htons((uint16_t)port)};
	memset(tr, 0, sizeof *tr);
	tr->fd = -1;
	tr->wake_fd = wake_fd;
	tr->deadline = deadline;
	if (inet_pton(AF_INET, host, &sa.sin_addr) != 1) {
		/* Not wakeable; only the control host is ever looked up (DERP nodes carry IPs). */
		struct addrinfo hints = {.ai_family = AF_INET, .ai_socktype = SOCK_STREAM}, *res = NULL;
		if (getaddrinfo(host, NULL, &hints, &res) == 0 && res) {
			sa.sin_addr = ((struct sockaddr_in *)res->ai_addr)->sin_addr;
			freeaddrinfo(res);
		} else if (!addr || !*addr || inet_pton(AF_INET, addr, &sa.sin_addr) != 1) {
			return TN_E_DNS;
		}
	}
	tr->fd = tn_sock_open(SOCK_STREAM);
	if (tr->fd < 0) {
		tr->sys_err = tn_sock_raw_errno();
		return TN_E_IO;
	}
	tn_sock_setint(tr->fd, IPPROTO_TCP, TCP_NODELAY, 1);
	tn_sock_nosigpipe(tr->fd);
	tn_sock_nonblock(tr->fd);
	int cr = tn_sock_connect(tr->fd, ntohl(sa.sin_addr.s_addr), (uint16_t)port);
	if (cr < 0) {
		tr->sys_err = tn_sock_raw_errno();
		tn_tr_close(tr);
		return TN_E_IO;
	}
	if (cr > 0) {
		int w = wait_fd(tr, POLLOUT), se = w < 0 ? 0 : tn_sock_error(tr->fd);
		if (w < 0 || se != 0) {
			if (se > 0)
				tr->sys_err = se; /* the connect's own error (SO_ERROR) */
			tn_tr_close(tr);
			return w < 0 ? w : TN_E_IO;
		}
	}
	tr->ip = ntohl(sa.sin_addr.s_addr);
	if (tls) {
		uint8_t seed[32];
		tr->tls = tls;
		tn_tls_client_setup(&tls->sc, &tls->xc, tls->tas, tls->n_tas);
		br_ssl_engine_set_buffer(&tls->sc.eng, tls->iobuf, sizeof tls->iobuf, 1);
		tn_random(seed, sizeof seed);
		br_ssl_engine_inject_entropy(&tls->sc.eng, seed, sizeof seed);
		if (!br_ssl_client_reset(&tls->sc, host, 0)) {
			tn_tr_close(tr);
			return TN_E_TLS;
		}
		br_sslio_init(&tr->ioc, &tls->sc.eng, low_read, tr, low_write, tr);
		if (br_sslio_flush(&tr->ioc) < 0) { /* runs the handshake */
			int r = tr->err ? tr->err : TN_E_TLS;
			tn_tr_close(tr);
			return r;
		}
	}
	return TN_OK;
}

static int tls_err(tn_tr *tr)
{
	if (tr->err)
		return tr->err;
	int e = br_ssl_engine_last_error(&tr->tls->sc.eng);
	return e == BR_ERR_OK ? TN_E_IO : TN_E_TLS;
}

int tn_tr_read(tn_tr *tr, void *buf, size_t n)
{
	if (tr->pre_pos < tr->pre_len) {
		size_t m = tr->pre_len - tr->pre_pos;
		if (m > n)
			m = n;
		memcpy(buf, tr->pre + tr->pre_pos, m);
		tr->pre_pos += m;
		return (int)m;
	}
	if (n > 0x7fffffff)
		n = 0x7fffffff;
	if (tr->tls) {
		int r = br_sslio_read(&tr->ioc, buf, n);
		return r > 0 ? r : tls_err(tr);
	}
	return sock_read(tr, buf, n);
}

int tn_tr_read_full(tn_tr *tr, void *buf, size_t n)
{
	uint8_t *p = buf;
	while (n > 0) {
		int r = tn_tr_read(tr, p, n);
		if (r < 0)
			return r;
		p += r;
		n -= (size_t)r;
	}
	return TN_OK;
}

int tn_tr_write(tn_tr *tr, const void *buf, size_t n)
{
	if (tr->tls) {
		if (br_sslio_write_all(&tr->ioc, buf, n) < 0 || br_sslio_flush(&tr->ioc) < 0)
			return tls_err(tr);
		return TN_OK;
	}
	const uint8_t *p = buf;
	while (n > 0) {
		int r = sock_write(tr, p, n);
		if (r < 0)
			return r;
		p += r;
		n -= (size_t)r;
	}
	return TN_OK;
}

void tn_tr_close(tn_tr *tr)
{
	tn_sock_close(tr->fd);
	tr->fd = -1;
	tr->tls = NULL;
	tr->pre_pos = tr->pre_len = 0;
}

int tn_http_read_header(tn_tr *tr, char *hdr, size_t cap)
{
	size_t len = 0;
	for (;;) {
		if (len + 1 >= cap)
			return TN_E_TOOBIG;
		int r = tn_tr_read(tr, hdr + len, cap - 1 - len);
		if (r < 0)
			return r;
		len += (size_t)r;
		hdr[len] = 0;
		char *end = strstr(hdr, "\r\n\r\n");
		if (!end)
			continue;
		size_t hl = (size_t)(end - hdr) + 4;
		size_t extra = len - hl;
		if (extra > sizeof tr->pre - (tr->pre_len - tr->pre_pos))
			return TN_E_TOOBIG;
		/* Keep what followed the header (Noise response, body) for later reads. */
		memmove(tr->pre, tr->pre + tr->pre_pos, tr->pre_len - tr->pre_pos);
		tr->pre_len -= tr->pre_pos;
		tr->pre_pos = 0;
		memmove(tr->pre + extra, tr->pre, tr->pre_len);
		memcpy(tr->pre, hdr + hl, extra);
		tr->pre_len += extra;
		hdr[hl] = 0;
		int code = 0;
		if (sscanf(hdr, "HTTP/1.%*d %d", &code) != 1)
			return TN_E_PROTO;
		return code;
	}
}

int tn_http_header(const char *hdr, const char *name, char *out, size_t cap)
{
	size_t nl = strlen(name);
	const char *p = strstr(hdr, "\r\n");
	while (p && p[2] != '\r') {
		p += 2;
		if (!strncasecmp(p, name, nl) && p[nl] == ':') {
			const char *v = p + nl + 1;
			while (*v == ' ' || *v == '\t')
				v++;
			size_t vl = strcspn(v, "\r\n");
			if (vl >= cap)
				vl = cap - 1;
			memcpy(out, v, vl);
			out[vl] = 0;
			return 0;
		}
		p = strstr(p, "\r\n");
	}
	return -1;
}
