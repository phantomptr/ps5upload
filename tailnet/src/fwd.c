/* The forwarder: serves a fixed set of tailnet ports from this node's local ones
 * (spec 10, 16). Runs inside the data-plane poll loop, under c->dp_mu.
 *
 *   UDP: a straight relay. Each remote (ip, port) -> our port P gets its own socket
 *        connected to 127.0.0.1:P; replies are wrapped as IPv4/UDP from our tailnet
 *        address. Idle flows close after 2 minutes.
 *   TCP: terminated in lwIP (NO_SYS) and spliced to a socket connected to
 *        127.0.0.1:P, with back-pressure both ways (tcp_recved only after the local
 *        write, local reads only while lwIP has send buffer). A SYN is held until the
 *        local dial succeeds, so a closed local port answers the peer with a RST
 *        ("connection refused") instead of an accept followed by a reset (spec 10).
 *
 * lwIP keeps global state, so one forwarder per process. */
#include "tn.h"
#include "crypto.h"

#include "lwip/init.h"
#include "lwip/ip4.h"
#include "lwip/netif.h"
#include "lwip/pbuf.h"
#include "lwip/tcp.h"
#include "lwip/timeouts.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#ifndef MSG_NOSIGNAL
#define MSG_NOSIGNAL 0
#endif

#define UDP_FLOWS 32
#define UDP_IDLE_MS 120000
#define TCP_CONNS 16
#define SYN_WAITS 8
#define LOCAL_CONNECT_MS 5000

/* A SYN waiting for its local dial. */
typedef struct {
	int fd;
	uint8_t ready;          /* dialed; the SYN went into lwIP, on_accept takes the fd */
	uint32_t rip;
	uint16_t rport, port;
	int64_t started;
	uint16_t len;
	uint8_t syn[120];       /* the SYN itself (IPv4 + TCP headers with options) */
} syn_wait;

typedef struct {
	int fd;
	uint32_t rip;
	uint16_t rport, port;
	int64_t last;
} udp_flow;

typedef struct tcp_conn {
	int used;
	int fd;
	struct tcp_pcb *pcb;
	uint8_t connecting, remote_eof, local_eof, shut_wr, fin_sent;
	int64_t started;
	struct pbuf *rxq;      /* from the peer, not yet written to the local socket */
	size_t rxq_off;        /* bytes of rxq's first pbuf already written */
} tcp_conn;

struct tn_fwd {
	tn_ctx *c;
	struct netif nif;
	uint32_t addr;
	struct tcp_pcb *listen[8];
	uint32_t n_listen;
	udp_flow udp[UDP_FLOWS];
	tcp_conn tcp[TCP_CONNS];
	syn_wait syn[SYN_WAITS];
	uint8_t buf[TN_MTU];
	uint8_t rbuf[16384];
	uint64_t udp_in, udp_out, tcp_accepted;
};

static tn_fwd *g_fwd;      /* lwIP callbacks have no context of their own beyond `arg` */
static int g_lwip_ready;

unsigned tn_lwip_rand(void)
{
	unsigned v;
	tn_random(&v, sizeof v);
	return v;
}

u32_t sys_now(void) { return (u32_t)tn_now_ms(); }

static const tn_serve *rule_for(const tn_ctx *c, uint8_t proto, uint16_t port)
{
	for (uint32_t i = 0; i < c->n_serve; i++)
		if (c->serve[i].proto == proto && c->serve[i].port == port)
			return &c->serve[i];
	return NULL;
}

static uint16_t local_port(const tn_serve *r) { return r->local_port ? r->local_port : r->port; }

static void nonblock(int fd)
{
	fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK);
#ifdef SO_NOSIGPIPE
	int one = 1;
	setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof one);
#endif
}

/* ---------------- UDP relay ---------------- */

static void udp_close(udp_flow *f)
{
	if (f->fd >= 0)
		close(f->fd);
	memset(f, 0, sizeof *f);
	f->fd = -1;
}

