#include "ava1_server.h"
#include "ava1_pairlimit.h"

#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

#include "ava1_conn.h"
#include "ava1_frame.h"
#include "ava1_gen.h"
#include "ava1_noise.h"
#include "ava1_platform.h"
#include "ava1_store.h"
#include "monocypher.h"

#define MAX_SESSIONS 16
#define MAX_CONNS 64
#define CTRL_MAX 65536u
#define NONCES 64
#define RPC_WORKERS 8
#define RPC_OUT_MAX (256u * 1024u) /* SPEC.md §7.4: the largest reply body */
#define RPC_REQ_MAX (56u * 1024u)  /* ... and the largest request body */
#define MAX_PAIRING_WINDOW_S 600u
#define MAX_CONNS_PER_IP 12u
#define MAX_UNPAIRED 2u
#define PAIR_CONFIRM_MS 60000u
#define NOTIFY_EVERY_MS 10000u
#define THREAD_STACK (256u * 1024u)
#define MGMT_STACK (512u * 1024u) /* = AVA1_MGMT_STACK; the thread test pins both */

static const uint8_t PROLOGUE[] = { 'A', 'V', 'A', '1', ' ', 'v', '1' };

/* A connection shared by its reader thread and any RPC workers: freed by the last. */
typedef struct {
    ava1_conn_t io;
    int refs;    /* under mu */
    uint32_t ip; /* source address (network order), for the per-address limit */
    int ip_held; /* under mu: still counted against ip (given back early on supersede) */
    int member;  /* under mu: the session slot this connection is listed in, or -1 */
} conn_t;

/* Connections one session lists: its control, 8 lanes, and joins still proving
 * themselves. One that finds the list full simply is not listed (it is still counted
 * and released normally; it only misses the early release on supersede). */
#define MEMBERS 24

typedef struct {
    int used;
    int reserved; /* claimed by a handshake in progress; not yet joinable */
    uint8_t sid[16];
    uint8_t c2s[32];
    uint8_t s2c[32];
    int paired;
    uint32_t pair_code; /* the random code this session's screen shows (SPEC.md 5.5): never sent */
    uint8_t h[64];      /* the Noise handshake hash: part of the PAKE inputs */
    int pake_state;     /* 0 none, 1 PairPakeClient seen, 2 key ready, 3 spent by the confirm */
    uint8_t pake_k[32];
    int unpaired_hold;  /* reserved slot whose client is about to be welcomed unpaired */
    uint64_t since_ms;  /* when the session was welcomed */
    uint8_t peer_key[32];
    char peer_name[64];
    int rpc_inflight;
    uint32_t lane_gen[AVA1_MAX_LANES + 1];
    uint8_t nonces[NONCES][16];
    unsigned nonce_next;
    unsigned nonce_count;
    /* The same device connected again: this session is over. It keeps its slot until its
     * control thread leaves, but no lane may join it and it is no longer counted. */
    int superseded;
    conn_t *members[MEMBERS];
    /* The live connection per lane (lane 0 = control); each holds a reference
     * (ava1_server_send/_post/_lanes). */
    conn_t *conn[AVA1_MAX_LANES + 1];
} sess_t;

static struct {
    ava1_server_cfg_t cfg;
    int listen_fd;
    uint16_t port;
    pthread_t accept_thread;
    int accept_started;
    volatile int stopping;
    sess_t sessions[MAX_SESSIONS];
    int conns;
    uint64_t pairing_until_ms;
    ava1_peers_t peers;
    int peers_ok; /* 0: the peers file could not be read; never write it */
    /* Pairing limits (SPEC.md 5 item 5): real guesses per source address and in all, and the
     * per-address rate of new pairing sessions. */
    ava1_pairlimit_t pl;
    /* The notifications shown lately, so an identical request (same address, same key) is
     * not shown twice in a row; every other session shows its own code. */
    struct {
        uint32_t ip;
        uint8_t key[32];
        uint64_t at_ms;
        int used;
    } shown[8];
    struct {
        uint32_t ip;
        int n;
    } ips[MAX_CONNS];
} S = { .listen_fd = -1 };

static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
static __thread uint8_t rpc_peer[32];

const uint8_t *ava1_server_rpc_peer(void) { return rpc_peer; }
/* Serialises peers-file writes. Taken before mu, and held across the write so two
 * pairings land in order; mu itself is never held during file I/O. */
static pthread_mutex_t store_mu = PTHREAD_MUTEX_INITIALIZER;

static void slog(const char *fmt, ...) {
    char msg[256];
    va_list ap;
    if (!S.cfg.log) return;
    va_start(ap, fmt);
    vsnprintf(msg, sizeof msg, fmt, ap);
    va_end(ap);
    S.cfg.log(msg);
}

static uint32_t cfg_or(uint32_t v, uint32_t dflt) { return v ? v : dflt; }

/* Monotonic only: a settimeofday jump on the console must not age or revive anything. */
static uint64_t now_ms(void) { return ava1_now_ms(); }

static void set_timeouts(int fd, uint32_t ms) {
    struct timeval tv;
    tv.tv_sec = (time_t)(ms / 1000u);
    tv.tv_usec = (suseconds_t)((ms % 1000u) * 1000u);
    (void)setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv);
    (void)setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof tv);
}

static int spawn_detached_stack(void *(*fn)(void *), void *arg, size_t stack) {
    pthread_attr_t attr;
    pthread_t t;
    int rc;
    if (pthread_attr_init(&attr) != 0) return -1;
    (void)pthread_attr_setstacksize(&attr, stack);
    (void)pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    rc = pthread_create(&t, &attr, fn, arg);
    pthread_attr_destroy(&attr);
    return rc == 0 ? 0 : -1;
}

static int spawn_detached(void *(*fn)(void *), void *arg) { return spawn_detached_stack(fn, arg, THREAD_STACK); }

/* Management methods (4 and up, except the data plane's 16-19) call handlers written for the
 * FTX2 management thread, which had 512 KiB; everything else keeps the 256 KiB rule. */
static size_t rpc_stack(uint16_t method) {
    return (method >= 4 && !(method >= 16 && method <= 19)) ? MGMT_STACK : THREAD_STACK;
}

static void conn_get(conn_t *k) {
    pthread_mutex_lock(&mu);
    k->refs++;
    pthread_mutex_unlock(&mu);
}

/* Caller holds mu. Gives back k's place in its address's count, once. */
static void release_ip_locked(conn_t *k) {
    int i;
    if (!k->ip_held) return;
    k->ip_held = 0;
    for (i = 0; i < MAX_CONNS; i++) {
        if (S.ips[i].n > 0 && S.ips[i].ip == k->ip) {
            S.ips[i].n--;
            break;
        }
    }
}

/* Caller holds mu. Lists k in session idx, so a supersede can reach it. */
static void member_add_locked(int idx, conn_t *k) {
    int i;
    for (i = 0; i < MEMBERS; i++) {
        if (!S.sessions[idx].members[i]) {
            S.sessions[idx].members[i] = k;
            k->member = idx;
            return;
        }
    }
}

/* Takes k off its session's list; before k can be freed. */
static void member_drop(conn_t *k) {
    int i;
    pthread_mutex_lock(&mu);
    if (k->member >= 0) {
        for (i = 0; i < MEMBERS; i++)
            if (S.sessions[k->member].members[i] == k) S.sessions[k->member].members[i] = NULL;
        k->member = -1;
    }
    pthread_mutex_unlock(&mu);
}

/* Caller holds mu. One session per device (SPEC.md §8): a device that has just proved
 * its key in a new handshake replaces any session it still has. A client reconnecting
 * after its link died must not be refused because of its own dead connections, which
 * would otherwise linger until dead_after: they are shut down now (waking their
 * threads) and their per-address counts given back at once. */
