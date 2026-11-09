/* Platform layer implementation (see os.h). */
#include "os.h"

#include <stdio.h>
#include <string.h>
#include <time.h>

#ifdef _WIN32
#include <iphlpapi.h>
#include <io.h>
#include <direct.h>
#include <fcntl.h>
#include <sys/stat.h>
#else
#include <ifaddrs.h>
#include <net/if.h>
#include <sys/stat.h>
#endif

/* ---------------- threads ---------------- */

#ifdef _WIN32
void tn_mutex_init(tn_mutex *m) { InitializeCriticalSection(m); }
void tn_mutex_lock(tn_mutex *m) { EnterCriticalSection(m); }
void tn_mutex_unlock(tn_mutex *m) { LeaveCriticalSection(m); }
void tn_mutex_destroy(tn_mutex *m) { DeleteCriticalSection(m); }

typedef struct {
	void *(*fn)(void *);
	void *arg;
} thread_start;

static DWORD WINAPI thread_main(LPVOID p)
{
	thread_start s = *(thread_start *)p;
	HeapFree(GetProcessHeap(), 0, p);
	s.fn(s.arg);
	return 0;
}

int tn_thread_start(tn_thread *t, void *(*fn)(void *), void *arg, size_t stack)
{
	thread_start *s = HeapAlloc(GetProcessHeap(), 0, sizeof *s);
	if (!s)
		return -1;
	s->fn = fn;
	s->arg = arg;
	*t = CreateThread(NULL, stack, thread_main, s, 0, NULL);
	if (!*t) {
		HeapFree(GetProcessHeap(), 0, s);
		return -1;
	}
	return 0;
}

void tn_thread_join(tn_thread t)
{
	WaitForSingleObject(t, INFINITE);
	CloseHandle(t);
}
#else
void tn_mutex_init(tn_mutex *m)
{
	pthread_mutexattr_t a;
	pthread_mutexattr_init(&a);
	pthread_mutexattr_settype(&a, PTHREAD_MUTEX_RECURSIVE);
	pthread_mutex_init(m, &a);
	pthread_mutexattr_destroy(&a);
}
void tn_mutex_lock(tn_mutex *m) { pthread_mutex_lock(m); }
void tn_mutex_unlock(tn_mutex *m) { pthread_mutex_unlock(m); }
void tn_mutex_destroy(tn_mutex *m) { pthread_mutex_destroy(m); }

int tn_thread_start(tn_thread *t, void *(*fn)(void *), void *arg, size_t stack)
{
	pthread_attr_t at;
	pthread_attr_init(&at);
	if (stack)
		pthread_attr_setstacksize(&at, stack);
	int r = pthread_create(t, &at, fn, arg);
	pthread_attr_destroy(&at);
	return r == 0 ? 0 : -1;
}

void tn_thread_join(tn_thread t) { pthread_join(t, NULL); }
#endif

/* ---------------- sockets ---------------- */

#ifdef _WIN32
#define SOCK(fd) ((SOCKET)(intptr_t)(fd))

int tn_os_init(void)
{
	static tn_atomic done;
	if (tn_atomic_xchg(&done, 1))
		return 0;
	WSADATA w;
	return WSAStartup(MAKEWORD(2, 2), &w) == 0 ? 0 : -1;
}

int tn_sock_open(int type)
{
	SOCKET s = socket(AF_INET, type, 0);
	if (s == INVALID_SOCKET)
		return -1;
	if (type == SOCK_DGRAM) {
		/* An ICMP port-unreachable must not make the next recvfrom fail (WSAECONNRESET). */
		BOOL off = FALSE;
		DWORD got = 0;
		WSAIoctl(s, _WSAIOW(IOC_VENDOR, 12) /* SIO_UDP_CONNRESET */, &off, sizeof off, NULL, 0, &got, NULL, NULL);
	}
	return (int)s;
}

void tn_sock_close(int fd)
{
	if (fd >= 0)
		closesocket(SOCK(fd));
}

void tn_sock_nonblock(int fd)
{
	u_long one = 1;
	ioctlsocket(SOCK(fd), FIONBIO, &one);
}

void tn_sock_nosigpipe(int fd) { (void)fd; }

int tn_sock_raw_errno(void) { return WSAGetLastError(); }

int tn_sock_errno(void)
{
	switch (WSAGetLastError()) {
	case WSAEWOULDBLOCK:
	case WSAEINPROGRESS:
	case WSAEALREADY:
		return TN_ERR_AGAIN;
	case WSAEINTR:
		return TN_ERR_INTR;
	case WSAECONNREFUSED:
		return TN_ERR_REFUSED;
	case WSAECONNRESET:
		return TN_ERR_RESET;
	case WSAEHOSTUNREACH:
	case WSAENETUNREACH:
	case WSAEACCES:
		return TN_ERR_UNREACH;
	default:
		return TN_ERR_OTHER;
	}
}

