/* See include/smb_shares.h. */
#include "smb_shares.h"

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "monocypher.h"
#include "smbd_json.h"
#include "smbd_path.h"

#define LINE_MAX_BYTES 8192u
#define FILE_MAX_BYTES (SMB_SHARES_MAX * LINE_MAX_BYTES)

static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static const char *g_file = SMB_SHARES_PATH;
static const char *g_key_file = SMB_SHARES_KEY_PATH;

void smb_shares_set_paths(const char *file, const char *key_file) {
    g_file = file ? file : SMB_SHARES_PATH;
    g_key_file = key_file ? key_file : SMB_SHARES_KEY_PATH;
}

/* ---- pure ---- */

static void hex_of(const uint8_t *b, size_t n, char *out) {
    static const char d[] = "0123456789abcdef";
    size_t i;
    for (i = 0; i < n; i++) {
        out[2 * i] = d[b[i] >> 4];
        out[2 * i + 1] = d[b[i] & 15];
    }
    out[2 * n] = '\0';
}

static int nib(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

static int unhex(const char *s, uint8_t *out, size_t n) {
    size_t i;
    for (i = 0; i < n; i++) {
        int a = nib(s[2 * i]), b = nib(s[2 * i + 1]);
        if (a < 0 || b < 0) return -1;
        out[i] = (uint8_t)(a << 4 | b);
    }
    return 0;
}

int smb_share_to_line(const smb_share_t *s, const uint8_t key[32], char *out, size_t cap) {
    sj_buf_t b;
    int len = -1;
    sj_init(&b, cap);
    sj_raw(&b, "{");
    sj_kv_str(&b, 0, "id", s->id);
    sj_kv_str(&b, 1, "name", s->name);
    sj_kv_str(&b, 1, "host", s->host);
    sj_fmt(&b, ",\"port\":%u", (unsigned)(s->port ? s->port : 445));
    sj_kv_str(&b, 1, "share", s->share);
    sj_kv_str(&b, 1, "user", s->user);
    sj_kv_str(&b, 1, "domain", s->domain);
    sj_kv_str(&b, 1, "start_path", s->start_path);
    if (s->password[0]) {
        size_t pl = strlen(s->password);
        uint8_t nonce[24], mac[16], *ct = malloc(pl);
        char *hex = malloc(2 * (24 + 16 + pl) + 1);
        if (!ct || !hex) {
            free(ct);
            free(hex);
            sj_free(&b);
            return -1;
        }
        arc4random_buf(nonce, sizeof nonce);
        crypto_aead_lock(ct, mac, key, nonce, NULL, 0, (const uint8_t *)s->password, pl);
        hex_of(nonce, 24, hex);
        hex_of(mac, 16, hex + 48);
        hex_of(ct, pl, hex + 80);
        sj_kv_str(&b, 1, "secret", hex);
        crypto_wipe(ct, pl);
        free(ct);
        free(hex);
    }
    sj_raw(&b, "}");
    if (!b.err && b.buf && b.len < cap) {
        memcpy(out, b.buf, b.len + 1);
        len = (int)b.len;
    }
    sj_free(&b);
    return len;
}

int smb_share_from_line(const char *line, size_t n, const uint8_t key[32], smb_share_t *s) {
    char *hex;
    uint64_t port = 445;
    memset(s, 0, sizeof *s);
    if (sj_str(line, n, "id", s->id, sizeof s->id) != 1 || strlen(s->id) != SMB_ID_LEN) return -1;
    if (sj_str(line, n, "host", s->host, sizeof s->host) != 1) return -1;
    if (sj_str(line, n, "share", s->share, sizeof s->share) != 1) return -1;
    if (sj_str(line, n, "name", s->name, sizeof s->name) < 0) return -1;
    if (sj_str(line, n, "user", s->user, sizeof s->user) < 0) return -1;
    if (sj_str(line, n, "domain", s->domain, sizeof s->domain) < 0) return -1;
    if (sj_str(line, n, "start_path", s->start_path, sizeof s->start_path) < 0) return -1;
    if (sj_u64(line, n, "port", &port) < 0 || port == 0 || port > 65535) return -1;
    s->port = (uint16_t)port;
    hex = malloc(2 * (24 + 16 + SMB_FIELD_MAX) + 2);
    if (!hex) return -1;
    if (sj_str(line, n, "secret", hex, 2 * (24 + 16 + SMB_FIELD_MAX) + 2) == 1) {
        size_t hl = strlen(hex);
        uint8_t nonce[24], mac[16], ct[SMB_FIELD_MAX];
        size_t pl = hl >= 80 && hl % 2 == 0 ? (hl - 80) / 2 : 0;
        if (pl > 0 && pl < SMB_FIELD_MAX && unhex(hex, nonce, 24) == 0 && unhex(hex + 48, mac, 16) == 0 &&
            unhex(hex + 80, ct, pl) == 0 &&
            crypto_aead_unlock((uint8_t *)s->password, mac, key, nonce, NULL, 0, ct, pl) == 0) {
            s->password[pl] = '\0';
            s->has_password = strlen(s->password) == pl; /* a NUL inside: not a password we wrote */
            if (!s->has_password) crypto_wipe(s->password, sizeof s->password);
        }
    }
    free(hex);
    if (smb_share_check(s, NULL) != 0) {
        crypto_wipe(s->password, sizeof s->password);
        return -1;
    }
    return 0;
}

static int text_ok(const char *s, size_t max) {
    const unsigned char *p = (const unsigned char *)s;
    if (strlen(s) >= max) return 0;
    for (; *p; p++)
        if (*p < 0x20 || *p == 0x7f) return 0;
    return 1;
}

int smb_share_check(smb_share_t *s, const char **why) {
    char norm[sizeof s->start_path];
    const char *w = NULL;
    if (!smbp_host_ok(s->host)) w = "bad_host";
    else if (!smbp_share_ok(s->share)) w = "bad_share";
    else if (!text_ok(s->name, SMB_NAME_MAX)) w = "bad_name";
    else if (!text_ok(s->user, SMB_FIELD_MAX) || strchr(s->user, '\\') || strchr(s->user, '/')) w = "bad_user";
    else if (!text_ok(s->domain, SMB_FIELD_MAX) || strchr(s->domain, '\\') || strchr(s->domain, '/')) w = "bad_domain";
    else if (smbp_rel_normalize(s->start_path, norm, sizeof norm) != 0) w = "bad_path";
    if (w) {
        if (why) *why = w;
        return -1;
    }
    memcpy(s->start_path, norm, sizeof norm);
    if (!s->port) s->port = 445;
    if (!s->name[0]) snprintf(s->name, sizeof s->name, "%.60s/%.60s", s->host, s->share);
    return 0;
}

int smb_path_parse(const char *p, char id[SMB_ID_LEN + 1], char *rel, size_t cap) {
    const char *s, *slash;
    size_t i;
    if (!p || strncmp(p, SMB_PATH_PREFIX, sizeof SMB_PATH_PREFIX - 1) != 0) return -1;
    s = p + sizeof SMB_PATH_PREFIX - 1;
    slash = strchr(s, '/');
    if ((slash ? (size_t)(slash - s) : strlen(s)) != SMB_ID_LEN) return -1;
    for (i = 0; i < SMB_ID_LEN; i++)
        if (nib(s[i]) < 0 || (s[i] >= 'A' && s[i] <= 'F')) return -1;
    memcpy(id, s, SMB_ID_LEN);
    id[SMB_ID_LEN] = '\0';
    return smbp_rel_normalize(slash ? slash : "", rel, cap);
}

int smb_path_make(const char *id, const char *rel, char *out, size_t cap) {
    int k = snprintf(out, cap, "%s%s/%s", SMB_PATH_PREFIX, id, rel);
    if (k < 0 || (size_t)k >= cap) {
        if (cap) out[0] = '\0';
        return -1;
    }
    return 0;
}

/* ---- the file ---- */

static int ensure_dir(void) {
    char dir[512];
    const char *slash = strrchr(g_file, '/');
    size_t n = slash ? (size_t)(slash - g_file) : 0;
    if (!n || n >= sizeof dir) return 0;
    memcpy(dir, g_file, n);
    dir[n] = '\0';
    if (mkdir(dir, 0700) != 0 && errno != EEXIST) return -1;
    return 0;
}

static int write_whole(const char *path, const void *data, size_t len, mode_t mode) {
    char tmp[600];
    int fd, k;
    size_t done = 0;
    k = snprintf(tmp, sizeof tmp, "%s.tmp", path); /* same folder: the rename never crosses a mount */
    if (k < 0 || (size_t)k >= sizeof tmp) return -1;
    fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, mode);
    if (fd < 0) return -1;
    (void)fchmod(fd, mode);
    while (done < len) {
        ssize_t w = write(fd, (const char *)data + done, len - done);
        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) break;
        done += (size_t)w;
    }
    if (done != len || fsync(fd) != 0) {
        close(fd);
        unlink(tmp);
        return -1;
    }
    close(fd);
    if (rename(tmp, path) != 0) {
        unlink(tmp);
        return -1;
    }
    return 0;
}