static udp_flow *udp_flow_for(tn_fwd *w, uint32_t rip, uint16_t rport, uint16_t port, uint16_t lport, int64_t now)
{
	udp_flow *free_f = NULL, *oldest = NULL;
	for (int i = 0; i < UDP_FLOWS; i++) {
		udp_flow *f = &w->udp[i];
		if (f->fd >= 0 && f->rip == rip && f->rport == rport && f->port == port)
			return f;
		if (f->fd < 0 && !free_f)
			free_f = f;
		if (f->fd >= 0 && (!oldest || f->last < oldest->last))
			oldest = f;
	}
	udp_flow *f = free_f ? free_f : oldest;
	udp_close(f);
	int fd = socket(AF_INET, SOCK_DGRAM, 0);
	if (fd < 0)
		return NULL;
	nonblock(fd);
	struct sockaddr_in sa = {.sin_family = AF_INET, .sin_port = htons(lport), .sin_addr.s_addr = htonl(0x7f000001)};
	if (connect(fd, (struct sockaddr *)&sa, sizeof sa) != 0) {
		close(fd);
		return NULL;
	}
	f->fd = fd;
	f->rip = rip;
	f->rport = rport;
	f->port = port;
	f->last = now;
	return f;
}

static void udp_from_peer(tn_fwd *w, const tn_ip_info *ip, const tn_serve *r, int64_t now)
{
	udp_flow *f = udp_flow_for(w, ip->src, ip->sport, ip->dport, local_port(r), now);
	if (!f)
		return;
	f->last = now;
	if (send(f->fd, ip->payload, ip->plen, 0) < 0 && errno != EAGAIN && errno != EWOULDBLOCK &&
	    errno != ECONNREFUSED)
		udp_close(f); /* errno 163 (rest) and friends: the next datagram makes a new one */
	else
		w->udp_in++;
}

static void udp_from_local(tn_fwd *w, udp_flow *f, int64_t now)
{
	for (int i = 0; i < 64; i++) {
		ssize_t n = recv(f->fd, w->buf + 28, sizeof w->buf - 28, 0);
		if (n < 0) {
			if (errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR && errno != ECONNREFUSED)
				udp_close(f);
			return;
		}
		f->last = now;
		size_t m = tn_ip_build_udp(w->buf, sizeof w->buf, (tn_ep4){.ip = w->addr, .port = f->port},
					   (tn_ep4){.ip = f->rip, .port = f->rport}, w->buf + 28, (size_t)n);
		if (m && tn_dp_output(w->c, w->buf, m) == TN_OK)
			w->udp_out++;
	}
}

/* ---------------- TCP via lwIP ---------------- */

static void conn_free(tcp_conn *k)
{
	if (k->fd >= 0)
		close(k->fd);
	if (k->rxq)
		pbuf_free(k->rxq);
	memset(k, 0, sizeof *k);
	k->fd = -1;
}

static void conn_abort(tcp_conn *k)
{
	if (k->pcb) {
		tcp_arg(k->pcb, NULL);
		tcp_recv(k->pcb, NULL);
		tcp_sent(k->pcb, NULL);
		tcp_err(k->pcb, NULL);
		tcp_abort(k->pcb);
	}
	k->pcb = NULL;
	conn_free(k);
}

/* Returns 1 if lwIP had to abort the pcb (a callback must then return ERR_ABRT). */
static int conn_close(tcp_conn *k)
{
	int aborted = 0;
	if (k->pcb) {
		tcp_arg(k->pcb, NULL);
		tcp_recv(k->pcb, NULL);
		tcp_sent(k->pcb, NULL);
		tcp_err(k->pcb, NULL);
		if (tcp_close(k->pcb) != ERR_OK) {
			tcp_abort(k->pcb);
			aborted = 1;
		}
	}
	k->pcb = NULL;
	conn_free(k);
	return aborted;
}

/* Write what the peer sent to the local socket; ack it to lwIP as it goes. */
static void flush_rx(tcp_conn *k)
{
	while (k->rxq && !k->connecting) {
		struct pbuf *p = k->rxq;
		size_t left = p->len - k->rxq_off;
		ssize_t n = left ? send(k->fd, (uint8_t *)p->payload + k->rxq_off, left, MSG_NOSIGNAL) : 0;
		if (n < 0) {
			if (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR)
				return;
			conn_abort(k);
			return;
		}
		if (k->pcb && n > 0)
			tcp_recved(k->pcb, (u16_t)n);
		k->rxq_off += (size_t)n;
		if (k->rxq_off < p->len)
			return;
		k->rxq = p->next;
		if (k->rxq)
			pbuf_ref(k->rxq);
		pbuf_free(p); /* frees only the head: its next was referenced above */
		k->rxq_off = 0;
	}
	if (!k->rxq && k->remote_eof && !k->shut_wr && !k->connecting) {
		shutdown(k->fd, SHUT_WR);
		k->shut_wr = 1;
	}
}

