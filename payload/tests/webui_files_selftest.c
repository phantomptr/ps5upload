/* Host self-test for webui_files.c: atomic text save with its precondition, an upload received into a
 * temporary file and renamed (cancel, short body, existing target), Range parsing, Content-Disposition,
 * and the .tar stream checked by the system tar extracting it. */
#include "webui_files.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

static int g_fail;
#define CHECK(c)                                                                 \
    do {                                                                         \
        if (!(c)) {                                                              \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #c);         \
            g_fail = 1;                                                          \
        }                                                                        \
    } while (0)

static char g_root[256];

static void put_file(const char *path, const char *data, size_t n) {
    FILE *f = fopen(path, "wb");
    if (!f) {
        perror(path);
        exit(1);
    }
    if (n) fwrite(data, 1, n, f);
    fclose(f);
}

static size_t read_file(const char *path, char *out, size_t cap) {
    FILE *f = fopen(path, "rb");
    size_t n;
    if (!f) return (size_t)-1;
    n = fread(out, 1, cap, f);
    fclose(f);
    return n;
}

/* Temporary files left in `dir`. */
static int temps_in(const char *dir) {
    DIR *d = opendir(dir);
    struct dirent *e;
    int n = 0;
    if (!d) return -1;
    while ((e = readdir(d)) != NULL)
        if (strncmp(e->d_name, ".ps5u-", 6) == 0) n++;
    closedir(d);
    return n;
}

static void test_text_save(void) {
    char p[512], back[64], big_path[512];
    struct stat st;
    wf_text_pre_t pre;
    uint64_t mt = 0, sz = 0;
    uint8_t *big;
    snprintf(p, sizeof p, "%s/notes.txt", g_root);
    put_file(p, "old text\n", 9);
    chmod(p, 0640);
    stat(p, &st);

    memset(&pre, 0, sizeof pre);
    pre.check = 1;
    pre.mtime = (uint64_t)st.st_mtime;
    pre.size = 9;
    CHECK(wf_text_save(p, (const uint8_t *)"new\n", 4, &pre, &mt, &sz, NULL) == WF_OK);
    CHECK(sz == 4);
    CHECK(read_file(p, back, sizeof back) == 4 && memcmp(back, "new\n", 4) == 0);
    stat(p, &st);
    CHECK((st.st_mode & 0777) == 0640); /* the old file's permissions are kept */

    /* changed since it was opened (size differs): refused, the file untouched */
    pre.mtime = (uint64_t)st.st_mtime;
    pre.size = 99;
    CHECK(wf_text_save(p, (const uint8_t *)"lost", 4, &pre, NULL, NULL, NULL) == WF_E_CHANGED);
    CHECK(read_file(p, back, sizeof back) == 4 && memcmp(back, "new\n", 4) == 0);
    pre.size = 4;
    pre.mtime = (uint64_t)st.st_mtime - 5;
    CHECK(wf_text_save(p, (const uint8_t *)"lost", 4, &pre, NULL, NULL, NULL) == WF_E_CHANGED);

    /* binary and too large: refused before anything is written */
    pre.mtime = (uint64_t)st.st_mtime;
    CHECK(wf_text_save(p, (const uint8_t *)"a\0b", 3, &pre, NULL, NULL, NULL) == WF_E_BINARY);
    big = calloc(1, WF_TEXT_MAX + 1);
    memset(big, 'x', WF_TEXT_MAX + 1);
    CHECK(wf_text_save(p, big, WF_TEXT_MAX + 1, &pre, NULL, NULL, NULL) == WF_E_TOO_BIG);
    CHECK(wf_text_save(p, big, WF_TEXT_MAX, &pre, NULL, &sz, NULL) == WF_OK && sz == WF_TEXT_MAX);
    free(big);

    /* a folder or a link is never replaced */
    snprintf(big_path, sizeof big_path, "%s/adir", g_root);
    mkdir(big_path, 0755);
    pre.check = 0;
    CHECK(wf_text_save(big_path, (const uint8_t *)"x", 1, &pre, NULL, NULL, NULL) == WF_E_NOT_FILE);
    snprintf(big_path, sizeof big_path, "%s/alink", g_root);
    CHECK(symlink(p, big_path) == 0);
    CHECK(wf_text_save(big_path, (const uint8_t *)"x", 1, &pre, NULL, NULL, NULL) == WF_E_NOT_FILE);
    snprintf(big_path, sizeof big_path, "%s/missing.txt", g_root);
    CHECK(wf_text_save(big_path, (const uint8_t *)"x", 1, &pre, NULL, NULL, NULL) == WF_E_GONE);

    /* a same-size change within the same second: caught by the nanoseconds */
    {
        uint32_t ns = 0;
        memset(&pre, 0, sizeof pre);
        CHECK(wf_text_save(p, (const uint8_t *)"aaaa", 4, &pre, &mt, &sz, &ns) == WF_OK);
        usleep(2000);
        put_file(p, "bbbb", 4);
        stat(p, &st);
        if ((uint64_t)st.st_mtime == mt && wf_mtime_ns(&st) != ns) {
            pre.check = 1;
            pre.mtime = mt;
            pre.size = 4;
            CHECK(wf_text_save(p, (const uint8_t *)"cccc", 4, &pre, NULL, NULL, NULL) == WF_OK); /* seconds alone miss it */
            put_file(p, "bbbb", 4);
            stat(p, &st);
            pre.mtime = (uint64_t)st.st_mtime;
            pre.have_ns = 1;
            pre.mtime_ns = wf_mtime_ns(&st) + 1;
            CHECK(wf_text_save(p, (const uint8_t *)"dddd", 4, &pre, NULL, NULL, NULL) == WF_E_CHANGED);
            pre.mtime_ns = wf_mtime_ns(&st);
            CHECK(wf_text_save(p, (const uint8_t *)"dddd", 4, &pre, NULL, NULL, NULL) == WF_OK);
        }
    }

    /* create: an empty new file, never over anything */
    memset(&pre, 0, sizeof pre);
    pre.create = 1;
    snprintf(big_path, sizeof big_path, "%s/new.txt", g_root);
    CHECK(wf_text_save(big_path, NULL, 0, &pre, NULL, &sz, NULL) == WF_OK && sz == 0);
    CHECK(stat(big_path, &st) == 0 && st.st_size == 0);
    CHECK(wf_text_save(big_path, NULL, 0, &pre, NULL, NULL, NULL) == WF_E_EXISTS);
    CHECK(wf_text_save(p, NULL, 0, &pre, NULL, NULL, NULL) == WF_E_EXISTS);
    CHECK(temps_in(g_root) == 0);
}