int tn_poll(struct pollfd *p, unsigned n, int timeout_ms) { return WSAPoll(p, (ULONG)n, timeout_ms); }
void tn_sleep_ms(int ms) { Sleep(ms > 0 ? (DWORD)ms : 0); }
#else
#define SOCK(fd) (fd)

int tn_os_init(void) { return 0; }

int tn_sock_open(int type)
{
	int fd = socket(AF_INET, type, 0);
	if (fd >= 0) {
		int fl = fcntl(fd, F_GETFD);
		if (fl >= 0)
			fcntl(fd, F_SETFD, fl | FD_CLOEXEC);
	}
	return fd;
}

void tn_sock_close(int fd)
{
	if (fd >= 0)
		close(fd);
}

void tn_sock_nonblock(int fd) { fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK); }

void tn_sock_nosigpipe(int fd)
{
#ifdef SO_NOSIGPIPE
	int one = 1;
	setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof one);
#else
	(void)fd;
#endif
}

int tn_sock_raw_errno(void) { return errno; }

int tn_sock_errno(void)
{
	switch (errno) {
	case EAGAIN:
#if EWOULDBLOCK != EAGAIN
	case EWOULDBLOCK:
#endif
	case EINPROGRESS:
		return TN_ERR_AGAIN;
	case EINTR:
		return TN_ERR_INTR;
	case ECONNREFUSED:
		return TN_ERR_REFUSED;
	case ECONNRESET:
		return TN_ERR_RESET;
	case EHOSTUNREACH:
	case ENETUNREACH:
	case EPERM:
	case EACCES:
		return TN_ERR_UNREACH;
	default:
		return TN_ERR_OTHER; /* includes the PS5's errno 163 in rest mode */
	}
}

int tn_poll(struct pollfd *p, unsigned n, int timeout_ms) { return poll(p, (nfds_t)n, timeout_ms); }
void tn_sleep_ms(int ms)
{
	if (ms <= 0)
		return;
	struct timespec ts = {.tv_sec = ms / 1000, .tv_nsec = (long)(ms % 1000) * 1000000L};
	while (nanosleep(&ts, &ts) != 0 && errno == EINTR) {
	}
}
#endif

static struct sockaddr_in sin4(uint32_t ip, uint16_t port)
{
	struct sockaddr_in sa;
	memset(&sa, 0, sizeof sa);
	sa.sin_family = AF_INET;
	sa.sin_port = htons(port);
	sa.sin_addr.s_addr = htonl(ip);
	return sa;
}

int tn_sock_connect(int fd, uint32_t ip, uint16_t port)
{
	struct sockaddr_in sa = sin4(ip, port);
	if (connect(SOCK(fd), (struct sockaddr *)&sa, sizeof sa) == 0)
		return 0;
	return tn_sock_errno() == TN_ERR_AGAIN ? 1 : -1;
}

int tn_sock_error(int fd)
{
	int e = 0;
	socklen_t l = sizeof e;
	if (getsockopt(SOCK(fd), SOL_SOCKET, SO_ERROR, (char *)&e, &l) != 0)
		return -1;
	return e;
}

ssize_t tn_sock_send(int fd, const void *p, size_t n)
{
#ifdef _WIN32
	return send(SOCK(fd), (const char *)p, n > INT_MAX ? INT_MAX : (int)n, 0);
#else
	return send(fd, p, n, MSG_NOSIGNAL);
#endif
}

ssize_t tn_sock_recv(int fd, void *p, size_t n)
{
#ifdef _WIN32
	return recv(SOCK(fd), (char *)p, n > INT_MAX ? INT_MAX : (int)n, 0);
#else
	return recv(fd, p, n, 0);
#endif
}

ssize_t tn_sock_sendto(int fd, const void *p, size_t n, uint32_t ip, uint16_t port)
{
	struct sockaddr_in sa = sin4(ip, port);
#ifdef _WIN32
	return sendto(SOCK(fd), (const char *)p, (int)n, 0, (struct sockaddr *)&sa, sizeof sa);
#else
	return sendto(fd, p, n, 0, (struct sockaddr *)&sa, sizeof sa);
#endif
}