static void supersede_locked(int keep, const uint8_t peer[32]) {
    int i, m;
    for (i = 0; i < MAX_SESSIONS; i++) {
        sess_t *s = &S.sessions[i];
        if (i == keep || !s->used || s->superseded || memcmp(s->peer_key, peer, 32) != 0) continue;
        s->superseded = 1;
        for (m = 0; m < MEMBERS; m++) {
            if (!s->members[m]) continue;
            release_ip_locked(s->members[m]);
            shutdown(s->members[m]->io.fd, SHUT_RDWR);
        }
    }
}

/* The last reference closes the socket and frees the connection. */
static void conn_put(conn_t *k) {
    int last;
    pthread_mutex_lock(&mu);
    last = --k->refs == 0;
    if (last) {
        S.conns--;
        release_ip_locked(k);
    }
    pthread_mutex_unlock(&mu);
    if (!last) return;
    /* The writer is joined before the fd is closed: an in-flight send must not be able
     * to hit an fd number the accept loop has already reused. Every path to the last
     * reference has shut the socket down first (conn_main, supersede_locked), so the
     * join is bounded. */
    ava1_conn_destroy(&k->io);
    close(k->io.fd);
    crypto_wipe(k, sizeof *k);
    free(k);
}

static int send_error(ava1_conn_t *c, uint16_t code, const char *msg) {
    ava1_error_t e;
    uint8_t b[320];
    ava1_w_t w;
    size_t n = strlen(msg);
    if (n > 256) n = 256;
    memset(&e, 0, sizeof e);
    e.code = code;
    e.message = (const uint8_t *)msg;
    e.message_len = (uint16_t)n;
    ava1_w_init(&w, b, sizeof b);
    if (ava1_error_encode(&e, &w) != 0) return -1;
    return ava1_conn_send(c, AVA1_TYPE_ERROR, 0, b, w.len);
}

/* Ping and Pong share their fields, so one encoder serves both. Liveness frames never
 * wait for the write lock: whoever holds it is sending data, which is proof of life. */
static int send_liveness(ava1_conn_t *c, uint8_t type, uint32_t seq, uint64_t t_us) {
    ava1_ping_t p;
    uint8_t b[32];
    ava1_w_t w;
    memset(&p, 0, sizeof p);
    p.seq = seq;
    p.t_us = t_us;
    ava1_w_init(&w, b, sizeof b);
    if (ava1_ping_encode(&p, &w) != 0) return -1;
    return ava1_conn_try_send(c, type, 0, b, w.len);
}

/* Printable ASCII only: names end up in the peers file and in notifications. */
static void clean_name(const uint8_t *p, uint16_t n, char out[64]) {
    size_t i, k = 0;
    for (i = 0; i < n && k < 63; i++) {
        uint8_t ch = p[i];
        out[k++] = (ch < 0x20 || ch >= 0x7f) ? '?' : (char)ch;
    }
    out[k] = 0;
}

/* Claims a slot before any handshake work, so the limit is enforced up front. */
static int sess_reserve(void) {
    int i, idx = -1;
    pthread_mutex_lock(&mu);
    for (i = 0; i < MAX_SESSIONS; i++) {
        if (!S.sessions[i].used && !S.sessions[i].reserved) {
            memset(&S.sessions[i], 0, sizeof S.sessions[i]);
            S.sessions[i].reserved = 1;
            idx = i;
            break;
        }
    }
    pthread_mutex_unlock(&mu);
    return idx;
}

static void sess_release(int idx) {
    pthread_mutex_lock(&mu);
    if (!S.sessions[idx].used) {
        S.sessions[idx].reserved = 0;
        S.sessions[idx].unpaired_hold = 0;
    }
    pthread_mutex_unlock(&mu);
}

/* Caller holds mu. Sessions welcomed (or about to be) without being paired. */
static unsigned unpaired_locked(void) {
    unsigned n = 0;
    int i;
    for (i = 0; i < MAX_SESSIONS; i++) {
        const sess_t *s = &S.sessions[i];
        if ((s->used && !s->paired && !s->superseded) || (!s->used && s->reserved && s->unpaired_hold)) n++;
    }
    return n;
}

/* Fills a slot from sess_reserve; k is its control connection. */
static void sess_fill(int idx, conn_t *k, const uint8_t sid[16], const uint8_t c2s[32], const uint8_t s2c[32],
                      int paired, const uint8_t peer[32], const char *name) {
    pthread_mutex_lock(&mu);
    {
        sess_t *s = &S.sessions[idx];
        memset(s, 0, sizeof *s);
        member_add_locked(idx, k);
        s->used = 1;
        memcpy(s->sid, sid, 16);
        memcpy(s->c2s, c2s, 32);
        memcpy(s->s2c, s2c, 32);
        s->paired = paired;
        s->since_ms = now_ms();
        memcpy(s->peer_key, peer, 32);
        snprintf(s->peer_name, sizeof s->peer_name, "%s", name);
    }
    pthread_mutex_unlock(&mu);
}

static void sess_remove(int idx, const uint8_t sid[16]) {
    conn_t *held[AVA1_MAX_LANES + 1];
    int n = 0;
    uint16_t l;
    pthread_mutex_lock(&mu);
    if (S.sessions[idx].used && memcmp(S.sessions[idx].sid, sid, 16) == 0) {
        /* The lane threads each hold their own reference and withdraw it themselves;
         * these are the registry's references, released after the slot is wiped. */
        for (l = 0; l <= AVA1_MAX_LANES; l++)
            if (S.sessions[idx].conn[l]) held[n++] = S.sessions[idx].conn[l];
        crypto_wipe(&S.sessions[idx], sizeof S.sessions[idx]);
    }
    pthread_mutex_unlock(&mu);
    while (n > 0) conn_put(held[--n]);
}

/* Caller holds mu. A live (not superseded) session. */
static int sess_find_locked(const uint8_t sid[16]) {
    int i;
    for (i = 0; i < MAX_SESSIONS; i++)
        if (S.sessions[i].used && !S.sessions[i].superseded && memcmp(S.sessions[i].sid, sid, 16) == 0)
            return i;
    return -1;
}

/* Caller holds mu. */
static int sess_is_locked(int idx, const uint8_t sid[16]) {
    return S.sessions[idx].used && memcmp(S.sessions[idx].sid, sid, 16) == 0;
}

static int lane_current(int idx, const uint8_t sid[16], uint16_t lane, uint32_t gen) {
    int ok;
    pthread_mutex_lock(&mu);
    ok = sess_is_locked(idx, sid) && !S.sessions[idx].superseded && S.sessions[idx].lane_gen[lane] == gen;
    pthread_mutex_unlock(&mu);
    return ok;
}

static int is_data_type(uint8_t t) { return t >= 0x20 && t <= 0x3F; }

/* Publishes `k` as the session's connection for `lane` (taking a reference). */
static void conn_publish(int idx, const uint8_t sid[16], uint16_t lane, conn_t *k) {
    conn_t *old = NULL;
    if (lane > AVA1_MAX_LANES) return;
    pthread_mutex_lock(&mu);
    if (sess_is_locked(idx, sid)) {
        old = S.sessions[idx].conn[lane];
        S.sessions[idx].conn[lane] = k;
        k->refs++;
    }
    pthread_mutex_unlock(&mu);
    if (old) conn_put(old);
}

