#include "tmdb.h"

#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#define TMDB_CACHE_DIR "/data/ps5upload/tmdb"
#define TMDB_ROOT_DIR  "/data/ps5upload"
#define TMDB_TTL_SEC   (30 * 24 * 60 * 60)

static int ensure_dirs(void) {
    struct stat st;
    if (stat(TMDB_ROOT_DIR, &st) != 0) mkdir(TMDB_ROOT_DIR, 0755);
    if (stat(TMDB_CACHE_DIR, &st) != 0) mkdir(TMDB_CACHE_DIR, 0755);
    return 0;
}

void tmdb_init(void) {
    ensure_dirs();
}

static int is_valid_title_id(const char *s) {
    if (!s || strlen(s) != 12) return 0;
    for (int i = 0; i < 4; i++)
        if (!isupper((unsigned char)s[i])) return 0;
    for (int i = 4; i < 9; i++)
        if (!isdigit((unsigned char)s[i])) return 0;
    if (s[9] != '_') return 0;
    if (!isdigit((unsigned char)s[10]) || !isdigit((unsigned char)s[11])) return 0;
    return 1;
}

/* Accepts a bare title id (9 chars, e.g. CUSA12345), a full title id
   (12 chars, CUSA12345_00) or a content id (36 chars). Extracts the
   12-char title id for use as the cache key. Returns 1 on success, 0
   on invalid input.

   The 9-char form matters: enumerating /user/appmeta -- which is how
   the app lists installed games -- yields bare title ids, so every
   lookup started from an installed game arrived here 9 characters long
   and was rejected as malformed. `_00` is the standard first-release
   suffix and is what that enumeration implies. */
static int normalize_id(const char *input, char *title_id_out, size_t out_sz) {
    if (!input || !title_id_out || out_sz < 13) return 0;
    size_t len = strlen(input);
    if (len == 9) {
        char padded[13];
        memcpy(padded, input, 9);
        memcpy(padded + 9, "_00", 4);   /* includes the NUL */
        if (!is_valid_title_id(padded)) return 0;
        memcpy(title_id_out, padded, 13);
        return 1;
    }
    if (len == 12) {
        if (!is_valid_title_id(input)) return 0;
        memcpy(title_id_out, input, 12);
        title_id_out[12] = '\0';
        return 1;
    }
    if (len == 36) {
        /* Format: XXXXXX-TITLEID_00-YYYYYYYYYYYYYYYY */
        if (input[6] != '-' || input[19] != '-') return 0;
        char tid[13];
        memcpy(tid, input + 7, 12);
        tid[12] = '\0';
        if (!is_valid_title_id(tid)) return 0;
        memcpy(title_id_out, tid, 13);
        return 1;
    }
    return 0;
}

static int read_file(const char *path, char *buf, size_t cap, size_t *out_len) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    ssize_t n = read(fd, buf, cap - 1);
    close(fd);
    if (n < 0) return -1;
    buf[n] = '\0';
    if (out_len) *out_len = (size_t)n;
    return 0;
}

static int write_file(const char *path, const char *data, size_t len) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return -1;
    size_t off = 0;
    while (off < len) {
        ssize_t n = write(fd, data + off, len - off);
        if (n <= 0) { close(fd); return -1; }
        off += (size_t)n;
    }
    close(fd);
    return 0;
}

int tmdb_fetch(const char *id, int refresh,
               char *buf, size_t cap, size_t *written) {
    if (!id || !buf || cap == 0) return -1;
    char title_id[13];
    if (!normalize_id(id, title_id, sizeof(title_id))) {
        int n = snprintf(buf, cap, "{\"ok\":false,\"error\":\"invalid_title_id\"}");
        if (written) *written = (size_t)(n > 0 ? n : 0);
        return 0;
    }

    char path[256];
    snprintf(path, sizeof(path), "%s/%s.json", TMDB_CACHE_DIR, title_id);

    if (!refresh) {
        struct stat st;
        if (stat(path, &st) == 0) {
            time_t age = time(NULL) - st.st_mtime;
            if (age < TMDB_TTL_SEC) {
                size_t len = 0;
                if (read_file(path, buf, cap, &len) == 0) {
                    if (written) *written = len;
                    return 0;
                }
            }
        }
    }

    int n = snprintf(buf, cap,
        "{\"ok\":false,\"error\":\"not_cached\","
        "\"title_id\":\"%s\"}", title_id);
    if (written) *written = (size_t)(n > 0 ? n : 0);
    return 0;
}

int tmdb_store(const char *id, const char *json, size_t len) {
    if (!id || !json) return -1;
    char title_id[13];
    if (!normalize_id(id, title_id, sizeof(title_id))) return -1;
    ensure_dirs();
    char path[256];
    snprintf(path, sizeof(path), "%s/%s.json", TMDB_CACHE_DIR, title_id);
    return write_file(path, json, len);
}
