/* tnecho: echo servers and clients for the test bed (Linux, built static).
 *
 *   tnecho serve-udp PORT [BIND]       echo every datagram back to its sender
 *   tnecho serve-tcp PORT [BIND]       echo every TCP connection (one thread each)
 *   tnecho udp HOST PORT COUNT         COUNT datagrams, one at a time: RTT each + summary
 *   tnecho tcp HOST PORT COUNT         connect, COUNT 64-byte round trips: connect time, RTT
 *   tnecho bulk HOST PORT MBYTES       stream MBYTES through the echo and verify: throughput
 *   tnecho connect HOST PORT           connect only: prints "connected", "refused" or "timeout"
 *   tnecho flood HOST PORT COUNT PPS   COUNT 1200-byte datagrams at PPS (a Remote Play stream's
 *                                      shape): echoed, lost, RTT, Mbit/s each way
 * Exit status 0 = everything answered and matched. */
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

static double now_ms(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return ts.tv_sec * 1000.0 + ts.tv_nsec / 1e6;
}

static struct sockaddr_in addr(const char *host, int port)
{
	struct sockaddr_in sa = {.sin_family = AF_INET, .sin_port = htons((unsigned short)port)};
	if (!host || inet_pton(AF_INET, host, &sa.sin_addr) != 1)
		sa.sin_addr.s_addr = htonl(INADDR_ANY);
	return sa;
}

static void *tcp_echo_client(void *arg)
{
	int fd = (int)(long)arg;
	char buf[65536];
	for (;;) {
		ssize_t n = recv(fd, buf, sizeof buf, 0);
		if (n <= 0)
			break;
		for (ssize_t off = 0; off < n;) {
			ssize_t m = send(fd, buf + off, (size_t)(n - off), MSG_NOSIGNAL);
			if (m <= 0)
				goto out;
			off += m;
		}
	}
out:
	close(fd);
	return NULL;
}

static int serve(int tcp, int port, const char *bind_ip)
{
	int fd = socket(AF_INET, tcp ? SOCK_STREAM : SOCK_DGRAM, 0), one = 1;
	struct sockaddr_in sa = addr(bind_ip, port);
	setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
	if (bind(fd, (struct sockaddr *)&sa, sizeof sa) != 0 || (tcp && listen(fd, 16) != 0)) {
		perror("tnecho: bind");
		return 1;
	}
	for (;;) {
		if (tcp) {
			int c = accept(fd, NULL, NULL);
			if (c < 0)
				continue;
			pthread_t t;
			pthread_create(&t, NULL, tcp_echo_client, (void *)(long)c);
			pthread_detach(t);
		} else {
			char buf[2048];
			struct sockaddr_in from;
			socklen_t fl = sizeof from;
			ssize_t n = recvfrom(fd, buf, sizeof buf, 0, (struct sockaddr *)&from, &fl);
			if (n >= 0)
				sendto(fd, buf, (size_t)n, 0, (struct sockaddr *)&from, fl);
		}
	}
}

static int wait_fd(int fd, short ev, int ms)
{
	struct pollfd p = {.fd = fd, .events = ev};
	return poll(&p, 1, ms) > 0 ? p.revents : 0;
}

static int dial(const char *host, int port, int timeout_ms, double *ms)
{
	struct sockaddr_in sa = addr(host, port);
	int fd = socket(AF_INET, SOCK_STREAM, 0), one = 1;
	setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
	fcntl(fd, F_SETFL, O_NONBLOCK);
	double t0 = now_ms();
	if (connect(fd, (struct sockaddr *)&sa, sizeof sa) != 0 && errno != EINPROGRESS) {
		close(fd);
		return -2;
	}
	if (!wait_fd(fd, POLLOUT, timeout_ms)) {
		close(fd);
		return -1;
	}
	int err = 0;
	socklen_t el = sizeof err;
	getsockopt(fd, SOL_SOCKET, SO_ERROR, &err, &el);
	if (err) {
		close(fd);
		return -2;
	}
	*ms = now_ms() - t0;
	fcntl(fd, F_SETFL, 0);
	return fd;
}