/* Withdraws `k` if it is still the published connection for `lane`. */
static void conn_withdraw(int idx, const uint8_t sid[16], uint16_t lane, conn_t *k) {
    int mine = 0;
    if (lane > AVA1_MAX_LANES) return;
    pthread_mutex_lock(&mu);
    if (sess_is_locked(idx, sid) && S.sessions[idx].conn[lane] == k) {
        S.sessions[idx].conn[lane] = NULL;
        mine = 1;
    }
    pthread_mutex_unlock(&mu);
    if (mine) conn_put(k);
}

/* A referenced connection for (sid, lane), or NULL. Release with conn_put. */
static conn_t *conn_lookup(const uint8_t sid[16], uint16_t lane) {
    conn_t *k = NULL;
    int idx;
    if (lane > AVA1_MAX_LANES) return NULL;
    pthread_mutex_lock(&mu);
    idx = sess_find_locked(sid);
    if (idx >= 0 && S.sessions[idx].conn[lane]) {
        k = S.sessions[idx].conn[lane];
        k->refs++;
    }
    pthread_mutex_unlock(&mu);
    return k;
}

int ava1_server_send(const uint8_t sid[16], uint16_t lane, uint8_t type, uint8_t flags, uint32_t channel,
                     const uint8_t *body, size_t len) {
    conn_t *k = conn_lookup(sid, lane);
    int rc;
    if (!k) return AVA1_E_CLOSED;
    rc = ava1_conn_send_flags(&k->io, type, flags, channel, body, len);
    conn_put(k);
    return rc;
}

int ava1_server_send_frame(const uint8_t sid[16], uint16_t lane, uint8_t type, uint32_t channel, uint8_t *frame,
                           size_t body_len) {
    conn_t *k = conn_lookup(sid, lane);
    int rc;
    if (!k) return AVA1_E_CLOSED;
    rc = ava1_conn_send_frame(&k->io, type, channel, frame, body_len);
    conn_put(k);
    return rc;
}

int ava1_server_post(const uint8_t sid[16], uint16_t lane, uint8_t type, uint8_t flags, uint32_t channel,
                     const uint8_t *body, size_t len) {
    conn_t *k = conn_lookup(sid, lane);
    int rc;
    if (!k) return AVA1_E_CLOSED;
    rc = ava1_conn_post(&k->io, type, flags, channel, body, len);
    conn_put(k);
    return rc;
}

/* Free entries of the session's control post queue: a hook decides whether a post can
 * still be absorbed or must wait for the socket instead (a full queue closes the
 * connection). -1: no such session. */
int ava1_server_post_room(const uint8_t sid[16]) {
    conn_t *k = conn_lookup(sid, 0);
    int room;
    if (!k) return -1;
    pthread_mutex_lock(&k->io.qmu);
    room = (int)AVA1_Q_ENTRIES - (int)k->io.q_n;
    pthread_mutex_unlock(&k->io.qmu);
    conn_put(k);
    return room;
}

int ava1_server_lanes(const uint8_t sid[16], uint16_t out[AVA1_MAX_LANES]) {
    int idx, n = 0;
    uint16_t l;
    pthread_mutex_lock(&mu);
    idx = sess_find_locked(sid);
    if (idx >= 0)
        for (l = 1; l <= AVA1_MAX_LANES; l++)
            if (S.sessions[idx].conn[l]) out[n++] = l;
    pthread_mutex_unlock(&mu);
    return n;
}

/* Caller holds mu. */
static int nonce_seen(const sess_t *s, const uint8_t n[16]) {
    unsigned i;
    for (i = 0; i < s->nonce_count; i++)
        if (memcmp(s->nonces[i], n, 16) == 0) return 1;
    return 0;
}

/* Caller holds mu. */
static void nonce_add(sess_t *s, const uint8_t n[16]) {
    memcpy(s->nonces[s->nonce_next], n, 16);
    s->nonce_next = (s->nonce_next + 1) % NONCES;
    if (s->nonce_count < NONCES) s->nonce_count++;
}

static int send_status(ava1_conn_t *c, uint32_t ch, uint16_t status, const uint8_t *body, size_t len) {
    ava1_rpc_response_t resp;
    ava1_w_t w;
    uint8_t *frame = malloc(len + 64);
    int rc;
    if (!frame) return AVA1_E_IO;
    memset(&resp, 0, sizeof resp);
    resp.status = status;
    resp.body = body;
    resp.body_len = (uint32_t)len;
    ava1_w_init(&w, frame, len + 64);
    rc = ava1_rpc_response_encode(&resp, &w);
    if (rc == 0) rc = ava1_conn_send(c, AVA1_TYPE_RPC_RESPONSE, ch, frame, w.len);
    free(frame);
    return rc;
}

/* A wrong guess at the code: the PAKE was exchanged and the client's confirmation did not
 * verify. Logged per source address; the address's budget shrinks, and when the global cap on
 * guesses is spent the window closes until a paired device (or a restart) reopens it. The
 * address's next knock shows a new code at once. Caller holds mu. */
static void guess_failed_locked(const sess_t *s, uint32_t ip) {
    uint32_t n = 0;
    unsigned i;
    int close_window = ava1_pl_guess_failed(&S.pl, ip, now_ms(), &n);
    slog("ava1: pairing refused: wrong code from %s at %u.%u.%u.%u (%u of %u from this address, %u of %u in all)",
         s->peer_name, ip & 0xffu, (ip >> 8) & 0xffu, (ip >> 16) & 0xffu, (ip >> 24) & 0xffu, n, S.pl.per_ip_max,
         S.pl.total, S.pl.total_max);
    for (i = 0; i < 8; i++)
        if (S.shown[i].used && S.shown[i].ip == ip) S.shown[i].used = 0;
    if (close_window) {
        S.pairing_until_ms = 0;
        slog("ava1: too many wrong pairing codes: the window is closed");
    }
}

/* SPEC.md 5.5, the first half of the pairing: the client's public value arrives, ours is
 * computed from the code only this console's screen shows. Once per session. 1 closes. */
static int pair_pake(ava1_conn_t *c, int idx, uint32_t ip, uint32_t ch, const uint8_t *body, size_t len) {
    ava1_pair_pake_client_t m;
    ava1_pair_pake_server_t r;
    uint8_t h[64], x[32], g[32], yb[32], k[32], b[64];
    uint32_t code;
    ava1_w_t w;
    int ok = 0, rc;
    pthread_mutex_lock(&mu);
    {
        sess_t *s = &S.sessions[idx];
        if (s->paired || s->pake_state) {
            pthread_mutex_unlock(&mu);
            return 1;
        }
        s->pake_state = 1;
        if (now_ms() >= S.pairing_until_ms) {
            pthread_mutex_unlock(&mu);
            (void)send_error(c, AVA1_ERR_PAIRING_CLOSED, "pairing is closed");
            return 1;
        }
        if (!ava1_pl_guess_allowed(&S.pl, ip, now_ms())) {
            pthread_mutex_unlock(&mu);
            (void)send_error(c, AVA1_ERR_PAIRING_CLOSED, "too many wrong codes from this address");
            return 1;
        }
        memcpy(h, s->h, 64);
        code = s->pair_code;
    }
    pthread_mutex_unlock(&mu);
    /* Nothing below reveals anything about the code, so none of it counts as a guess. */
    if (ava1_pair_pake_client_decode(body, len, &m) != 0 || ava1_platform_random(x, 32) != 0) return 1;
    ava1_cpace_generator(h, code, g);
    if (ava1_cpace_public(x, g, yb) == 0 && ava1_cpace_key(h, x, m.y, m.y, yb, k) == 0) ok = 1;
    crypto_wipe(x, sizeof x);
    if (!ok) return 1;
    pthread_mutex_lock(&mu);
    memcpy(S.sessions[idx].pake_k, k, 32);
    S.sessions[idx].pake_state = 2;
    pthread_mutex_unlock(&mu);
    crypto_wipe(k, sizeof k);
    memset(&r, 0, sizeof r);
    memcpy(r.y, yb, 32);
    ava1_w_init(&w, b, sizeof b);
    rc = ava1_pair_pake_server_encode(&r, &w);
    if (rc == 0) rc = ava1_conn_send(c, AVA1_TYPE_PAIR_PAKE_SERVER, ch, b, w.len);
    return rc != 0;
}