/* The per-console key, made on first use. A new key is made ONLY when there is none (ENOENT) or
 * the file is read to its end and is not a key: a failure to open or read it (EMFILE, EACCES, an
 * I/O error) is an error, not a reason to replace the key, which would silently make every saved
 * share password undecryptable. */
static int load_key(uint8_t key[32]) {
    char hex[65];
    int fd;
    size_t got = 0;
    if (ensure_dir() != 0) return -1;
    fd = open(g_key_file, O_RDONLY);
    if (fd < 0) {
        if (errno != ENOENT) return -1;
    } else {
        for (;;) { /* the first 64 bytes (a trailing newline after them is tolerated, as before) */
            ssize_t n = read(fd, hex + got, 64 - got);
            if (n < 0 && errno == EINTR) continue;
            if (n < 0) {
                close(fd);
                return -1;
            }
            if (n == 0) break;
            got += (size_t)n;
            if (got >= 64) break;
        }
        close(fd);
        hex[got] = '\0';
        if (got == 64 && unhex(hex, key, 32) == 0) return 0;
        /* read to the end and not a key (empty, short, garbled): made again below */
    }
    arc4random_buf(key, 32);
    hex_of(key, 32, hex);
    if (write_whole(g_key_file, hex, 64, 0600) != 0) {
        crypto_wipe(key, 32);
        return -1;
    }
    return 0;
}