int main(int argc, char **argv)
{
	if (argc >= 3 && !strcmp(argv[1], "serve-udp"))
		return serve(0, atoi(argv[2]), argc > 3 ? argv[3] : NULL);
	if (argc >= 3 && !strcmp(argv[1], "serve-tcp"))
		return serve(1, atoi(argv[2]), argc > 3 ? argv[3] : NULL);
	if (argc < 4) {
		fprintf(stderr, "usage: see the comment at the top of tnecho.c\n");
		return 2;
	}
	const char *host = argv[2];
	int port = atoi(argv[3]);
	double ms;
	if (!strcmp(argv[1], "connect")) {
		int fd = dial(host, port, 5000, &ms);
		printf("%s\n", fd >= 0 ? "connected" : fd == -2 ? "refused" : "timeout");
		if (fd >= 0)
			close(fd);
		return fd >= 0 ? 0 : 1;
	}
	if (argc < 5)
		return 2;
	long count = atol(argv[4]);
	if (!strcmp(argv[1], "flood") && argc >= 6) {
		long pps = atol(argv[5]);
		int fd = socket(AF_INET, SOCK_DGRAM, 0);
		struct sockaddr_in sa = addr(host, port);
		connect(fd, (struct sockaddr *)&sa, sizeof sa);
		int sz = 4 << 20;
		setsockopt(fd, SOL_SOCKET, SO_RCVBUF, &sz, sizeof sz);
		fcntl(fd, F_SETFL, O_NONBLOCK);
		static unsigned char out[1200], in[2048];
		static double sent_at[200000];
		long got = 0, sent = 0;
		double rtt_sum = 0, t0 = now_ms(), gap = 1000.0 / (double)pps;
		if (count > 200000)
			count = 200000;
		while (sent < count || now_ms() - t0 < count * gap + 2000) {
			double now = now_ms();
			while (sent < count && now >= t0 + sent * gap) {
				memcpy(out, &sent, sizeof sent);
				sent_at[sent] = now;
				send(fd, out, sizeof out, 0);
				sent++;
			}
			ssize_t m;
			while ((m = recv(fd, in, sizeof in, 0)) == (ssize_t)sizeof out) {
				long seq;
				memcpy(&seq, in, sizeof seq);
				if (seq >= 0 && seq < sent) {
					got++;
					rtt_sum += now_ms() - sent_at[seq];
				}
			}
			if (sent >= count && got >= count)
				break;
			wait_fd(fd, POLLIN, 1);
		}
		double secs = count * gap / 1000.0;
		printf("flood %s:%d: %ld/%ld echoed (%.2f%% lost), avg rtt %.2f ms, %.1f Mbit/s each way\n", host, port, got,
		       count, 100.0 * (double)(count - got) / (double)count, got ? rtt_sum / got : 0, got * 1200.0 * 8 / secs / 1e6);
		return got * 100 >= count * 99 ? 0 : 1; /* tolerate up to 1%% loss, like a stream */
	}
	if (!strcmp(argv[1], "udp")) {
		int fd = socket(AF_INET, SOCK_DGRAM, 0), ok = 0;
		struct sockaddr_in sa = addr(host, port);
		connect(fd, (struct sockaddr *)&sa, sizeof sa);
		double min = 1e9, max = 0, sum = 0;
		for (long i = 0; i < count; i++) {
			char out[64], in[128];
			int n = snprintf(out, sizeof out, "tnecho udp %ld", i);
			double t0 = now_ms();
			send(fd, out, (size_t)n, 0);
			while (wait_fd(fd, POLLIN, 2000)) {
				ssize_t m = recv(fd, in, sizeof in, 0);
				if (m == n && !memcmp(in, out, (size_t)n)) {
					double r = now_ms() - t0;
					ok++;
					sum += r;
					min = r < min ? r : min;
					max = r > max ? r : max;
					break;
				}
			}
			usleep(100000);
		}
		printf("udp %s:%d: %d/%ld answered, rtt min/avg/max %.2f/%.2f/%.2f ms\n", host, port, ok, count,
		       ok ? min : 0, ok ? sum / ok : 0, ok ? max : 0);
		return ok == count ? 0 : 1;
	}
	int fd = dial(host, port, 5000, &ms);
	if (fd < 0) {
		printf("tcp %s:%d: %s\n", host, port, fd == -2 ? "refused" : "timeout");
		return 1;
	}
	struct timeval tv = {5, 0};
	setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv);
	if (!strcmp(argv[1], "tcp")) {
		double min = 1e9, max = 0, sum = 0;
		long ok = 0;
		for (long i = 0; i < count; i++) {
			char out[64], in[64];
			memset(out, 'a' + (int)(i % 26), sizeof out);
			double t0 = now_ms();
			if (send(fd, out, sizeof out, MSG_NOSIGNAL) != sizeof out)
				break;
			size_t got = 0;
			while (got < sizeof in) {
				ssize_t m = recv(fd, in + got, sizeof in - got, 0);
				if (m <= 0)
					break;
				got += (size_t)m;
			}
			if (got != sizeof in || memcmp(in, out, sizeof in))
				break;
			double r = now_ms() - t0;
			ok++;
			sum += r;
			min = r < min ? r : min;
			max = r > max ? r : max;
		}
		printf("tcp %s:%d: connect %.2f ms, %ld/%ld round trips, rtt min/avg/max %.2f/%.2f/%.2f ms\n", host, port,
		       ms, ok, count, ok ? min : 0, ok ? sum / ok : 0, ok ? max : 0);
		close(fd);
		return ok == count ? 0 : 1;
	}
	if (!strcmp(argv[1], "bulk")) {
		size_t total = (size_t)count << 20, sent = 0, got = 0;
		static unsigned char sb[65536], rb[65536];
		for (size_t i = 0; i < sizeof sb; i++)
			sb[i] = (unsigned char)(i * 7 + 3);
		fcntl(fd, F_SETFL, O_NONBLOCK);
		double t0 = now_ms();
		int bad = 0;
		while (got < total && !bad) {
			struct pollfd p = {.fd = fd, .events = POLLIN | (sent < total ? POLLOUT : 0)};
			if (poll(&p, 1, 10000) <= 0)
				break;
			if ((p.revents & POLLOUT) && sent < total) {
				size_t off = sent % sizeof sb, n = sizeof sb - off;
				if (n > total - sent)
					n = total - sent;
				ssize_t m = send(fd, sb + off, n, MSG_NOSIGNAL);
				if (m > 0)
					sent += (size_t)m;
			}
			if (p.revents & (POLLIN | POLLHUP)) {
				ssize_t m = recv(fd, rb, sizeof rb, 0);
				if (m <= 0)
					break;
				for (ssize_t i = 0; i < m; i++)
					if (rb[i] != sb[(got + (size_t)i) % sizeof sb])
						bad = 1;
				got += (size_t)m;
			}
		}
		double s = (now_ms() - t0) / 1000.0;
		printf("bulk %s:%d: %zu of %zu bytes echoed%s in %.2f s: %.2f MB/s each way\n", host, port, got, total,
		       bad ? " (CORRUPT)" : "", s, got / s / 1e6);
		close(fd);
		return got == total && !bad ? 0 : 1;
	}
	return 2;
}