/* SPEC.md 5.5, the second half: the client's key confirmation must verify (it knew the
 * code), and ours goes back so the client can tell this is the console it meant. A refusal
 * is counted and ends the session: one attempt each. */
static int pair_confirm(ava1_conn_t *c, int idx, uint32_t ip, uint32_t ch, const uint8_t *body, size_t len) {
    ava1_pair_result_t r;
    ava1_pair_confirm_t m;
    uint8_t b[64], k[32], h[64], want[32];
    ava1_w_t w;
    int accepted = 0, attempt = 0, have_k = 0, was_paired = 0;
    ava1_peers_t *next = malloc(sizeof *next);
    memset(k, 0, sizeof k);
    memset(h, 0, sizeof h);
    memset(want, 0, sizeof want);
    pthread_mutex_lock(&store_mu);
    pthread_mutex_lock(&mu);
    {
        sess_t *s = &S.sessions[idx];
        if (s->paired) {
            accepted = 1;
            was_paired = 1;
        } else {
            if (s->pake_state == 2) {
                memcpy(k, s->pake_k, 32);
                memcpy(h, s->h, 64);
                have_k = 1;
            }
            crypto_wipe(s->pake_k, sizeof s->pake_k);
            s->pake_state = 3;
            if (next && S.peers_ok && now_ms() < S.pairing_until_ms) {
                int proof_ok = 0;
                if (have_k && ava1_pair_confirm_decode(body, len, &m) == 0) {
                    ava1_cpace_mac(k, 0, h, want);
                    proof_ok = ava1_ct_eq32(want, m.mac);
                    /* The PAKE ran and the proof is wrong: a real guess. A confirm with no PAKE
                     * before it, or one that does not decode, guessed nothing: refused, free. */
                    if (!proof_ok) guess_failed_locked(s, ip);
                }
                if (proof_ok) {
                    *next = S.peers;
                    ava1_peers_put(next, s->peer_key, s->peer_name, (uint64_t)time(NULL));
                    attempt = 1;
                }
            }
        }
    }
    pthread_mutex_unlock(&mu);
    if (attempt) {
        /* The write and its fsync run outside mu: every other connection keeps going. */
        int rc = ava1_peers_save(next, S.cfg.peers_path);
        if (rc == 0) {
            pthread_mutex_lock(&mu);
            S.peers = *next;
            S.sessions[idx].paired = 1;
            /* One window, one pairing: whoever else is waiting must ask again. */
            S.pairing_until_ms = 0;
            pthread_mutex_unlock(&mu);
            accepted = 1;
        } else {
            slog("ava1: pairing not stored: cannot write %s", S.cfg.peers_path);
        }
    }
    pthread_mutex_unlock(&store_mu);
    free(next);
    memset(&r, 0, sizeof r);
    r.accepted = (uint8_t)accepted;
    if (accepted && !was_paired) ava1_cpace_mac(k, 1, h, r.mac);
    crypto_wipe(k, sizeof k);
    ava1_w_init(&w, b, sizeof b);
    if (ava1_pair_result_encode(&r, &w) != 0 || ava1_conn_send(c, AVA1_TYPE_PAIR_RESULT, ch, b, w.len) != 0)
        return 1;
    return accepted ? 0 : 1;
}

typedef struct {
    conn_t *k;
    int idx;
    uint8_t sid[16];
    uint8_t peer[32];
    uint32_t ch;
    uint16_t method;
    uint8_t *body;
    uint32_t body_len;
} rpc_job_t;

static void *rpc_worker(void *arg) {
    rpc_job_t *j = arg;
    uint8_t *out = malloc(RPC_OUT_MAX);
    size_t out_len = 0;
    int status = AVA1_ERR_INTERNAL;
    memcpy(rpc_peer, j->peer, sizeof rpc_peer);
    if (out && S.cfg.rpc) status = S.cfg.rpc(j->method, j->body, j->body_len, out, RPC_OUT_MAX, &out_len);
    else if (out) status = AVA1_ERR_UNKNOWN_METHOD;
    /* A handler that claims more than the buffer holds must not make us read past it. */
    if (out_len > RPC_OUT_MAX) {
        static const char cause[] = "reply exceeds the 256 KiB RPC cap";
        out_len = 0;
        status = AVA1_ERR_INTERNAL;
        if (out) {
            memcpy(out, cause, sizeof cause - 1);
            out_len = sizeof cause - 1;
        }
    }
    if (status != AVA1_STATUS_OK)
        fprintf(stderr, "[ava1] rpc method %u -> status %d, %zu byte cause: %.*s\n", (unsigned)j->method, status, out_len,
                (int)(out_len > 200 ? 200 : out_len), out ? (const char *)out : "");
    (void)send_status(&j->k->io, j->ch, (uint16_t)status, out, out_len); /* an error carries its cause as UTF-8 text */
    pthread_mutex_lock(&mu);
    if (sess_is_locked(j->idx, j->sid)) S.sessions[j->idx].rpc_inflight--;
    pthread_mutex_unlock(&mu);
    free(out);
    free(j->body);
    conn_put(j->k);
    free(j);
    return NULL;
}

/* Answers on the reader only what is instant (refusals, pairing.open); everything
 * else runs on a worker so liveness never waits for a call. */
static int do_rpc(conn_t *k, int idx, const uint8_t sid[16], uint32_t ch, const uint8_t *body, size_t len) {
    ava1_rpc_request_t q;
    rpc_job_t *j;
    int paired, slot;
    if (ava1_rpc_request_decode(body, len, &q) != 0) {
        (void)send_error(&k->io, AVA1_ERR_PROTOCOL, "bad RpcRequest");
        return 1;
    }
    pthread_mutex_lock(&mu);
    paired = S.sessions[idx].paired;
    slot = paired && S.sessions[idx].rpc_inflight < RPC_WORKERS;
    if (slot) S.sessions[idx].rpc_inflight++;
    pthread_mutex_unlock(&mu);
    if (!paired) return send_status(&k->io, ch, AVA1_ERR_NOT_PAIRED, NULL, 0) != 0;
    if (q.body_len > RPC_REQ_MAX) {
        static const char cause[] = "request exceeds the 56 KiB RPC cap";
        if (slot) {
            pthread_mutex_lock(&mu);
            S.sessions[idx].rpc_inflight--;
            pthread_mutex_unlock(&mu);
        }
        return send_status(&k->io, ch, AVA1_ERR_PROTOCOL, (const uint8_t *)cause, sizeof cause - 1) != 0;
    }
    if (q.method == AVA1_METHOD_PAIRING_OPEN) {
        ava1_pairing_open_t o;
        uint16_t st = AVA1_ERR_PROTOCOL;
        if (slot) {
            pthread_mutex_lock(&mu);
            S.sessions[idx].rpc_inflight--;
            pthread_mutex_unlock(&mu);
        }
        if (ava1_pairing_open_decode(q.body, q.body_len, &o) == 0) {
            ava1_server_open_pairing(o.seconds > MAX_PAIRING_WINDOW_S ? MAX_PAIRING_WINDOW_S : o.seconds);
            st = AVA1_STATUS_OK;
        }
        return send_status(&k->io, ch, st, NULL, 0) != 0;
    }
    if (!slot) return send_status(&k->io, ch, AVA1_ERR_BUSY, NULL, 0) != 0;
    j = calloc(1, sizeof *j);
    if (j) j->body = malloc(q.body_len ? q.body_len : 1);
    if (!j || !j->body) {
        if (j) free(j);
        pthread_mutex_lock(&mu);
        S.sessions[idx].rpc_inflight--;
        pthread_mutex_unlock(&mu);
        return send_status(&k->io, ch, AVA1_ERR_INTERNAL, NULL, 0) != 0;
    }
    if (q.body_len) memcpy(j->body, q.body, q.body_len);
    j->body_len = q.body_len;
    j->k = k;
    j->idx = idx;
    memcpy(j->sid, sid, 16);
    pthread_mutex_lock(&mu);
    if (sess_is_locked(idx, sid)) memcpy(j->peer, S.sessions[idx].peer_key, 32);
    pthread_mutex_unlock(&mu);
    j->ch = ch;
    j->method = q.method;
    conn_get(k);
    if (spawn_detached_stack(rpc_worker, j, rpc_stack(q.method)) != 0) {
        conn_put(k);
        free(j->body);
        free(j);
        pthread_mutex_lock(&mu);
        S.sessions[idx].rpc_inflight--;
        pthread_mutex_unlock(&mu);
        return send_status(&k->io, ch, AVA1_ERR_BUSY, NULL, 0) != 0;
    }
    return 0;
}