static err_t on_recv(void *arg, struct tcp_pcb *pcb, struct pbuf *p, err_t err)
{
	(void)pcb;
	tcp_conn *k = arg;
	if (!k)
		return ERR_ABRT;
	if (err != ERR_OK && p) {
		pbuf_free(p);
		return ERR_OK;
	}
	if (!p) {
		k->remote_eof = 1;
	} else if (k->rxq) {
		pbuf_cat(k->rxq, p);
	} else {
		k->rxq = p;
		k->rxq_off = 0;
	}
	flush_rx(k);
	if (!k->used)
		return ERR_ABRT; /* flush_rx aborted the pcb */
	if (k->remote_eof && k->local_eof && !k->rxq)
		return conn_close(k) ? ERR_ABRT : ERR_OK;
	return ERR_OK;
}

static void on_err(void *arg, err_t err)
{
	(void)err;
	tcp_conn *k = arg;
	if (!k)
		return;
	k->pcb = NULL; /* lwIP freed it */
	conn_free(k);
}

static err_t on_accept(void *arg, struct tcp_pcb *pcb, err_t err)
{
	tn_fwd *w = arg;
	if (err != ERR_OK || !pcb)
		return ERR_VAL;
	const tn_serve *r = rule_for(w->c, TN_IPPROTO_TCP, pcb->local_port);
	tcp_conn *k = NULL;
	for (int i = 0; i < TCP_CONNS && !k; i++)
		if (!w->tcp[i].used)
			k = &w->tcp[i];
	if (!r || !k) {
		tcp_abort(pcb);
		return ERR_ABRT;
	}
	/* The local connection was dialed when the SYN arrived (syn_from_peer). */
	int fd = -1, rc = 0;
	uint32_t rip = ntohl(ip4_addr_get_u32(ip_2_ip4(&pcb->remote_ip)));
	for (int i = 0; i < SYN_WAITS && fd < 0; i++) {
		syn_wait *s = &w->syn[i];
		if (s->fd >= 0 && s->ready && s->rip == rip && s->rport == pcb->remote_port && s->port == pcb->local_port) {
			fd = s->fd;
			s->fd = -1;
		}
	}
	if (fd < 0) {
		tcp_abort(pcb);
		return ERR_ABRT;
	}
	memset(k, 0, sizeof *k);
	k->used = 1;
	k->fd = fd;
	k->pcb = pcb;
	k->connecting = rc != 0;
	k->started = tn_now_ms();
	tcp_arg(pcb, k);
	tcp_recv(pcb, on_recv);
	tcp_err(pcb, on_err);
	tcp_nagle_disable(pcb);
	w->tcp_accepted++;
	return ERR_OK;
}

/* Local socket readable: move bytes into lwIP while it has send buffer. */
static void tcp_from_local(tn_fwd *w, tcp_conn *k)
{
	while (k->used && k->pcb && !k->local_eof) {
		size_t room = tcp_sndbuf(k->pcb);
		if (room == 0 || tcp_sndqueuelen(k->pcb) >= TCP_SND_QUEUELEN - 2)
			break;
		if (room > sizeof w->rbuf)
			room = sizeof w->rbuf;
		ssize_t n = recv(k->fd, w->rbuf, room, 0);
		if (n > 0) {
			if (tcp_write(k->pcb, w->rbuf, (u16_t)n, TCP_WRITE_FLAG_COPY) != ERR_OK) {
				conn_abort(k);
				return;
			}
			continue;
		}
		if (n == 0) {
			k->local_eof = 1;
			break;
		}
		if (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR)
			break;
		conn_abort(k);
		return;
	}
	if (!k->used)
		return;
	if (k->pcb)
		tcp_output(k->pcb);
	if (k->local_eof && (k->remote_eof || !k->pcb)) {
		conn_close(k);
	} else if (k->local_eof && k->pcb && !k->fin_sent) {
		/* local side finished: send FIN after the queued data, keep receiving */
		k->fin_sent = 1;
		tcp_shutdown(k->pcb, 0, 1);
	}
}

static err_t nif_output(struct netif *nif, struct pbuf *p, const ip4_addr_t *dst)
{
	(void)nif;
	(void)dst;
	tn_fwd *w = g_fwd;
	if (!w || p->tot_len > sizeof w->buf)
		return ERR_BUF;
	pbuf_copy_partial(p, w->buf, p->tot_len, 0);
	tn_dp_output(w->c, w->buf, p->tot_len);
	return ERR_OK;
}