static int load_locked(smb_share_t *out, int max, const uint8_t key[32]) {
    char *buf;
    int fd, n = 0;
    ssize_t got;
    size_t len = 0;
    char *p;
    fd = open(g_file, O_RDONLY);
    if (fd < 0) return errno == ENOENT ? 0 : SMB_SHARES_E_IO;
    buf = malloc(FILE_MAX_BYTES + 1);
    if (!buf) {
        close(fd);
        return SMB_SHARES_E_IO;
    }
    /* To the end of the file, or not at all. A read that failed partway used to count as the end:
     * the shares read so far were taken for all of them, and the next save or remove wrote the
     * file back without the rest. */
    while (len < FILE_MAX_BYTES) {
        got = read(fd, buf + len, FILE_MAX_BYTES - len);
        if (got < 0 && errno == EINTR) continue;
        if (got < 0) {
            close(fd);
            crypto_wipe(buf, len);
            free(buf);
            return SMB_SHARES_E_IO;
        }
        if (got == 0) break;
        len += (size_t)got;
    }
    close(fd);
    buf[len] = '\0';
    p = buf;
    while (*p && n < max) {
        char *nl = strchr(p, '\n');
        size_t l = nl ? (size_t)(nl - p) : strlen(p);
        if (l && smb_share_from_line(p, l, key, &out[n]) == 0) n++; /* a bad line is dropped */
        if (!nl) break;
        p = nl + 1;
    }
    crypto_wipe(buf, len);
    free(buf);
    return n;
}