typedef struct {
    int fd;
    const uint8_t *data;
    size_t n;
    unsigned pause_ms; /* before closing */
} sender_t;

static void *send_main(void *arg) {
    sender_t *s = (sender_t *)arg;
    size_t at = 0;
    while (at < s->n) {
        size_t k = s->n - at > 7000 ? 7000 : s->n - at;
        ssize_t w = send(s->fd, s->data + at, k, 0);
        if (w <= 0) break;
        at += (size_t)w;
    }
    if (s->pause_ms) usleep(s->pause_ms * 1000);
    close(s->fd);
    return NULL;
}

/* Receive `body` (sent `send_n` of it, then closed) as `len` bytes into `target`. */
static int receive(const char *target, const uint8_t *body, size_t send_n, uint64_t len, size_t pre, int overwrite,
                   unsigned idle, unsigned pause_ms) {
    int sv[2], rc;
    pthread_t t;
    sender_t s;
    uint8_t *buf = malloc(64 * 1024);
    socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
    s.fd = sv[1];
    s.data = body + pre;
    s.n = send_n - pre;
    s.pause_ms = pause_ms;
    pthread_create(&t, NULL, send_main, &s);
    rc = wf_receive_file(sv[0], body, pre, len, target, overwrite, 0777, idle, NULL, buf, 64 * 1024);
    close(sv[0]);
    pthread_join(t, NULL);
    free(buf);
    return rc;
}