/* 0 = keep going; nonzero = close the connection. */
static int handle_frame(conn_t *k, int idx, const uint8_t sid[16], uint16_t lane, uint8_t type,
                        uint8_t flags, uint32_t ch, const uint8_t *body, size_t len) {
    if (is_data_type(type)) {
        int paired;
        uint8_t peer[32];
        if (lane != 0) return 0; /* lane data frames are handled in serve_loop */
        pthread_mutex_lock(&mu);
        paired = sess_is_locked(idx, sid) && S.sessions[idx].paired;
        if (paired) memcpy(peer, S.sessions[idx].peer_key, 32);
        pthread_mutex_unlock(&mu);
        if (!paired) { /* SPEC.md §5: nothing but PairConfirm before the pairing is accepted */
            (void)send_error(&k->io, AVA1_ERR_NOT_PAIRED, "pair first");
            return 1;
        }
        if (!S.cfg.data || !S.cfg.data->on_control) return 0;
        return S.cfg.data->on_control(sid, peer, type, flags, body, len) != 0;
    }
    switch (type) {
    case AVA1_TYPE_PING: {
        ava1_ping_t p;
        if (ava1_ping_decode(body, len, &p) != 0) return 1;
        return send_liveness(&k->io, AVA1_TYPE_PONG, p.seq, p.t_us) == AVA1_E_IO;
    }
    case AVA1_TYPE_PONG:
        return 0;
    case AVA1_TYPE_PAIR_PAKE_CLIENT:
        if (lane != 0) break;
        return pair_pake(&k->io, idx, k->ip, ch, body, len);
    case AVA1_TYPE_PAIR_CONFIRM:
        if (lane != 0) break;
        return pair_confirm(&k->io, idx, k->ip, ch, body, len);
    case AVA1_TYPE_RPC_REQUEST:
        if (lane != 0) break;
        return do_rpc(k, idx, sid, ch, body, len);
    case AVA1_TYPE_BYE:
    case AVA1_TYPE_ERROR:
        return 1;
    default:
        if (flags & AVA1_FLAG_IGNORABLE) return 0;
        break;
    }
    (void)send_error(&k->io, AVA1_ERR_PROTOCOL, "unexpected frame");
    return 1;
}

/* The default slowest a frame may arrive (after a dead_after grace) before the peer
 * counts as dead: slow links are fine, a peer dripping one frame forever is not (§6). */
#define MIN_FRAME_RATE 8192u

typedef struct {
    conn_t *k;
    int idx;
    const uint8_t *sid;
    uint16_t lane;
    uint32_t gen;
    uint64_t last_ping;
    uint32_t ping_seq;
} serve_t;

/* Runs while the reader waits for bytes, also between the pieces of one large frame:
 * sends a due Ping (never waiting for the write lock: whoever holds it is sending data,
 * which is proof of life) and ends the connection on stop or when superseded. */
static int serve_tick(void *arg) {
    serve_t *x = arg;
    uint64_t t = now_ms();
    if (S.stopping) return 1;
    if (x->lane != 0 && !lane_current(x->idx, x->sid, x->lane, x->gen)) return 1;
    if (x->lane == 0) {
        /* An unconfirmed session is only useful while it can still be confirmed: it ends
         * with the pairing window, or after the confirm deadline. */
        int expired;
        pthread_mutex_lock(&mu);
        expired = !S.sessions[x->idx].paired &&
                  (t >= S.pairing_until_ms ||
                   t - S.sessions[x->idx].since_ms > cfg_or(S.cfg.pair_confirm_ms, PAIR_CONFIRM_MS));
        pthread_mutex_unlock(&mu);
        if (expired) {
            (void)send_error(&x->k->io, AVA1_ERR_PAIRING_CLOSED, "pairing was not confirmed in time");
            return 1;
        }
    }
    if (t - x->last_ping >= S.cfg.ping_every_ms) {
        if (send_liveness(&x->k->io, AVA1_TYPE_PING, ++x->ping_seq, t * 1000u) == AVA1_E_IO) return 1;
        x->last_ping = t;
    }
    return 0;
}

/* Liveness: dead after dead_after with no byte received (a frame in progress counts),
 * or when a frame arrives slower than MIN_FRAME_RATE. Writes wait at most dead_after
 * for the peer to make room, so no reply can wedge this reader: a peer that stops
 * reading has its connection broken (shut down), which ends the read below too. */
static void serve_loop(conn_t *k, int idx, const uint8_t sid[16], uint16_t lane, uint32_t gen, uint8_t *buf) {
    serve_t x;
    memset(&x, 0, sizeof x);
    x.k = k;
    x.idx = idx;
    x.sid = sid;
    x.lane = lane;
    x.gen = gen;
    k->io.deadline_ms = 0;
    set_timeouts(k->io.fd, S.cfg.dead_after_ms);
    pthread_mutex_lock(&k->io.wmu);
    k->io.send_idle_ms = S.cfg.dead_after_ms;
    k->io.min_rate = S.cfg.min_frame_rate ? S.cfg.min_frame_rate : MIN_FRAME_RATE;
    pthread_mutex_unlock(&k->io.wmu);
    k->io.idle_ms = S.cfg.dead_after_ms;
    k->io.last_rx_ms = now_ms();
    k->io.tick = serve_tick;
    k->io.tick_arg = &x;
    k->io.tick_ms = S.cfg.ping_every_ms;
    while (serve_tick(&x) == 0) {
        ava1_header_t h;
        size_t blen;
        uint8_t *heap = NULL;
        if (ava1_conn_recv_header(&k->io, &h, &blen) != 0) break;
        if (lane != 0 && is_data_type(h.type)) {
            /* A lane data frame's body never touches the 64 KiB control buffer: it is
             * admitted against credit, read into its own heap buffer and handed over. */
            const ava1_data_hooks_t *dh = S.cfg.data;
            if (!dh || !dh->admit || !dh->on_lane || dh->admit(sid, lane, blen) != 0) break;
            heap = ava1_frame_alloc(blen, NULL); /* pooled: released with ava1_frame_free(heap, ava1_frame_cap(blen)) */
            if (!heap || ava1_conn_recv_body(&k->io, heap, blen) != 0) {
                (void)ava1_frame_free(heap, ava1_frame_cap(blen));
                dh->on_lane(sid, lane, h.type, h.channel, NULL, blen);
                break;
            }
            if (dh->on_lane(sid, lane, h.type, h.channel, heap, blen) != 0) break;
            continue;
        }
        if (blen > CTRL_MAX || ava1_conn_recv_body(&k->io, buf, blen) != 0) break;
        if (handle_frame(k, idx, sid, lane, h.type, h.flags, h.channel, buf, blen) != 0) break;
    }
    k->io.tick = NULL;
}