static err_t nif_init(struct netif *nif)
{
	nif->name[0] = 't';
	nif->name[1] = 'n';
	nif->mtu = TN_MTU;
	nif->output = nif_output;
	nif->flags = NETIF_FLAG_UP | NETIF_FLAG_LINK_UP;
	return ERR_OK;
}

static void lwip_in(tn_fwd *w, const uint8_t *pkt, size_t len)
{
	struct pbuf *p = pbuf_alloc(PBUF_RAW, (u16_t)len, PBUF_POOL);
	if (!p)
		return; /* out of buffers: a dropped segment, TCP retransmits */
	pbuf_take(p, pkt, (u16_t)len);
	if (w->nif.input(p, &w->nif) != ERR_OK)
		pbuf_free(p);
}

/* RST+ACK for a SYN whose local port refused (RFC 9293 3.10.7.1). */
static void send_rst(tn_fwd *w, const syn_wait *s)
{
	const uint8_t *t = s->syn + (s->syn[0] & 15) * 4;
	uint8_t pkt[40] = {0x45};
	tn_put_be16(pkt + 2, 40);
	pkt[8] = 64;
	pkt[9] = TN_IPPROTO_TCP;
	tn_put_be32(pkt + 12, w->addr);
	tn_put_be32(pkt + 16, s->rip);
	tn_put_be16(pkt + 10, tn_ip_csum(pkt, 20));
	uint8_t *h = pkt + 20;
	tn_put_be16(h, s->port);
	tn_put_be16(h + 2, s->rport);
	tn_put_be32(h + 8, tn_be32(t + 4) + 1); /* ack = their ISN + 1 */
	h[12] = 5 << 4;
	h[13] = 0x14; /* RST, ACK */
	tn_put_be16(h + 16, tn_l4_csum(w->addr, s->rip, TN_IPPROTO_TCP, h, 20));
	tn_dp_output(w->c, pkt, sizeof pkt);
}

static void syn_drop(syn_wait *s)
{
	if (s->fd >= 0)
		close(s->fd);
	memset(s, 0, sizeof *s);
	s->fd = -1;
}

static void syn_dialed(tn_fwd *w, syn_wait *s, int ok)
{
	if (!ok) {
		send_rst(w, s);
		syn_drop(s);
		return;
	}
	s->ready = 1;
	lwip_in(w, s->syn, s->len); /* lwIP answers SYN-ACK; on_accept takes s->fd */
}

/* A SYN for a served port: dial 127.0.0.1 first. Returns 1 if the SYN was taken
 * (held, refused or a duplicate), 0 to hand it to lwIP now. */
static int syn_from_peer(tn_fwd *w, const uint8_t *pkt, const tn_ip_info *ip, const tn_serve *r, int64_t now)
{
	syn_wait *s = NULL;
	for (int i = 0; i < SYN_WAITS; i++) {
		syn_wait *x = &w->syn[i];
		if (x->fd >= 0 && x->rip == ip->src && x->rport == ip->sport && x->port == ip->dport)
			return !x->ready; /* a retransmitted SYN: lwIP has it once we are dialed */
		if (x->fd < 0 && !s)
			s = x;
	}
	if (!s || ip->len > sizeof s->syn)
		return 1; /* too many connections opening at once: the peer retransmits */
	int fd = socket(AF_INET, SOCK_STREAM, 0);
	if (fd < 0)
		return 1;
	nonblock(fd);
	int one = 1;
	setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
	memset(s, 0, sizeof *s);
	s->fd = fd;
	s->rip = ip->src;
	s->rport = ip->sport;
	s->port = ip->dport;
	s->started = now;
	s->len = (uint16_t)ip->len;
	memcpy(s->syn, pkt, ip->len);
	struct sockaddr_in sa = {.sin_family = AF_INET, .sin_port = htons(local_port(r)),
				 .sin_addr.s_addr = htonl(0x7f000001)};
	if (connect(fd, (struct sockaddr *)&sa, sizeof sa) == 0)
		syn_dialed(w, s, 1);
	else if (errno != EINPROGRESS)
		syn_dialed(w, s, 0);
	return 1;
}

/* ---------------- API (dp.c) ---------------- */