static void test_receive(void) {
    char p[512], *back;
    size_t n = 3 * 64 * 1024 + 123, i;
    uint8_t *body = malloc(n);
    struct stat st;
    for (i = 0; i < n; i++) body[i] = (uint8_t)(i * 7 + 3);
    back = malloc(n + 16);
    snprintf(p, sizeof p, "%s/up.bin", g_root);

    CHECK(receive(p, body, n, n, 100, 0, 2000, 0) == WF_OK);
    CHECK(read_file(p, back, n + 16) == n && memcmp(back, body, n) == 0);
    CHECK(stat(p, &st) == 0 && (st.st_mode & 0777) == 0777);

    /* there now: refused without overwrite, replaced with it */
    body[0] ^= 0xff;
    CHECK(receive(p, body, n, n, 0, 0, 2000, 0) == WF_E_EXISTS);
    CHECK(read_file(p, back, n + 16) == n && back[0] != (char)body[0]);
    CHECK(receive(p, body, n, n, 0, 1, 2000, 0) == WF_OK);
    CHECK(read_file(p, back, n + 16) == n && memcmp(back, body, n) == 0);

    /* the client goes away halfway: nothing under the final name changes, no temp left */
    body[0] ^= 0xff;
    CHECK(receive(p, body, n / 2, n, 0, 1, 2000, 0) == WF_E_SHORT);
    CHECK(read_file(p, back, n + 16) == n && back[0] != (char)body[0]);
    /* a silent client: the idle timeout ends it */
    CHECK(receive(p, body, n / 2, n, 0, 1, 400, 1500) == WF_E_SHORT);
    /* a new name the client abandons: never created */
    snprintf(p, sizeof p, "%s/never.bin", g_root);
    CHECK(receive(p, body, 10, n, 0, 0, 2000, 0) == WF_E_SHORT);
    CHECK(lstat(p, &st) != 0);
    /* a folder is never replaced */
    snprintf(p, sizeof p, "%s/adir", g_root);
    CHECK(receive(p, body, 10, 10, 0, 1, 2000, 0) == WF_E_NOT_FILE);
    /* an empty upload */
    snprintf(p, sizeof p, "%s/empty.bin", g_root);
    CHECK(receive(p, body, 0, 0, 0, 0, 2000, 0) == WF_OK);
    CHECK(stat(p, &st) == 0 && st.st_size == 0);
    CHECK(temps_in(g_root) == 0);
    free(body);
    free(back);
}

static void test_range(void) {
    uint64_t a = 0, b = 0;
    CHECK(wf_parse_range("", 100, &a, &b) == 0);
    CHECK(wf_parse_range("bytes=0-", 100, &a, &b) == 1 && a == 0 && b == 99);
    CHECK(wf_parse_range("bytes=10-19", 100, &a, &b) == 1 && a == 10 && b == 19);
    CHECK(wf_parse_range("bytes=90-500", 100, &a, &b) == 1 && a == 90 && b == 99);
    CHECK(wf_parse_range("bytes=-30", 100, &a, &b) == 1 && a == 70 && b == 99);
    CHECK(wf_parse_range("bytes=-300", 100, &a, &b) == 1 && a == 0 && b == 99);
    CHECK(wf_parse_range("bytes=100-", 100, &a, &b) == -1);
    CHECK(wf_parse_range("bytes=-0", 100, &a, &b) == -1);
    CHECK(wf_parse_range("bytes=0-1,5-6", 100, &a, &b) == 0);
    CHECK(wf_parse_range("bytes=5-2", 100, &a, &b) == 0);
    CHECK(wf_parse_range("items=0-1", 100, &a, &b) == 0);
    CHECK(wf_parse_range("bytes=x-1", 100, &a, &b) == 0);
    CHECK(wf_parse_range("bytes=99999999999999999999999-", 100, &a, &b) == 0);
}

static void test_disposition(void) {
    char out[512];
    size_t n = wf_disposition(out, sizeof out, "a \"b\".txt");
    CHECK(n > 0 && strstr(out, "filename=\"a _b_.txt\"") && strstr(out, "filename*=UTF-8''a%20%22b%22.txt\r\n"));
    n = wf_disposition(out, sizeof out, "\xe6\x97\xa5.txt");
    CHECK(n > 0 && strstr(out, "filename=\"___.txt\"") && strstr(out, "UTF-8''%E6%97%A5.txt"));
    CHECK(wf_disposition(out, 40, "a-very-long-name-that-does-not-fit.txt") == 0);
}