static int save_locked(const smb_share_t *v, int n, const uint8_t key[32]) {
    char *buf = malloc(FILE_MAX_BYTES), *line = malloc(LINE_MAX_BYTES);
    size_t len = 0;
    int i, rc = 0;
    if (!buf || !line) {
        free(buf);
        free(line);
        return SMB_SHARES_E_IO;
    }
    for (i = 0; i < n && rc == 0; i++) {
        int k = smb_share_to_line(&v[i], key, line, LINE_MAX_BYTES);
        if (k < 0 || len + (size_t)k + 1 > FILE_MAX_BYTES) {
            rc = SMB_SHARES_E_FULL;
            break;
        }
        memcpy(buf + len, line, (size_t)k);
        len += (size_t)k;
        buf[len++] = '\n';
    }
    if (rc == 0 && (ensure_dir() != 0 || write_whole(g_file, buf, len, 0600) != 0)) rc = SMB_SHARES_E_IO;
    free(buf);
    free(line);
    return rc;
}

int smb_shares_load(smb_share_t *out, int max) {
    uint8_t key[32];
    int n;
    pthread_mutex_lock(&g_mu);
    if (load_key(key) != 0) {
        pthread_mutex_unlock(&g_mu);
        return SMB_SHARES_E_IO;
    }
    n = load_locked(out, max, key);
    crypto_wipe(key, sizeof key);
    pthread_mutex_unlock(&g_mu);
    return n;
}

int smb_shares_get(const char *id, smb_share_t *out) {
    smb_share_t *v = calloc(SMB_SHARES_MAX, sizeof *v);
    int n, i, rc = SMB_SHARES_E_NOT_FOUND;
    if (!v) return SMB_SHARES_E_IO;
    n = smb_shares_load(v, SMB_SHARES_MAX);
    if (n < 0) rc = n;
    for (i = 0; i < n; i++) {
        if (strcmp(v[i].id, id) == 0) {
            *out = v[i];
            rc = 0;
            break;
        }
    }
    crypto_wipe(v, SMB_SHARES_MAX * sizeof *v);
    free(v);
    return rc;
}

static int change(smb_share_t *put, const char *remove_id) {
    smb_share_t *v = calloc(SMB_SHARES_MAX + 1, sizeof *v);
    uint8_t key[32];
    int n, i, rc = 0, at = -1;
    if (!v) return SMB_SHARES_E_IO;
    pthread_mutex_lock(&g_mu);
    if (load_key(key) != 0) {
        rc = SMB_SHARES_E_IO;
        goto out;
    }
    n = load_locked(v, SMB_SHARES_MAX, key);
    if (n < 0) {
        rc = n;
        goto out;
    }
    if (remove_id) {
        for (i = 0; i < n; i++)
            if (strcmp(v[i].id, remove_id) == 0) at = i;
        if (at < 0) {
            rc = SMB_SHARES_E_NOT_FOUND;
            goto out;
        }
        memmove(&v[at], &v[at + 1], (size_t)(n - at - 1) * sizeof *v);
        n--;
    } else {
        if (!put->id[0]) {
            uint8_t r[4];
            int clash;
            do {
                arc4random_buf(r, sizeof r);
                hex_of(r, 4, put->id);
                clash = 0;
                for (i = 0; i < n; i++)
                    if (strcmp(v[i].id, put->id) == 0) clash = 1;
            } while (clash);
        }
        for (i = 0; i < n; i++)
            if (strcmp(v[i].id, put->id) == 0) at = i;
        if (at < 0) {
            if (n >= SMB_SHARES_MAX) {
                rc = SMB_SHARES_E_FULL;
                goto out;
            }
            at = n++;
        }
        v[at] = *put;
    }
    rc = save_locked(v, n, key);
out:
    crypto_wipe(key, sizeof key);
    pthread_mutex_unlock(&g_mu);
    crypto_wipe(v, (SMB_SHARES_MAX + 1) * sizeof *v);
    free(v);
    return rc;
}

int smb_shares_put(smb_share_t *s) {
    if (smb_share_check(s, NULL) != 0) return SMB_SHARES_E_BAD;
    return change(s, NULL);
}

int smb_shares_remove(const char *id) { return change(NULL, id); }