static int send_noise(ava1_conn_t *c, uint8_t type, const uint8_t *msg, size_t n) {
    ava1_hs2_t m; /* Hs1/Hs2/Hs3 share one shape */
    uint8_t out[700];
    ava1_w_t w;
    memset(&m, 0, sizeof m);
    m.noise = msg;
    m.noise_len = (uint32_t)n;
    ava1_w_init(&w, out, sizeof out);
    if (ava1_hs2_encode(&m, &w) != 0) return -1;
    return ava1_conn_send(c, type, 0, out, w.len);
}

/* Hs1 already in buf. Noise XX as responder, then Welcome or a sealed refusal. */
static void run_control(conn_t *k, uint8_t *buf, size_t len) {
    ava1_hs1_t m1;
    ava1_hs3_t m3;
    ava1_hello_info_t hello;
    ava1_server_info_t si;
    ava1_client_info_t ci;
    ava1_welcome_t wel;
    ava1_noise_t ns;
    ava1_identity_t eph;
    uint8_t secret[32], sid[16], nonce_s[16], pl[512], msg[600], c2s[32], s2c[32];
    char peer_name[64];
    size_t pn, mn;
    uint8_t type, flags;
    uint32_t ch;
    ava1_w_t w;
    int known, open, busy = 0, notify = 0, idx, filled = 0, ci_ok;
    uint32_t pair_code = 0;

    memset(&ns, 0, sizeof ns);
    memset(&eph, 0, sizeof eph);
    memset(c2s, 0, sizeof c2s);
    memset(s2c, 0, sizeof s2c);
    idx = sess_reserve();
    if (idx < 0) {
        (void)send_error(&k->io, AVA1_ERR_BUSY, "too many sessions");
        return;
    }
    if (ava1_hs1_decode(buf, len, &m1) != 0) {
        (void)send_error(&k->io, AVA1_ERR_PROTOCOL, "bad Hs1");
        goto out;
    }
    if (ava1_platform_random(secret, 32) != 0 || ava1_platform_random(sid, 16) != 0 ||
        ava1_platform_random(nonce_s, 16) != 0) {
        (void)send_error(&k->io, AVA1_ERR_INTERNAL, "no random source");
        goto out;
    }
    ava1_identity_from_secret(&eph, secret);
    crypto_wipe(secret, sizeof secret);
    ava1_noise_init(&ns, 0, &S.cfg.identity, &eph, PROLOGUE, sizeof PROLOGUE);
    if (ava1_noise_read(&ns, m1.noise, m1.noise_len, pl, sizeof pl, &pn) != 0 ||
        ava1_hello_info_decode(pl, pn, &hello) != 0) {
        (void)send_error(&k->io, AVA1_ERR_PROTOCOL, "bad handshake");
        goto out;
    }
    if (hello.version_min > AVA1_PROTOCOL_VERSION || hello.version_max < AVA1_PROTOCOL_VERSION) {
        (void)send_error(&k->io, AVA1_ERR_UNSUPPORTED_VERSION, "no protocol version in common");
        goto out;
    }
    memset(&si, 0, sizeof si);
    si.version = AVA1_PROTOCOL_VERSION;
    si.caps = S.cfg.caps;
    /* A server without data hooks cannot serve the data plane, whatever the caller asked
     * for: the caps are what a client is entitled to rely on. */
    if (!S.cfg.data) si.caps &= ~AVA1_CAP_DATA_PLANE;
    memcpy(si.session_id, sid, 16);
    /* Commit to the pairing nonce before the client has shown its own (SPEC.md 4.6). */
    ava1_pair_commit(nonce_s, si.pair_commit);
    si.has_name = 1;
    si.name = (const uint8_t *)S.cfg.name;
    si.name_len = (uint16_t)strlen(S.cfg.name);
    ava1_w_init(&w, pl, sizeof pl);
    if (ava1_server_info_encode(&si, &w) != 0 ||
        ava1_noise_write(&ns, pl, w.len, msg, sizeof msg, &mn) != 0 ||
        send_noise(&k->io, AVA1_TYPE_HS2, msg, mn) != 0)
        goto out;
    if (ava1_conn_recv(&k->io, &type, &flags, &ch, buf, CTRL_MAX, &len) != 0 || type != AVA1_TYPE_HS3 ||
        ava1_hs3_decode(buf, len, &m3) != 0 ||
        ava1_noise_read(&ns, m3.noise, m3.noise_len, pl, sizeof pl, &pn) != 0)
        goto out;
    ci_ok = ava1_client_info_decode(pl, pn, &ci) == 0;
    if (ci_ok) clean_name(ci.name, ci.has_name ? ci.name_len : 0, peer_name);
    if (ava1_noise_split(&ns, c2s, s2c) != 0) goto out;
    ava1_control_key(c2s, k->io.recv_key);
    ava1_control_key(s2c, k->io.send_key);
    k->io.keyed = 1;
    if (!ci_ok) {
        /* No pairing nonce (SPEC.md 4.6): refused, sealed like every frame from here on. */
        (void)send_error(&k->io, AVA1_ERR_PROTOCOL, "bad ClientInfo");
        goto out;
    }
    pthread_mutex_lock(&mu);
    known = ava1_peers_contains(&S.peers, ns.rs);
    open = now_ms() < S.pairing_until_ms;
    /* Message 3 has just proved the client holds ns.rs: any session that key still has
     * is replaced, before the limits below. */
    if (known || open) supersede_locked(idx, ns.rs);
    if (!known && open) {
        if (unpaired_locked() >= cfg_or(S.cfg.max_unpaired, MAX_UNPAIRED)) busy = 1;
        else if (!ava1_pl_welcome_allowed(&S.pl, k->ip, now_ms())) busy = 1; /* this address's share */
        else S.sessions[idx].unpaired_hold = 1;
    }
    pthread_mutex_unlock(&mu);
    if (!known && !open) {
        (void)send_error(&k->io, AVA1_ERR_PAIRING_CLOSED, "this device is not paired and pairing is closed");
        goto out;
    }
    if (busy) {
        (void)send_error(&k->io, AVA1_ERR_BUSY, "too many devices are pairing");
        goto out;
    }
    /* A random code for this session, shown on this console's screen only: never derived from
     * the transcript and never sent (SPEC.md 5.5). */
    if (!known && ava1_random_code(&pair_code) != 0) {
        (void)send_error(&k->io, AVA1_ERR_INTERNAL, "no random source");
        goto out;
    }
    memset(&wel, 0, sizeof wel);
    wel.knows_you = known ? 1 : 0;
    memcpy(wel.nonce_s, nonce_s, 16); /* the reveal; the client checks it against si.pair_commit */
    if (known && S.cfg.has_launch && memcmp(ns.rs, S.cfg.launch_key, 32) == 0) {
        wel.has_launch_proof = 1;
        ava1_launch_proof(S.cfg.launch_token, ns.h, wel.launch_proof);
    }
    ava1_w_init(&w, pl, sizeof pl);
    if (ava1_welcome_encode(&wel, &w) != 0 || ava1_conn_send(&k->io, AVA1_TYPE_WELCOME, 0, pl, w.len) != 0)
        goto out;
    sess_fill(idx, k, sid, c2s, s2c, known, ns.rs, peer_name);
    filled = 1;
    pthread_mutex_lock(&mu);
    S.sessions[idx].pair_code = pair_code;
    memcpy(S.sessions[idx].h, ns.h, 64);
    pthread_mutex_unlock(&mu);
    if (!known) {
        /* Only a device that was welcomed is shown, and a stranger reconnecting in a loop
         * must not flood the screen. */
        uint64_t t = now_ms();
        unsigned i, slot = 0;
        int dup = 0;
        pthread_mutex_lock(&mu);
        for (i = 0; i < 8; i++) {
            if (S.shown[i].used && S.shown[i].ip == k->ip && memcmp(S.shown[i].key, ns.rs, 32) == 0 &&
                t - S.shown[i].at_ms < cfg_or(S.cfg.notify_every_ms, NOTIFY_EVERY_MS))
                dup = 1;
            if (!S.shown[i].used) slot = i;
            else if (S.shown[slot].used && S.shown[i].at_ms < S.shown[slot].at_ms) slot = i;
        }
        if (!dup) {
            S.shown[slot].used = 1;
            S.shown[slot].ip = k->ip;
            memcpy(S.shown[slot].key, ns.rs, 32);
            S.shown[slot].at_ms = t;
            notify = 1; /* every session shows its own code; only an identical repeat is hidden */
        }
        pthread_mutex_unlock(&mu);
    }
    if (notify && S.cfg.on_pair_request) S.cfg.on_pair_request(peer_name, pair_code);
    conn_publish(idx, sid, 0, k);
    serve_loop(k, idx, sid, 0, 0, buf);
    /* Withdraw first: from here on a send to this session fails (E_CLOSED), so a job
     * racing the teardown cannot be left attached to a session that is gone. Then park
     * whatever is attached. */
    conn_withdraw(idx, sid, 0, k);
    if (S.cfg.data && S.cfg.data->on_session_end) S.cfg.data->on_session_end(sid);
    sess_remove(idx, sid);
out:
    if (!filled) sess_release(idx);
    ava1_noise_wipe(&ns);
    crypto_wipe(&eph, sizeof eph);
    crypto_wipe(secret, sizeof secret);
    crypto_wipe(pl, sizeof pl);
    crypto_wipe(msg, sizeof msg);
    crypto_wipe(c2s, sizeof c2s);
    crypto_wipe(s2c, sizeof s2c);
}