static int sum_ok(const uint8_t *h) {
    unsigned s = 0, want;
    int i;
    for (i = 0; i < 512; i++) s += (i >= 148 && i < 156) ? ' ' : h[i];
    want = (unsigned)strtoul((const char *)h + 148, NULL, 8);
    return s == want;
}

static void test_tar_header(void) {
    static uint8_t h[WF_TAR_HEADER_MAX];
    char name[400];
    size_t n;
    n = wf_tar_header(h, sizeof h, "dir/file.txt", 0, 1234, 0644, 1700000000);
    CHECK(n == 512 && sum_ok(h) && strcmp((char *)h, "dir/file.txt") == 0 && h[156] == '0');
    CHECK(strtoull((const char *)h + 124, NULL, 8) == 1234);
    CHECK(memcmp(h + 257, "ustar\0" "00", 8) == 0);
    n = wf_tar_header(h, sizeof h, "dir", 1, 99, 0755, 0);
    CHECK(n == 512 && strcmp((char *)h, "dir/") == 0 && h[156] == '5' && strtoull((const char *)h + 124, NULL, 8) == 0);
    /* 150 chars: split into prefix + name, no pax */
    memset(name, 'a', 120);
    name[120] = '/';
    memset(name + 121, 'b', 29);
    name[150] = '\0';
    n = wf_tar_header(h, sizeof h, name, 0, 1, 0644, 0);
    CHECK(n == 512 && sum_ok(h) && strlen((char *)h + 345) == 120 && strncmp((char *)h, "bbb", 3) == 0);
    /* 300 chars, no usable '/': a pax header with the path first */
    memset(name, 'c', 300);
    name[300] = '\0';
    n = wf_tar_header(h, sizeof h, name, 0, 1, 0644, 0);
    CHECK(n == 512 * 3 && h[156] == 'x' && sum_ok(h) && sum_ok(h + 1024));
    CHECK(strstr((char *)h + 512, " path=ccc") != NULL);
    /* 9 GiB: a pax size record and a base-256 size field */
    n = wf_tar_header(h, sizeof h, "big.bin", 0, 9ull << 30, 0644, 0);
    CHECK(n == 512 * 3 && strstr((char *)h + 512, " size=9663676416\n") != NULL && h[1024 + 124] == 0x80);
    CHECK(wf_tar_header(h, 100, "x", 0, 1, 0644, 0) == 0);
}

typedef struct {
    int fd;
    const char *out;
} sink_t;

static void *sink_main(void *arg) {
    sink_t *s = (sink_t *)arg;
    FILE *f = fopen(s->out, "wb");
    char buf[65536];
    ssize_t n;
    while ((n = recv(s->fd, buf, sizeof buf, 0)) > 0) fwrite(buf, 1, (size_t)n, f);
    fclose(f);
    close(s->fd);
    return NULL;
}

static int skip_secret(const char *p, void *ctx) { return strstr(p, (const char *)ctx) != NULL; }