ssize_t tn_sock_recvfrom(int fd, void *p, size_t n, uint32_t *ip, uint16_t *port)
{
	struct sockaddr_in sa;
	socklen_t sl = sizeof sa;
	memset(&sa, 0, sizeof sa);
#ifdef _WIN32
	ssize_t r = recvfrom(SOCK(fd), (char *)p, (int)n, 0, (struct sockaddr *)&sa, &sl);
#else
	ssize_t r = recvfrom(fd, p, n, 0, (struct sockaddr *)&sa, &sl);
#endif
	if (r >= 0) {
		*ip = ntohl(sa.sin_addr.s_addr);
		*port = ntohs(sa.sin_port);
	}
	return r;
}

int tn_sock_bind(int fd, uint32_t ip, uint16_t port)
{
	struct sockaddr_in sa = sin4(ip, port);
	return bind(SOCK(fd), (struct sockaddr *)&sa, sizeof sa) == 0 ? 0 : -1;
}

int tn_sock_listen(int fd) { return listen(SOCK(fd), 16) == 0 ? 0 : -1; }

int tn_sock_accept(int fd)
{
#ifdef _WIN32
	SOCKET s = accept(SOCK(fd), NULL, NULL);
	return s == INVALID_SOCKET ? -1 : (int)s;
#else
	return accept(fd, NULL, NULL);
#endif
}

uint16_t tn_sock_local_port(int fd)
{
	struct sockaddr_in sa;
	socklen_t sl = sizeof sa;
	memset(&sa, 0, sizeof sa);
	if (getsockname(SOCK(fd), (struct sockaddr *)&sa, &sl) != 0)
		return 0;
	return ntohs(sa.sin_port);
}

void tn_sock_setint(int fd, int level, int opt, int v)
{
	setsockopt(SOCK(fd), level, opt, (const char *)&v, sizeof v);
}

void tn_sock_shutdown_wr(int fd)
{
#ifdef _WIN32
	shutdown(SOCK(fd), SD_SEND);
#else
	shutdown(fd, SHUT_WR);
#endif
}

/* ---------------- wake channel ---------------- */

#ifdef _WIN32
int tn_wake_pair(int *r, int *w)
{
	int a = tn_sock_open(SOCK_DGRAM), b = tn_sock_open(SOCK_DGRAM);
	if (a < 0 || b < 0 || tn_sock_bind(a, 0x7f000001, 0) < 0 || tn_sock_bind(b, 0x7f000001, 0) < 0 ||
	    tn_sock_connect(a, 0x7f000001, tn_sock_local_port(b)) != 0 ||
	    tn_sock_connect(b, 0x7f000001, tn_sock_local_port(a)) != 0) {
		tn_sock_close(a);
		tn_sock_close(b);
		return -1;
	}
	tn_sock_nonblock(a);
	tn_sock_nonblock(b);
	*r = a;
	*w = b;
	return 0;
}

void tn_wake_signal(int w)
{
	char c = 1;
	send(SOCK(w), &c, 1, 0);
}

void tn_wake_drain(int r)
{
	char b[64];
	while (recv(SOCK(r), b, sizeof b, 0) > 0)
		;
}

void tn_wake_close(int r, int w)
{
	tn_sock_close(r);
	tn_sock_close(w);
}
#else
int tn_wake_pair(int *r, int *w)
{
	int p[2];
	if (pipe(p) != 0)
		return -1;
	for (int i = 0; i < 2; i++) {
		fcntl(p[i], F_SETFL, fcntl(p[i], F_GETFL) | O_NONBLOCK);
		fcntl(p[i], F_SETFD, fcntl(p[i], F_GETFD) | FD_CLOEXEC);
	}
	*r = p[0];
	*w = p[1];
	return 0;
}

void tn_wake_signal(int w)
{
	char c = 1;
	if (write(w, &c, 1) < 0) { /* full: a wake is already pending */
	}
}

void tn_wake_drain(int r)
{
	char b[64];
	while (read(r, b, sizeof b) > 0)
		;
}

void tn_wake_close(int r, int w)
{
	if (r >= 0)
		close(r);
	if (w >= 0)
		close(w);
}
#endif

/* ---------------- clocks ---------------- */

#ifdef _WIN32
int64_t tn_mono_us(void)
{
	LARGE_INTEGER f, c;
	QueryPerformanceFrequency(&f);
	QueryPerformanceCounter(&c);
	return (int64_t)(c.QuadPart / f.QuadPart) * 1000000 + (int64_t)(c.QuadPart % f.QuadPart) * 1000000 / f.QuadPart;
}

int64_t tn_wall_ns(void)
{
	FILETIME ft;
	GetSystemTimePreciseAsFileTime(&ft);
	uint64_t t = ((uint64_t)ft.dwHighDateTime << 32) | ft.dwLowDateTime; /* 100 ns since 1601 */
	return (int64_t)(t - 116444736000000000ULL) * 100;
}
#else
int64_t tn_mono_us(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (int64_t)ts.tv_sec * 1000000 + ts.tv_nsec / 1000;
}