static void run_lane(conn_t *k, uint8_t *buf, size_t len) {
    ava1_join_t j;
    ava1_join_ack_t ack;
    uint8_t expect[16] = { 0 }, c2s[32], s2c[32], out[64];
    ava1_w_t w;
    int idx, verified = 0, fresh = 0, paired = 0;
    uint32_t gen = 0;
    uint8_t type, flags;
    uint32_t ch;
    memset(c2s, 0, sizeof c2s);
    memset(s2c, 0, sizeof s2c);
    if (ava1_join_decode(buf, len, &j) != 0 || j.lane_id == 0 || j.lane_id > AVA1_MAX_LANES) {
        (void)send_error(&k->io, AVA1_ERR_BAD_JOIN, "bad Join");
        return;
    }
    pthread_mutex_lock(&mu);
    idx = sess_find_locked(j.session_id);
    if (idx >= 0) {
        sess_t *s = &S.sessions[idx];
        ava1_join_tag(s->c2s, j.session_id, j.lane_id, j.client_nonce, expect);
        verified = crypto_verify16(expect, j.tag) == 0;
        fresh = verified && !nonce_seen(s, j.client_nonce);
        if (fresh) {
            nonce_add(s, j.client_nonce);
            paired = s->paired;
        }
        if (fresh && paired) {
            memcpy(c2s, s->c2s, 32);
            memcpy(s2c, s->s2c, 32);
            member_add_locked(idx, k);
        }
    }
    pthread_mutex_unlock(&mu);
    if (!(fresh && paired)) {
        if (fresh) (void)send_error(&k->io, AVA1_ERR_NOT_PAIRED, "pair first");
        else (void)send_error(&k->io, AVA1_ERR_BAD_JOIN, "join refused");
        goto wipe;
    }
    memset(&ack, 0, sizeof ack);
    ack.lane_id = j.lane_id;
    /* A fresh server nonce per join: a replayed Join (even one older than the nonce
     * window) still gets keys never used before (SPEC.md §4.3). */
    if (ava1_platform_random(ack.server_nonce, sizeof ack.server_nonce) != 0) {
        (void)send_error(&k->io, AVA1_ERR_INTERNAL, "no random source");
        goto wipe;
    }
    ava1_join_ack_tag(s2c, j.session_id, j.lane_id, j.client_nonce, ack.server_nonce, ack.tag);
    ava1_w_init(&w, out, sizeof out);
    if (ava1_join_ack_encode(&ack, &w) != 0 || ava1_conn_send(&k->io, AVA1_TYPE_JOIN_ACK, 0, out, w.len) != 0)
        goto wipe;
    ava1_lane_key(c2s, j.lane_id, j.client_nonce, ack.server_nonce, k->io.recv_key);
    ava1_lane_key(s2c, j.lane_id, j.client_nonce, ack.server_nonce, k->io.send_key);
    k->io.keyed = 1;
    /* A Join can be captured and sent again by someone without the session keys. So this
     * connection takes the lane over (ending an older connection of the same id) only
     * once its first sealed frame has opened under the new lane key, within the
     * handshake deadline still in force. Until then the older connection stays. */
    if (ava1_conn_recv(&k->io, &type, &flags, &ch, buf, CTRL_MAX, &len) != 0) goto wipe;
    pthread_mutex_lock(&mu);
    if (sess_is_locked(idx, j.session_id) && !S.sessions[idx].superseded) {
        gen = ++S.sessions[idx].lane_gen[j.lane_id];
        paired = 1;
    } else {
        paired = 0;
    }
    pthread_mutex_unlock(&mu);
    if (paired && handle_frame(k, idx, j.session_id, j.lane_id, type, flags, ch, buf, len) == 0) {
        conn_publish(idx, j.session_id, j.lane_id, k);
        if (S.cfg.data && S.cfg.data->on_lane_change) S.cfg.data->on_lane_change(j.session_id, j.lane_id, 1);
        serve_loop(k, idx, j.session_id, j.lane_id, gen, buf);
        conn_withdraw(idx, j.session_id, j.lane_id, k);
        if (S.cfg.data && S.cfg.data->on_lane_change) S.cfg.data->on_lane_change(j.session_id, j.lane_id, 0);
    }
wipe:
    crypto_wipe(expect, sizeof expect);
    crypto_wipe(c2s, sizeof c2s);
    crypto_wipe(s2c, sizeof s2c);
}

static void *conn_main(void *arg) {
    conn_t *k = arg;
    uint8_t *buf = malloc(CTRL_MAX);
    set_timeouts(k->io.fd, S.cfg.handshake_ms);
    k->io.deadline_ms = now_ms() + S.cfg.handshake_ms;
    if (buf) {
        uint8_t type, flags;
        uint32_t ch;
        size_t len;
        if (ava1_conn_recv(&k->io, &type, &flags, &ch, buf, CTRL_MAX, &len) == 0) {
            if (type == AVA1_TYPE_HS1) run_control(k, buf, len);
            else if (type == AVA1_TYPE_JOIN) run_lane(k, buf, len);
            else (void)send_error(&k->io, AVA1_ERR_PROTOCOL, "expected Hs1 or Join");
        }
        free(buf);
    }
    member_drop(k);
    /* What was posted last (an Error saying why a lane closes) goes out first, briefly. */
    ava1_conn_drain(&k->io, 250);
    /* Wake any worker blocked writing to a peer that is gone, then drop our reference. */
    shutdown(k->io.fd, SHUT_RDWR);
    conn_put(k);
    return NULL;
}