int tn_fwd_create(tn_ctx *c)
{
	if (!c->n_serve)
		return TN_OK;
	if (g_lwip_ready && g_fwd)
		return TN_E_STATE; /* lwIP is global: one forwarder per process */
	tn_fwd *w = calloc(1, sizeof *w);
	if (!w)
		return TN_E_NOMEM;
	w->c = c;
	for (int i = 0; i < UDP_FLOWS; i++)
		w->udp[i].fd = -1;
	for (int i = 0; i < TCP_CONNS; i++)
		w->tcp[i].fd = -1;
	for (int i = 0; i < SYN_WAITS; i++)
		w->syn[i].fd = -1;
	if (!g_lwip_ready) {
		lwip_init();
		g_lwip_ready = 1;
	}
	g_fwd = w;
	ip4_addr_t a, m, gw;
	ip4_addr_set_zero(&a);
	IP4_ADDR(&m, 255, 255, 255, 255);
	ip4_addr_set_zero(&gw);
	netif_add(&w->nif, &a, &m, &gw, NULL, nif_init, ip4_input);
	netif_set_default(&w->nif);
	netif_set_up(&w->nif);
	netif_set_link_up(&w->nif);
	for (uint32_t i = 0; i < c->n_serve && w->n_listen < 8; i++) {
		if (c->serve[i].proto != TN_IPPROTO_TCP)
			continue;
		struct tcp_pcb *pcb = tcp_new();
		if (!pcb || tcp_bind(pcb, IP4_ADDR_ANY, c->serve[i].port) != ERR_OK) {
			if (pcb)
				tcp_close(pcb);
			continue;
		}
		struct tcp_pcb *l = tcp_listen_with_backlog(pcb, 4);
		if (!l)
			continue;
		tcp_arg(l, w);
		tcp_accept(l, on_accept);
		w->listen[w->n_listen++] = l;
	}
	c->fwd = w;
	c->footprint += sizeof *w;
	return TN_OK;
}

static void reset_all(tn_fwd *w)
{
	for (int i = 0; i < UDP_FLOWS; i++)
		if (w->udp[i].fd >= 0)
			udp_close(&w->udp[i]);
	for (int i = 0; i < TCP_CONNS; i++)
		if (w->tcp[i].used)
			conn_abort(&w->tcp[i]);
	for (int i = 0; i < SYN_WAITS; i++)
		if (w->syn[i].fd >= 0)
			syn_drop(&w->syn[i]);
}

void tn_fwd_destroy(tn_ctx *c)
{
	tn_fwd *w = c->fwd;
	if (!w)
		return;
	reset_all(w);
	for (uint32_t i = 0; i < w->n_listen; i++)
		tcp_close(w->listen[i]);
	netif_remove(&w->nif);
	g_fwd = NULL;
	free(w);
	c->fwd = NULL;
}

void tn_fwd_reset(tn_ctx *c)
{
	if (c->fwd)
		reset_all(c->fwd);
}

void tn_fwd_set_addr(tn_ctx *c, uint32_t addr)
{
	tn_fwd *w = c->fwd;
	if (!w || w->addr == addr)
		return;
	reset_all(w); /* connections to the old address are gone */
	w->addr = addr;
	ip4_addr_t a;
	a.addr = htonl(addr);
	netif_set_ipaddr(&w->nif, &a);
}

int tn_fwd_input(tn_ctx *c, const uint8_t *pkt, const tn_ip_info *ip, int64_t now)
{
	tn_fwd *w = c->fwd;
	if (!w)
		return 0;
	const tn_serve *r = rule_for(c, ip->proto, ip->dport);
	if (!r)
		return 0;
	if (ip->proto == TN_IPPROTO_UDP) {
		udp_from_peer(w, ip, r, now);
		return 1;
	}
	if ((ip->tcp_flags & 0x16) == 0x02 && syn_from_peer(w, pkt, ip, r, now))
		return 1; /* held until the local dial finishes */
	lwip_in(w, pkt, ip->len);
	return 1;
}

int tn_fwd_has_udp_flow(tn_ctx *c, uint32_t rip, uint16_t rport, uint16_t port)
{
	tn_fwd *w = c->fwd;
	for (int i = 0; w && i < UDP_FLOWS; i++)
		if (w->udp[i].fd >= 0 && w->udp[i].rip == rip && w->udp[i].rport == rport && w->udp[i].port == port)
			return 1;
	return 0;
}