int64_t tn_wall_ns(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_REALTIME, &ts);
	return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}
#endif

/* ---------------- local addresses ---------------- */

static int skip_ip(uint32_t ip)
{
	return (ip >> 24) == 127 || (ip & 0xffc00000u) == 0x64400000u /* 100.64/10: a tailnet's */ ||
	       (ip >> 16) == 0xa9fe /* link-local */ || ip == 0;
}

#ifdef _WIN32
int tn_os_local_ipv4(uint32_t *out, int cap)
{
	ULONG len = 15000;
	int n = 0;
	IP_ADAPTER_ADDRESSES *a = HeapAlloc(GetProcessHeap(), 0, len);
	if (!a)
		return 0;
	if (GetAdaptersAddresses(AF_INET, GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER,
				 NULL, a, &len) == NO_ERROR) {
		for (IP_ADAPTER_ADDRESSES *i = a; i && n < cap; i = i->Next) {
			if (i->OperStatus != IfOperStatusUp)
				continue;
			for (IP_ADAPTER_UNICAST_ADDRESS *u = i->FirstUnicastAddress; u && n < cap; u = u->Next) {
				struct sockaddr_in *s = (struct sockaddr_in *)u->Address.lpSockaddr;
				uint32_t ip = ntohl(s->sin_addr.s_addr);
				if (s->sin_family == AF_INET && !skip_ip(ip))
					out[n++] = ip;
			}
		}
	}
	HeapFree(GetProcessHeap(), 0, a);
	return n;
}
#else
int tn_os_local_ipv4(uint32_t *out, int cap)
{
	struct ifaddrs *ifa = NULL;
	int n = 0;
	if (getifaddrs(&ifa) != 0)
		return 0;
	for (struct ifaddrs *i = ifa; i && n < cap; i = i->ifa_next) {
		if (!i->ifa_addr || i->ifa_addr->sa_family != AF_INET || !(i->ifa_flags & IFF_UP) ||
		    (i->ifa_flags & IFF_LOOPBACK))
			continue;
		uint32_t ip = ntohl(((struct sockaddr_in *)i->ifa_addr)->sin_addr.s_addr);
		if (!skip_ip(ip))
			out[n++] = ip;
	}
	freeifaddrs(ifa);
	return n;
}
#endif

/* ---------------- files ---------------- */

int tn_file_read(const char *path, void *buf, size_t cap)
{
	FILE *f = fopen(path, "rb");
	if (!f)
		return -1;
	size_t n = fread(buf, 1, cap, f);
	int bad = ferror(f);
	fclose(f);
	return bad ? -1 : (int)n;
}

int tn_file_write_atomic(const char *path, const void *p, size_t n)
{
	char tmp[600];
	snprintf(tmp, sizeof tmp, "%s.tmp", path);
#ifdef _WIN32
	int fd = _open(tmp, _O_WRONLY | _O_CREAT | _O_TRUNC | _O_BINARY, _S_IREAD | _S_IWRITE);
	if (fd < 0)
		return -1;
	int ok = _write(fd, p, (unsigned)n) == (int)n && _commit(fd) == 0;
	_close(fd);
	if (!ok || !MoveFileExA(tmp, path, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)) {
		_unlink(tmp);
		return -1;
	}
	return 0;
#else
	int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
	if (fd < 0)
		return -1;
	const uint8_t *b = p;
	size_t off = 0;
	while (off < n) {
		ssize_t w = write(fd, b + off, n - off);
		if (w < 0 && errno == EINTR)
			continue;
		if (w <= 0)
			break;
		off += (size_t)w;
	}
	int ok = off == n && fsync(fd) == 0;
	close(fd);
	if (!ok || rename(tmp, path) != 0) {
		unlink(tmp);
		return -1;
	}
	return 0;
#endif
}

int tn_file_remove(const char *path)
{
#ifdef _WIN32
	return _unlink(path) == 0 ? 0 : -1;
#else
	return unlink(path) == 0 ? 0 : -1;
#endif
}

void tn_mkdir(const char *path)
{
#ifdef _WIN32
	_mkdir(path);
#else
	mkdir(path, 0700);
#endif
}

int tn_poll_fail_wait_ms(int interrupted, long long budget_ms)
{
	enum { POLL_FAIL_WAIT_MS = 100 };
	if (interrupted)
		return 0;
	if (budget_ms >= 0 && budget_ms < POLL_FAIL_WAIT_MS)
		return budget_ms < 1 ? 1 : (int)budget_ms;
	return POLL_FAIL_WAIT_MS;
}
