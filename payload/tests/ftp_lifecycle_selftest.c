/* Host-side FTP lifecycle integration test.
 *
 * Repeatedly stops the server with a live control connection and starts it
 * again on the same port. This catches stale-listener ABA, descriptor
 * double-close/reuse, untracked sessions, and mutable-auth regressions. */
#include <arpa/inet.h>
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <strings.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

#define PS5UPLOAD_FTP_HOST_SELFTEST 1
#include "../src/ftp_server.c"

static int failures = 0;

#define CHECK(expr)                                                         \
    do {                                                                    \
        if (!(expr)) {                                                      \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);       \
            failures++;                                                     \
        }                                                                   \
    } while (0)

static int reserve_port(void) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    struct sockaddr_in a;
    memset(&a, 0, sizeof(a));
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    a.sin_port = 0;
    if (bind(fd, (struct sockaddr *)&a, sizeof(a)) != 0) {
        close(fd);
        return -1;
    }
    socklen_t len = sizeof(a);
    if (getsockname(fd, (struct sockaddr *)&a, &len) != 0) {
        close(fd);
        return -1;
    }
    int port = ntohs(a.sin_port);
    close(fd);
    return port;
}

static int start_until_ready(int port, const char *root) {
    char resp[512];
    size_t written = 0;
    for (int i = 0; i < 500; i++) {
        memset(resp, 0, sizeof(resp));
        ftp_server_start(port, root, 0, "tester", "secret",
                         resp, sizeof(resp), &written);
        if (strstr(resp, "\"ok\":true")) return 0;
        if (!strstr(resp, "\"error\":\"stopping\"")) {
            fprintf(stderr, "unexpected start response: %s\n", resp);
            return -1;
        }
        usleep(2000);
    }
    return -1;
}

static int connect_control(int port) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    struct timeval tv = {.tv_sec = 2, .tv_usec = 0};
    (void)setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    (void)setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv));
    struct sockaddr_in a;
    memset(&a, 0, sizeof(a));
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    a.sin_port = htons((uint16_t)port);
    for (int i = 0; i < 200; i++) {
        if (connect(fd, (struct sockaddr *)&a, sizeof(a)) == 0) return fd;
        if (errno != ECONNREFUSED && errno != EINTR) break;
        usleep(2000);
    }
    close(fd);
    return -1;
}

static int recv_until(int fd, const char *needle) {
    char all[4096];
    size_t used = 0;
    all[0] = '\0';
    while (used + 1 < sizeof(all)) {
        ssize_t n = recv(fd, all + used, sizeof(all) - used - 1, 0);
        if (n <= 0) return -1;
        used += (size_t)n;
        all[used] = '\0';
        if (strstr(all, needle)) return 0;
    }
    return -1;
}

static int send_text(int fd, const char *s) {
    size_t len = strlen(s);
    size_t off = 0;
    while (off < len) {
        ssize_t n = send(fd, s + off, len - off, 0);
        if (n < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (n == 0) return -1;
        off += (size_t)n;
    }
    return 0;
}

int main(void) {
    (void)signal(SIGPIPE, SIG_IGN);
    char root_template[] = "/tmp/ps5upload-ftp-XXXXXX";
    char *root = mkdtemp(root_template);
    CHECK(root != NULL);
    int port = reserve_port();
    CHECK(port > 0);
    if (!root || port <= 0) return 1;

    ftp_server_init();
    CHECK(start_until_ready(port, root) == 0);

    int fd = connect_control(port);
    CHECK(fd >= 0);
    if (fd >= 0) {
        CHECK(recv_until(fd, "220 PS5 FTP Server ready") == 0);
        CHECK(send_text(fd, "PWD\r\n") == 0);
        CHECK(recv_until(fd, "530 Not logged in") == 0);
        CHECK(send_text(fd, "USER tester\r\n") == 0);
        CHECK(recv_until(fd, "331 User name okay") == 0);
        CHECK(send_text(fd, "PASS secret\r\n") == 0);
        CHECK(recv_until(fd, "230 Login successful") == 0);
        CHECK(send_text(fd, "PWD\r\n") == 0);
        CHECK(recv_until(fd, "257 \"/\"") == 0);
    }

    char resp[512];
    size_t written = 0;
    ftp_server_stop(resp, sizeof(resp), &written);
    CHECK(strstr(resp, "\"ok\":true") != NULL);
    if (fd >= 0) {
        char b;
        CHECK(recv(fd, &b, 1, 0) <= 0);
        close(fd);
    }

    /* Exercise rapid stop/start with a live registered session. Start may
     * briefly answer `stopping`; it must never bind over an old session. */
    for (int cycle = 0; cycle < 20; cycle++) {
        CHECK(start_until_ready(port, root) == 0);
        fd = connect_control(port);
        CHECK(fd >= 0);
        if (fd >= 0) CHECK(recv_until(fd, "220 PS5 FTP Server ready") == 0);
        ftp_server_stop(resp, sizeof(resp), &written);
        CHECK(strstr(resp, "\"ok\":true") != NULL);
        if (fd >= 0) close(fd);
    }

    for (int i = 0; i < 500 && atomic_load(&g_ftp.connections) != 0; i++) {
        usleep(2000);
    }
    ftp_server_status(resp, sizeof(resp), &written);
    CHECK(strstr(resp, "\"running\":false") != NULL);
    CHECK(strstr(resp, "\"connections\":0") != NULL);

    (void)rmdir(root);
    printf("ftp_lifecycle_selftest: %s\n",
           failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