int tn_fwd_pollfds(tn_ctx *c, struct pollfd *pf, int cap)
{
	tn_fwd *w = c->fwd;
	int n = 0;
	if (!w)
		return 0;
	for (int i = 0; i < UDP_FLOWS && n < cap; i++)
		if (w->udp[i].fd >= 0)
			pf[n++] = (struct pollfd){.fd = w->udp[i].fd, .events = POLLIN};
	for (int i = 0; i < SYN_WAITS && n < cap; i++)
		if (w->syn[i].fd >= 0 && !w->syn[i].ready)
			pf[n++] = (struct pollfd){.fd = w->syn[i].fd, .events = POLLOUT};
	for (int i = 0; i < TCP_CONNS && n < cap; i++) {
		tcp_conn *k = &w->tcp[i];
		if (!k->used)
			continue;
		short ev = 0;
		if (k->connecting || k->rxq)
			ev |= POLLOUT;
		if (!k->connecting && !k->local_eof && k->pcb && tcp_sndbuf(k->pcb) > 0)
			ev |= POLLIN;
		pf[n++] = (struct pollfd){.fd = k->fd, .events = ev};
	}
	return n;
}

void tn_fwd_io(tn_ctx *c, int fd, short revents, int64_t now)
{
	tn_fwd *w = c->fwd;
	if (!w)
		return;
	for (int i = 0; i < UDP_FLOWS; i++)
		if (w->udp[i].fd == fd) {
			udp_from_local(w, &w->udp[i], now);
			return;
		}
	for (int i = 0; i < SYN_WAITS; i++) {
		syn_wait *s = &w->syn[i];
		if (s->fd != fd || s->ready)
			continue;
		int soerr = 0;
		socklen_t sl = sizeof soerr;
		if (revents & (POLLOUT | POLLERR | POLLHUP))
			syn_dialed(w, s, getsockopt(fd, SOL_SOCKET, SO_ERROR, &soerr, &sl) == 0 && !soerr);
		return;
	}
	for (int i = 0; i < TCP_CONNS; i++) {
		tcp_conn *k = &w->tcp[i];
		if (!k->used || k->fd != fd)
			continue;
		if (k->connecting) {
			int soerr = 0;
			socklen_t sl = sizeof soerr;
			if (!(revents & (POLLOUT | POLLERR | POLLHUP)))
				return;
			if (getsockopt(fd, SOL_SOCKET, SO_ERROR, &soerr, &sl) != 0 || soerr) {
				conn_abort(k); /* the local service refused: reset the peer */
				return;
			}
			k->connecting = 0;
		}
		if (revents & POLLOUT)
			flush_rx(k);
		if (k->used && (revents & (POLLIN | POLLHUP | POLLERR)))
			tcp_from_local(w, k);
		if (k->used && k->remote_eof && k->local_eof && !k->rxq)
			conn_close(k);
		return;
	}
}

void tn_fwd_tick(tn_ctx *c, int64_t now)
{
	tn_fwd *w = c->fwd;
	if (!w)
		return;
	sys_check_timeouts();
	for (int i = 0; i < UDP_FLOWS; i++)
		if (w->udp[i].fd >= 0 && now - w->udp[i].last > UDP_IDLE_MS)
			udp_close(&w->udp[i]);
	for (int i = 0; i < SYN_WAITS; i++) {
		syn_wait *s = &w->syn[i];
		if (s->fd >= 0 && !s->ready && now - s->started > LOCAL_CONNECT_MS)
			syn_dialed(w, s, 0);
		else if (s->fd >= 0 && s->ready && now - s->started > 2 * LOCAL_CONNECT_MS)
			syn_drop(s); /* the handshake never completed in lwIP */
	}
	for (int i = 0; i < TCP_CONNS; i++) {
		tcp_conn *k = &w->tcp[i];
		if (k->used && k->connecting && now - k->started > LOCAL_CONNECT_MS)
			conn_abort(k);
		else if (k->used && !k->connecting && k->pcb)
			tcp_from_local(w, k); /* lwIP may have freed send buffer since */
	}
}

int tn_fwd_sleep_ms(tn_ctx *c)
{
	if (!c->fwd)
		return -1;
	u32_t t = sys_timeouts_sleeptime();
	return t == SYS_TIMEOUTS_SLEEPTIME_INFINITE ? -1 : (int)t;
}

void tn_fwd_stats(tn_ctx *c, uint32_t *udp_flows, uint32_t *tcp_conns)
{
	tn_fwd *w = c->fwd;
	*udp_flows = *tcp_conns = 0;
	for (int i = 0; w && i < UDP_FLOWS; i++)
		*udp_flows += w->udp[i].fd >= 0;
	for (int i = 0; w && i < TCP_CONNS; i++)
		*tcp_conns += (uint32_t)w->tcp[i].used;
}
