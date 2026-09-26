/* Socket self-test for the installer's loopback HTTP server. Writes a temp
 * pkg, starts the server on 127.0.0.1:0, and drives it with raw HTTP. */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>

#include "../installer/loopback.h"

static int failures = 0;
#define CHECK(expr)                                                     \
    do {                                                                \
        if (!(expr)) {                                                  \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);     \
            failures++;                                                 \
        }                                                               \
    } while (0)

/* Send `req` to 127.0.0.1:port, read the whole response into resp (cap).
 * Returns total bytes read, or -1. */
static int http_roundtrip(uint16_t port, const char *req, char *resp, size_t cap) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    struct sockaddr_in a;
    memset(&a, 0, sizeof(a));
    a.sin_family = AF_INET;
    a.sin_port = htons(port);
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (connect(fd, (struct sockaddr *)&a, sizeof(a)) != 0) { close(fd); return -1; }
    if (write(fd, req, strlen(req)) < 0) { close(fd); return -1; }
    size_t got = 0;
    for (;;) {
        ssize_t n = read(fd, resp + got, cap - 1 - got);
        if (n <= 0) break;
        got += (size_t)n;
        if (got >= cap - 1) break;
    }
    resp[got] = '\0';
    close(fd);
    return (int)got;
}

/* Byte count of the body after the "\r\n\r\n" header terminator. */
static size_t body_len(const char *resp, int total) {
    const char *b = strstr(resp, "\r\n\r\n");
    if (!b) return 0;
    b += 4;
    return (size_t)(resp + total - b);
}

int main(void) {
    /* temp pkg: 1000 bytes, byte i == i & 0xff */
    char pkgpath[] = "/tmp/ps5upload-lb-XXXXXX";
    int pfd = mkstemp(pkgpath);
    CHECK(pfd >= 0);
    /* rename to .pkg so the daemon-side name policy is realistic */
    char pkgname[128];
    snprintf(pkgname, sizeof(pkgname), "%s.pkg", pkgpath);
    rename(pkgpath, pkgname);
    pfd = open(pkgname, O_WRONLY);
    unsigned char buf[1000];
    for (int i = 0; i < 1000; i++) buf[i] = (unsigned char)(i & 0xff);
    write(pfd, buf, sizeof(buf));
    close(pfd);

    const char *base = strrchr(pkgname, '/') + 1;
    const char *attempt = "1758800000-1";

    inst_loopback_t *lb = NULL;
    uint16_t port = 0;
    CHECK(inst_loopback_start(&lb, pkgname, attempt, base, &port) == 0);
    CHECK(port != 0);
    CHECK(inst_loopback_total(lb) == 1000);

    char req[512], resp[8192];

    /* full GET on the right path (query appended) */
    snprintf(req, sizeof(req),
        "GET /%s/%s?product=x&serverIpAddr=y HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        attempt, base);
    int n = http_roundtrip(port, req, resp, sizeof(resp));
    CHECK(n > 0);
    CHECK(strncmp(resp, "HTTP/1.1 200 OK\r\n", 17) == 0);
    CHECK(strstr(resp, "Last-Modified: ") != NULL);
    CHECK(strstr(resp, "Accept-Ranges: bytes") != NULL);
    CHECK(body_len(resp, n) == 1000);

    /* range GET bytes=0-3 -> 206, 4 bytes */
    snprintf(req, sizeof(req),
        "GET /%s/%s HTTP/1.1\r\nHost: h\r\nRange: bytes=0-3\r\nConnection: close\r\n\r\n",
        attempt, base);
    n = http_roundtrip(port, req, resp, sizeof(resp));
    CHECK(strncmp(resp, "HTTP/1.1 206 Partial Content\r\n", 30) == 0);
    CHECK(strstr(resp, "Content-Range: bytes 0-3/1000") != NULL);
    CHECK(body_len(resp, n) == 4);

    /* HEAD -> headers only, no body */
    snprintf(req, sizeof(req),
        "HEAD /%s/%s HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n", attempt, base);
    n = http_roundtrip(port, req, resp, sizeof(resp));
    CHECK(strncmp(resp, "HTTP/1.1 200 OK\r\n", 17) == 0);
    CHECK(body_len(resp, n) == 0);

    /* unsatisfiable range -> 416 with Content-Range: "bytes star/1000" */
    snprintf(req, sizeof(req),
        "GET /%s/%s HTTP/1.1\r\nHost: h\r\nRange: bytes=5000-6000\r\nConnection: close\r\n\r\n",
        attempt, base);
    n = http_roundtrip(port, req, resp, sizeof(resp));
    CHECK(strncmp(resp, "HTTP/1.1 416", 12) == 0);
    CHECK(strstr(resp, "Content-Range: bytes */1000") != NULL);

    /* wrong path -> 404 */
    snprintf(req, sizeof(req),
        "GET /%s/other.pkg HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n", attempt);
    n = http_roundtrip(port, req, resp, sizeof(resp));
    CHECK(strncmp(resp, "HTTP/1.1 404", 12) == 0);

    /* .crc with no sidecar -> 404 (never pkg bytes) */
    snprintf(req, sizeof(req),
        "GET /%s/ABCD00000-0000.crc HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n", attempt);
    n = http_roundtrip(port, req, resp, sizeof(resp));
    CHECK(strncmp(resp, "HTTP/1.1 404", 12) == 0);
    CHECK(body_len(resp, n) == 0);

    inst_loopback_stop(lb);

    /* now create a .crc sidecar next to the pkg and restart: it must be served */
    char crcpath[160];
    snprintf(crcpath, sizeof(crcpath), "/tmp/ABCD00000-0000.crc");
    /* place the sidecar in the SAME directory as the pkg (/tmp) */
    int cfd = open(crcpath, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    const char *crcbytes = "CRCDATA!";
    write(cfd, crcbytes, 8);
    close(cfd);

    CHECK(inst_loopback_start(&lb, pkgname, attempt, base, &port) == 0);
    snprintf(req, sizeof(req),
        "GET /%s/ABCD00000-0000.crc HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n", attempt);
    n = http_roundtrip(port, req, resp, sizeof(resp));
    CHECK(strncmp(resp, "HTTP/1.1 200 OK\r\n", 17) == 0);
    CHECK(body_len(resp, n) == 8);
    CHECK(strstr(resp, "CRCDATA!") != NULL);

    /* bytes_served accounts for the pkg body union, not the sidecar */
    snprintf(req, sizeof(req),
        "GET /%s/%s HTTP/1.1\r\nHost: h\r\nRange: bytes=0-99\r\nConnection: close\r\n\r\n",
        attempt, base);
    http_roundtrip(port, req, resp, sizeof(resp));
    snprintf(req, sizeof(req),
        "GET /%s/%s HTTP/1.1\r\nHost: h\r\nRange: bytes=50-149\r\nConnection: close\r\n\r\n",
        attempt, base);
    http_roundtrip(port, req, resp, sizeof(resp));
    /* union of [0,99] and [50,149] = 150 bytes, not 200 */
    CHECK(inst_loopback_bytes_served(lb) == 150);

    inst_loopback_stop(lb);
    unlink(pkgname);
    unlink(crcpath);

    printf("installer_loopback_selftest: %s\n", failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