static void refuse_busy(int fd, const char *why) {
    ava1_conn_t c;
    ava1_conn_init(&c, fd);
    set_timeouts(fd, 1000);
    (void)send_error(&c, AVA1_ERR_BUSY, why);
    ava1_conn_destroy(&c);
    close(fd);
}

static void *accept_main(void *arg) {
    (void)arg;
    while (!S.stopping) {
        struct pollfd p;
        struct sockaddr_in from;
        socklen_t from_len = sizeof from;
        uint32_t ip;
        int fd, one = 1, admit, per_ip = 0, pr, i, slot = -1;
        conn_t *k;
        p.fd = S.listen_fd;
        p.events = POLLIN;
        p.revents = 0;
        pr = poll(&p, 1, 200);
        if (pr < 0) {
            ava1_platform_sleep_ms(50);
            continue;
        }
        if (pr == 0) continue;
        memset(&from, 0, sizeof from);
        fd = accept(S.listen_fd, (struct sockaddr *)&from, &from_len);
        if (fd < 0) {
            /* Never leave this loop on an errno: Sony returns undocumented ones (163)
             * and once that killed the helper's accept loop. */
            ava1_platform_sleep_ms(50);
            continue;
        }
        (void)setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
#ifdef SO_NOSIGPIPE
        (void)setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof one);
#endif
        {
            /* Before the first read: 4 MiB each way, whatever the kernel grants. */
            static int logged;
            int rcv = 0, snd = 0;
            (void)ava1_conn_tune_buffers(fd, &rcv, &snd);
            if (!__atomic_exchange_n(&logged, 1, __ATOMIC_RELAXED))
                slog("ava1: lane socket buffers asked %d, kernel gave rcv %d snd %d", AVA1_LANE_SOCKBUF, rcv, snd);
        }
        ip = from.sin_addr.s_addr;
        k = calloc(1, sizeof *k);
        pthread_mutex_lock(&mu);
        admit = k != NULL && S.conns < MAX_CONNS;
        if (admit) {
            /* This address's entry, or a free one (there is always one: at most
             * MAX_CONNS connections are counted). */
            for (i = 0; i < MAX_CONNS; i++) {
                if (S.ips[i].n > 0 && S.ips[i].ip == ip) {
                    slot = i;
                    break;
                }
                if (S.ips[i].n == 0 && slot < 0) slot = i;
            }
            if (slot < 0 || (S.ips[slot].n > 0 &&
                             (uint32_t)S.ips[slot].n >= cfg_or(S.cfg.max_conns_per_ip, MAX_CONNS_PER_IP))) {
                admit = 0;
                per_ip = 1;
            } else {
                S.ips[slot].ip = ip;
                S.ips[slot].n++;
                S.conns++;
            }
        }
        pthread_mutex_unlock(&mu);
        if (!admit) {
            free(k);
            refuse_busy(fd, per_ip ? "too many connections from this address" : "too many connections");
            continue;
        }
        ava1_conn_init(&k->io, fd);
        k->refs = 1;
        k->ip = ip;
        k->ip_held = 1;
        k->member = -1;
        if (spawn_detached(conn_main, k) != 0) conn_put(k);
    }
    return NULL;
}

int ava1_server_start(const ava1_server_cfg_t *cfg) {
    struct sockaddr_in a;
    socklen_t alen = sizeof a;
    int one = 1, i, busy = 1;
    for (i = 0; i < 500 && busy; i++) {
        pthread_mutex_lock(&mu);
        busy = S.conns > 0 || S.accept_started;
        pthread_mutex_unlock(&mu);
        if (busy) ava1_platform_sleep_ms(10);
    }
    if (busy) return -EBUSY;
    pthread_mutex_lock(&mu);
    memset(S.sessions, 0, sizeof S.sessions);
    S.cfg = *cfg;
    S.stopping = 0;
    memset(S.ips, 0, sizeof S.ips);
    memset(S.shown, 0, sizeof S.shown);
    ava1_pl_init(&S.pl, cfg->max_pair_fails_per_ip, cfg->max_pair_fails_total, cfg->max_welcomes_per_ip, 0);
    S.peers_ok = ava1_peers_load(&S.peers, cfg->peers_path) == 0;
    if (!S.peers_ok) {
        /* Unknown is not empty: run for nobody rather than open pairing to anyone or
         * overwrite a file that may list every paired device. */
        memset(&S.peers, 0, sizeof S.peers);
        slog("ava1: cannot read %s; pairing stays closed and the file is left alone", cfg->peers_path);
    }
    /* Opens by itself only while nothing is paired (design review, flaw 2). */
    S.pairing_until_ms = (cfg->pairing_window_s && S.peers_ok && S.peers.n == 0)
                             ? now_ms() + (uint64_t)cfg->pairing_window_s * 1000u
                             : 0;
    pthread_mutex_unlock(&mu);
    S.listen_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (S.listen_fd < 0) return -errno;
    (void)setsockopt(S.listen_fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons(cfg->port);
    a.sin_addr.s_addr = htonl(cfg->bind_loopback ? INADDR_LOOPBACK : INADDR_ANY);
    if (bind(S.listen_fd, (struct sockaddr *)&a, sizeof a) != 0 || listen(S.listen_fd, 64) != 0 ||
        getsockname(S.listen_fd, (struct sockaddr *)&a, &alen) != 0) {
        int e = errno;
        close(S.listen_fd);
        S.listen_fd = -1;
        return -e;
    }
    S.port = ntohs(a.sin_port);
    if (pthread_create(&S.accept_thread, NULL, accept_main, NULL) != 0) {
        close(S.listen_fd);
        S.listen_fd = -1;
        return -EAGAIN;
    }
    pthread_mutex_lock(&mu);
    S.accept_started = 1;
    pthread_mutex_unlock(&mu);
    return 0;
}

uint16_t ava1_server_port(void) { return S.port; }

void ava1_server_open_pairing(uint32_t seconds) {
    pthread_mutex_lock(&mu);
    ava1_pl_reset(&S.pl);
    S.pairing_until_ms = now_ms() + (uint64_t)seconds * 1000u;
    pthread_mutex_unlock(&mu);
}

/* Wrong guesses at the code since the window was last opened (tests, logs). */
uint32_t ava1_server_pair_guesses(void) {
    uint32_t n;
    pthread_mutex_lock(&mu);
    n = S.pl.total;
    pthread_mutex_unlock(&mu);
    return n;
}

int ava1_server_pairing_open(void) {
    int open;
    pthread_mutex_lock(&mu);
    open = now_ms() < S.pairing_until_ms;
    pthread_mutex_unlock(&mu);
    return open;
}

int ava1_server_conns(void) {
    int n;
    pthread_mutex_lock(&mu);
    n = S.conns;
    pthread_mutex_unlock(&mu);
    return n;
}

void ava1_server_stop(void) {
    int started;
    pthread_mutex_lock(&mu);
    started = S.accept_started;
    pthread_mutex_unlock(&mu);
    if (!started) return;
    S.stopping = 1;
    pthread_join(S.accept_thread, NULL);
    close(S.listen_fd);
    S.listen_fd = -1;
    pthread_mutex_lock(&mu);
    S.accept_started = 0;
    pthread_mutex_unlock(&mu);
}