static void test_tar_stream(void) {
    char tree[512], cmd[2048], p[1200], longdir[700], tarf[512], out[512];
    uint8_t *buf = malloc(WF_IO_BUF), *big = malloc(1536 * 1024);
    int sv[2], i, rc;
    uint64_t sent = 0;
    pthread_t t;
    sink_t s;
    snprintf(tree, sizeof tree, "%s/tree", g_root);
    mkdir(tree, 0755);
    snprintf(p, sizeof p, "%s/a.txt", tree);
    put_file(p, "hello\n", 6);
    snprintf(p, sizeof p, "%s/empty", tree);
    put_file(p, "", 0);
    snprintf(p, sizeof p, "%s/\xe6\x97\xa5\xe6\x9c\xac.txt", tree);
    put_file(p, "unicode", 7);
    for (i = 0; i < 1536 * 1024; i++) big[i] = (uint8_t)(i * 31 + 1);
    snprintf(p, sizeof p, "%s/big.bin", tree);
    put_file(p, (const char *)big, 1536 * 1024);
    snprintf(p, sizeof p, "%s/secret", tree);
    mkdir(p, 0755);
    snprintf(p, sizeof p, "%s/secret/key", tree);
    put_file(p, "k", 1);
    /* a path over 255 bytes inside the archive */
    {
        char d[93], e[108], f[90];
        memset(d, 'd', 92);
        d[92] = '\0';
        memset(e, 'e', 107);
        e[107] = '\0';
        memset(f, 'f', 85);
        memcpy(f + 85, ".txt", 5);
        snprintf(p, sizeof p, "%s/%s", tree, d);
        mkdir(p, 0755);
        snprintf(longdir, sizeof longdir, "%s/%s/%s", tree, d, e);
        mkdir(longdir, 0755);
        snprintf(p, sizeof p, "%s/%s", longdir, f);
        put_file(p, "deep", 4);
    }
    snprintf(p, sizeof p, "%s/link", tree);
    symlink("a.txt", p);

    snprintf(tarf, sizeof tarf, "%s/out.tar", g_root);
    socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
    s.fd = sv[1];
    s.out = tarf;
    pthread_create(&t, NULL, sink_main, &s);
    rc = wf_send_tar(sv[0], tree, skip_secret, (void *)"/secret", 5000, buf, WF_IO_BUF, &sent);
    close(sv[0]);
    pthread_join(t, NULL);
    CHECK(rc == 0 && sent > 1536 * 1024 && sent % 512 == 0);

    /* As an HTTP body in chunks: the same archive once the framing is taken off, and the closing
     * chunk at the end (the browser's proof that it has all of it). */
    {
        char chf[700];
        struct stat st_plain, st_ch;
        uint64_t sent2 = 0, body = 0;
        FILE *in, *ref;
        int ok = 1, closed = 0;
        snprintf(chf, sizeof chf, "%s/out.chunked", g_root);
        socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
        s.fd = sv[1];
        s.out = chf;
        pthread_create(&t, NULL, sink_main, &s);
        rc = wf_send_tar_http(sv[0], tree, skip_secret, (void *)"/secret", 5000, buf, WF_IO_BUF, &sent2, 1);
        close(sv[0]);
        pthread_join(t, NULL);
        CHECK(rc == 0 && sent2 == sent);
        CHECK(stat(tarf, &st_plain) == 0 && stat(chf, &st_ch) == 0 && st_ch.st_size > st_plain.st_size);
        in = fopen(chf, "rb");
        ref = fopen(tarf, "rb");
        CHECK(in && ref);
        while (in && ref && ok && !closed) {
            char line[32];
            unsigned long n;
            if (!fgets(line, sizeof line, in)) {
                ok = 0;
                break;
            }
            n = strtoul(line, NULL, 16);
            if (n == 0) {
                closed = strcmp(line, "0\r\n") == 0 && fgetc(in) == '\r' && fgetc(in) == '\n' && fgetc(in) == EOF;
                break;
            }
            for (unsigned long i = 0; i < n && ok; i++) ok = fgetc(in) == fgetc(ref);
            body += n;
            ok = ok && fgetc(in) == '\r' && fgetc(in) == '\n';
        }
        CHECK(ok && closed && body == sent && ref && fgetc(ref) == EOF);
        if (in) fclose(in);
        if (ref) fclose(ref);
    }

    snprintf(out, sizeof out, "%s/x", g_root);
    mkdir(out, 0755);
    snprintf(cmd, sizeof cmd, "tar -xf '%s' -C '%s' 2>&1", tarf, out);
    CHECK(system(cmd) == 0);
    /* everything but the skipped folder and the link, byte for byte */
    snprintf(cmd, sizeof cmd, "rm -rf '%s/secret' '%s/link' && diff -r '%s' '%s/tree' >/dev/null", tree, tree, tree, out);
    CHECK(system(cmd) == 0);
    snprintf(p, sizeof p, "%s/tree/secret", out);
    CHECK(access(p, F_OK) != 0);
    free(buf);
    free(big);
}

