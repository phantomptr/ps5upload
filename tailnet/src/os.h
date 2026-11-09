/* Platform layer: POSIX (macOS, Linux, Android, iOS, the PS5) and Windows (Winsock,
 * MSVC or MinGW). Sockets are `int` everywhere; on Windows the SOCKET handle is cast
 * (Winsock handles are small kernel handles; INVALID_SOCKET casts to -1). The rest of
 * the core uses only what is declared here plus standard C. Not part of the API. */
#ifndef TN_OS_H
#define TN_OS_H

#include <stddef.h>
#include <stdint.h>

#ifdef _WIN32
#ifndef NOMINMAX
#define NOMINMAX
#endif
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef _WIN32_WINNT
#define _WIN32_WINNT 0x0602 /* WSAPoll (Vista), GetSystemTimePreciseAsFileTime (8) */
#endif
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#include <basetsd.h>
#include <limits.h>
typedef SSIZE_T ssize_t;
#ifndef SSIZE_MAX
#define SSIZE_MAX INTPTR_MAX
#endif
#ifndef MSG_NOSIGNAL
#define MSG_NOSIGNAL 0
#endif
#define strncasecmp _strnicmp
#else
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <pthread.h>
#include <strings.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <unistd.h>
#ifndef MSG_NOSIGNAL
#define MSG_NOSIGNAL 0
#endif
#endif

/* ---- atomics (MSVC has no <stdatomic.h> without an experimental flag) ---- */
typedef volatile long tn_atomic;
#if defined(_MSC_VER) && !defined(__clang__)
#define tn_atomic_load(p) InterlockedCompareExchange((p), 0, 0)
#define tn_atomic_store(p, v) ((void)InterlockedExchange((p), (long)(v)))
#define tn_atomic_xchg(p, v) InterlockedExchange((p), (long)(v))
#define tn_atomic_inc(p) ((void)InterlockedIncrement(p))
#else
#define tn_atomic_load(p) __atomic_load_n((p), __ATOMIC_SEQ_CST)
#define tn_atomic_store(p, v) __atomic_store_n((p), (long)(v), __ATOMIC_SEQ_CST)
#define tn_atomic_xchg(p, v) __atomic_exchange_n((p), (long)(v), __ATOMIC_SEQ_CST)
#define tn_atomic_inc(p) ((void)__atomic_add_fetch((p), 1, __ATOMIC_SEQ_CST))
#endif

/* ---- threads and a recursive mutex ---- */
#ifdef _WIN32
typedef CRITICAL_SECTION tn_mutex; /* recursive by nature */
typedef HANDLE tn_thread;
#else
typedef pthread_mutex_t tn_mutex;
typedef pthread_t tn_thread;
#endif
void tn_mutex_init(tn_mutex *m);
void tn_mutex_lock(tn_mutex *m);
void tn_mutex_unlock(tn_mutex *m);
void tn_mutex_destroy(tn_mutex *m);
/* 0 or -1. stack: bytes, 0 = default. */
int tn_thread_start(tn_thread *t, void *(*fn)(void *), void *arg, size_t stack);
void tn_thread_join(tn_thread t);

/* ---- sockets ---- */
int tn_os_init(void);               /* WSAStartup once; 0 or -1 */
int tn_sock_open(int type);         /* AF_INET SOCK_STREAM / SOCK_DGRAM, or -1 */
void tn_sock_close(int fd);
void tn_sock_nonblock(int fd);
void tn_sock_nosigpipe(int fd);
/* connect: 0 connected, 1 in progress, -1 failed */
int tn_sock_connect(int fd, uint32_t ip, uint16_t port);
int tn_sock_error(int fd);          /* SO_ERROR, 0 = fine */
int tn_sock_errno(void);            /* last socket error as a TN_ERR_* class */
enum { TN_ERR_OTHER = 0, TN_ERR_AGAIN, TN_ERR_INTR, TN_ERR_REFUSED, TN_ERR_UNREACH, TN_ERR_RESET };
int tn_sock_raw_errno(void);        /* the platform's number, for logs */
ssize_t tn_sock_send(int fd, const void *p, size_t n);
ssize_t tn_sock_recv(int fd, void *p, size_t n);
ssize_t tn_sock_sendto(int fd, const void *p, size_t n, uint32_t ip, uint16_t port);
ssize_t tn_sock_recvfrom(int fd, void *p, size_t n, uint32_t *ip, uint16_t *port);
int tn_sock_bind(int fd, uint32_t ip, uint16_t port);   /* 0 or -1 */
int tn_sock_listen(int fd);
int tn_sock_accept(int fd);                             /* nonblocking, -1 */
uint16_t tn_sock_local_port(int fd);
void tn_sock_setint(int fd, int level, int opt, int v);
void tn_sock_shutdown_wr(int fd);
int tn_poll(struct pollfd *p, unsigned n, int timeout_ms);
/* Sleep this thread for ms milliseconds (no wake-up: callers keep it short). */
void tn_sleep_ms(int ms);
/* How long a loop waits before polling again when poll() itself failed: 0 for an interrupt
 * (retry at once), otherwise a short pause so a vanished network (rest mode: every socket call
 * fails, errno 163) never turns a loop into a busy spin. `budget_ms` < 0 means no deadline. */
int tn_poll_fail_wait_ms(int interrupted, long long budget_ms);

/* A wake channel: write side signals, read side is pollable (a pipe; on Windows two
 * connected loopback UDP sockets). */
int tn_wake_pair(int *r, int *w);
void tn_wake_signal(int w);
void tn_wake_drain(int r);
void tn_wake_close(int r, int w);

/* ---- clocks ---- */
int64_t tn_mono_us(void);
int64_t tn_wall_ns(void);

/* ---- local IPv4 addresses (up, not loopback) ---- */
int tn_os_local_ipv4(uint32_t *out, int cap);

/* ---- files ---- */
int tn_file_read(const char *path, void *buf, size_t cap);           /* bytes or -1 */
int tn_file_write_atomic(const char *path, const void *p, size_t n); /* 0 or -1, mode 0600 */
int tn_file_remove(const char *path);
void tn_mkdir(const char *path);

#endif