static void test_rate(void) {
    wf_rate_t r;
    memset(&r, 0, sizeof r);
    /* defaults: 1 KiB/s averaged over 60 s */
    CHECK(wf_rate_ok(&r, 1000, 10) == 1);
    CHECK(wf_rate_ok(&r, 30000, 100) == 1);        /* inside the window: never judged */
    CHECK(wf_rate_ok(&r, 61000, 100) == 0);        /* 210 bytes in 60 s: dropped */
    memset(&r, 0, sizeof r);
    CHECK(wf_rate_ok(&r, 1000, 70 * 1024) == 1);
    CHECK(wf_rate_ok(&r, 61000, 0) == 1);          /* 70 KiB in 60 s: kept, a new window starts */
    CHECK(wf_rate_ok(&r, 100000, 0) == 1);
    CHECK(wf_rate_ok(&r, 121000, 59 * 1024) == 0); /* the next window is judged on its own */
    memset(&r, 0, sizeof r);
    r.window_ms = 100;
    r.min_bps = 1000;
    CHECK(wf_rate_ok(&r, 5, 50) == 1);
    CHECK(wf_rate_ok(&r, 110, 60) == 1); /* 110 bytes over 105 ms: above 1000 B/s */
    memset(&r, 0, sizeof r);
    r.window_ms = 100;
    r.min_bps = 1000;
    CHECK(wf_rate_ok(&r, 5, 10) == 1);
    CHECK(wf_rate_ok(&r, 205, 10) == 0); /* 20 bytes over 200 ms */
}

static void test_sweep(void) {
    char d[512], p[600];
    struct timespec old[2];
    time_t now = time(NULL);
    snprintf(d, sizeof d, "%s/sweep", g_root);
    mkdir(d, 0755);
    snprintf(p, sizeof p, "%s/.ps5u-0011223344556677.part", d);
    put_file(p, "old", 3);
    old[0].tv_sec = old[1].tv_sec = now - 25 * 3600;
    old[0].tv_nsec = old[1].tv_nsec = 0;
    utimensat(AT_FDCWD, p, old, AT_SYMLINK_NOFOLLOW);
    snprintf(p, sizeof p, "%s/.ps5u-8899aabbccddeeff.part", d);
    put_file(p, "new", 3); /* an upload running now: kept */
    snprintf(p, sizeof p, "%s/.ps5u-link.part", d);
    symlink("/etc/hosts", p);
    utimensat(AT_FDCWD, p, old, AT_SYMLINK_NOFOLLOW); /* old, but a link: never touched */
    snprintf(p, sizeof p, "%s/keep.part", d);
    put_file(p, "x", 1);
    utimensat(AT_FDCWD, p, old, 0);
    CHECK(wf_sweep_stale(d, now, WF_STALE_PART_S) == 1);
    snprintf(p, sizeof p, "%s/.ps5u-0011223344556677.part", d);
    CHECK(access(p, F_OK) != 0);
    snprintf(p, sizeof p, "%s/.ps5u-8899aabbccddeeff.part", d);
    CHECK(access(p, F_OK) == 0);
    snprintf(p, sizeof p, "%s/.ps5u-link.part", d);
    { struct stat st; CHECK(lstat(p, &st) == 0 && S_ISLNK(st.st_mode)); }
    CHECK(wf_sweep_stale("/nonexistent-ps5u", now, 1) == 0);
}

int main(void) {
    char cmd[600];
    signal(SIGPIPE, SIG_IGN);
    snprintf(g_root, sizeof g_root, "/tmp/ps5u-webui-files-%d", (int)getpid());
    snprintf(cmd, sizeof cmd, "rm -rf '%s'", g_root);
    system(cmd);
    if (mkdir(g_root, 0755) != 0) {
        perror(g_root);
        return 1;
    }
    {
        char t[4200];
        CHECK(wf_temp_path("/data/x/file.bin", t, sizeof t) == 0 && strncmp(t, "/data/x/.ps5u-", 14) == 0 &&
              strstr(t, ".part") != NULL);
        CHECK(wf_temp_path("/top.bin", t, sizeof t) == 0 && strncmp(t, "/.ps5u-", 7) == 0);
        CHECK(wf_temp_path("/data/x/file.bin", t, 10) != 0);
    }
    test_text_save();
    test_receive();
    test_range();
    test_disposition();
    test_tar_header();
    test_tar_stream();
    test_rate();
    test_sweep();
    system(cmd);
    if (g_fail) return 1;
    printf("webui_files selftest: ok\n");
    return 0;
}
