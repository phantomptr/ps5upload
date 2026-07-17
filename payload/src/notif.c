#include "notif.h"

#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "config.h"

extern int sceNotificationSend(int32_t type, const char *msg);

#define NOTIF_RING_SIZE 64
#define NOTIF_MSG_MAX   256

/* On-PS5 persistence so the ring survives a payload redeploy. The
 * payload links no JSON parser (runtime.c only has single-field
 * extractors, not an array parser), so instead of the reference's
 * cJSON-encoded notifications.json we use a small length-prefixed text
 * format that is trivial and robust to parse without a library — the
 * message field carries the byte count up front, so arbitrary content
 * (delimiters, embedded newlines) round-trips exactly.
 *
 * Format (v1):
 *   NOTIF1 <seq_counter>\n
 *   <seq> <ts> <level> <msglen>\n
 *   <msglen bytes of message>\n
 *   ... one (header line + body line) pair per entry, oldest first ...
 *
 * Written to a .tmp sibling then atomically renamed, so a crash mid-write
 * never corrupts the live file. Same durability pattern as the reference. */
#define NOTIF_STORE_PATH PS5UPLOAD2_RUNTIME_ROOT "/notifications.dat"
#define NOTIF_STORE_TMP  PS5UPLOAD2_RUNTIME_ROOT "/notifications.dat.tmp"

typedef struct {
    uint64_t seq;
    uint64_t timestamp;
    char     message[NOTIF_MSG_MAX];
    int      level;
    int      used;
} notif_entry_t;

static notif_entry_t s_ring[NOTIF_RING_SIZE];
static pthread_mutex_t s_ring_mtx = PTHREAD_MUTEX_INITIALIZER;
static uint64_t s_seq_counter = 0;
static size_t   s_write_idx = 0;
static size_t   s_count = 0;

static const char *level_to_str(int level) {
    switch (level) {
        case NOTIF_LEVEL_WARN:  return "warn";
        case NOTIF_LEVEL_ERROR: return "error";
        default:                return "info";
    }
}

static int valid_level(int level) {
    return level == NOTIF_LEVEL_INFO ||
           level == NOTIF_LEVEL_WARN ||
           level == NOTIF_LEVEL_ERROR;
}

/* Serialize the ring to disk, oldest entry first. Caller must hold
 * s_ring_mtx. Best-effort: any I/O failure leaves the previous on-disk
 * snapshot intact (we only rename over it once the temp write fully
 * succeeds). */
static void notif_persist_locked(void) {
    FILE *f = fopen(NOTIF_STORE_TMP, "w");
    if (!f) return;

    if (fprintf(f, "NOTIF1 %llu\n", (unsigned long long)s_seq_counter) < 0) {
        fclose(f);
        unlink(NOTIF_STORE_TMP);
        return;
    }

    /* Oldest entry sits at (write_idx - count) mod SIZE. */
    size_t start = (s_write_idx + NOTIF_RING_SIZE - s_count) % NOTIF_RING_SIZE;
    for (size_t i = 0; i < s_count; i++) {
        notif_entry_t *e = &s_ring[(start + i) % NOTIF_RING_SIZE];
        if (!e->used) continue;
        size_t mlen = strnlen(e->message, NOTIF_MSG_MAX - 1);
        if (fprintf(f, "%llu %llu %d %zu\n",
                    (unsigned long long)e->seq,
                    (unsigned long long)e->timestamp,
                    e->level, mlen) < 0 ||
            fwrite(e->message, 1, mlen, f) != mlen ||
            fputc('\n', f) == EOF) {
            fclose(f);
            unlink(NOTIF_STORE_TMP);
            return;
        }
    }

    if (fclose(f) != 0) {
        unlink(NOTIF_STORE_TMP);
        return;
    }
    if (rename(NOTIF_STORE_TMP, NOTIF_STORE_PATH) != 0) {
        unlink(NOTIF_STORE_TMP);
    }
}

void notif_init(void) {
    FILE *f = fopen(NOTIF_STORE_PATH, "r");
    if (!f) return;

    pthread_mutex_lock(&s_ring_mtx);

    unsigned long long saved_seq = 0;
    if (fscanf(f, "NOTIF1 %llu\n", &saved_seq) != 1) {
        /* Unrecognized header — ignore the file rather than guess. */
        pthread_mutex_unlock(&s_ring_mtx);
        fclose(f);
        return;
    }
    s_seq_counter = (uint64_t)saved_seq;

    for (;;) {
        unsigned long long seq = 0, ts = 0;
        int level = 0;
        size_t mlen = 0;
        int got = fscanf(f, "%llu %llu %d %zu\n", &seq, &ts, &level, &mlen);
        if (got != 4) break;
        if (mlen >= NOTIF_MSG_MAX) mlen = NOTIF_MSG_MAX - 1;

        notif_entry_t *entry = &s_ring[s_write_idx];
        entry->seq = (uint64_t)seq;
        entry->timestamp = (uint64_t)ts;
        entry->level = valid_level(level) ? level : NOTIF_LEVEL_INFO;
        size_t rd = fread(entry->message, 1, mlen, f);
        /* Short read: the file ended before mlen bytes. If we just NUL-
         * terminate at rd and continue, the subsequent fgetc/fscanf would
         * be reading from the wrong offset (mid-message or past EOF),
         * corrupting every remaining entry. Best-effort: keep what we
         * read (truncated message is still useful), but stop parsing
         * further entries — the file is truncated/corrupt past here. */
        entry->message[rd] = '\0';
        entry->used = 1;
        /* Consume the newline that terminates the body line. */
        int c = fgetc(f);
        if (c != '\n' && c != EOF) ungetc(c, f);

        s_write_idx = (s_write_idx + 1) % NOTIF_RING_SIZE;
        if (s_count < NOTIF_RING_SIZE) s_count++;
        if ((uint64_t)seq > s_seq_counter) s_seq_counter = (uint64_t)seq;

        /* If fread got fewer bytes than the header promised, the rest
         * of the file is unreliable — stop here rather than feed garbage
         * to the next fscanf iteration. */
        if (rd < mlen) break;
    }

    pthread_mutex_unlock(&s_ring_mtx);
    fclose(f);
}

static size_t json_escape_append(char *buf, size_t cap, size_t off,
                                 const char *src) {
    for (size_t i = 0; src[i] != '\0'; i++) {
        char c = src[i];
        char tmp[8];
        const char *esc;
        size_t esc_len;

        switch (c) {
            case '"':  esc = "\\\""; esc_len = 2; break;
            case '\\': esc = "\\\\"; esc_len = 2; break;
            case '\b': esc = "\\b";  esc_len = 2; break;
            case '\f': esc = "\\f";  esc_len = 2; break;
            case '\n': esc = "\\n";  esc_len = 2; break;
            case '\r': esc = "\\r";  esc_len = 2; break;
            case '\t': esc = "\\t";  esc_len = 2; break;
            default:
                if ((unsigned char)c < 0x20) {
                    int n = snprintf(tmp, sizeof(tmp),
                                     "\\u%04x", (unsigned char)c);
                    if (n < 0 || (size_t)n >= sizeof(tmp)) {
                        return off;
                    }
                    esc = tmp;
                    esc_len = (size_t)n;
                } else {
                    tmp[0] = c;
                    esc = tmp;
                    esc_len = 1;
                }
                break;
        }

        if (off + esc_len + 1 >= cap) {
            return off;
        }
        memcpy(buf + off, esc, esc_len);
        off += esc_len;
    }
    return off;
}

int notif_send(const char *msg, int level) {
    if (!msg) return -1;
    if (!valid_level(level)) level = NOTIF_LEVEL_INFO;

    size_t msg_len = strlen(msg);
    if (msg_len >= NOTIF_MSG_MAX) msg_len = NOTIF_MSG_MAX - 1;

    pthread_mutex_lock(&s_ring_mtx);
    uint64_t seq = ++s_seq_counter;
    uint64_t ts = (uint64_t)time(NULL);

    notif_entry_t *entry = &s_ring[s_write_idx];
    entry->seq = seq;
    entry->timestamp = ts;
    memcpy(entry->message, msg, msg_len);
    entry->message[msg_len] = '\0';
    entry->level = level;
    entry->used = 1;

    s_write_idx = (s_write_idx + 1) % NOTIF_RING_SIZE;
    if (s_count < NOTIF_RING_SIZE) s_count++;
    notif_persist_locked();
    pthread_mutex_unlock(&s_ring_mtx);

    sceNotificationSend((int32_t)level, msg);
    return 0;
}

static size_t append_str(char *buf, size_t cap, size_t off,
                         const char *str) {
    size_t len = strlen(str);
    if (off + len + 1 >= cap) {
        return off;
    }
    memcpy(buf + off, str, len);
    return off + len;
}

int notif_list(uint64_t since_seq, char *buf, size_t cap,
               size_t *written) {
    if (!buf || cap == 0) return -1;

    size_t off = append_str(buf, cap, 0, "{\"notifications\":[");
    if (off == 0) {
        buf[cap - 1] = '\0';
        if (written) *written = cap - 1;
        return 0;
    }

    uint64_t latest_seq;
    int first = 1;

    pthread_mutex_lock(&s_ring_mtx);

    latest_seq = s_seq_counter;
    size_t available = s_count;
    size_t start_base = (s_count < NOTIF_RING_SIZE)
        ? 0
        : s_write_idx;

    for (size_t i = 0; i < available; i++) {
        size_t idx = (start_base + i) % NOTIF_RING_SIZE;
        notif_entry_t *e = &s_ring[idx];
        if (!e->used) continue;
        if (e->seq <= since_seq) continue;

        if (!first) {
            if (off + 1 + 1 >= cap) break;
            buf[off++] = ',';
        }

        char prefix[96];
        int n = snprintf(prefix, sizeof(prefix),
            "{\"seq\":%llu,\"ts\":%llu,\"level\":\"%s\",\"msg\":\"",
            (unsigned long long)e->seq,
            (unsigned long long)e->timestamp,
            level_to_str(e->level));
        if (n < 0 || (size_t)n >= sizeof(prefix)) {
            break;
        }
        off = append_str(buf, cap, off, prefix);
        if (off + 1 >= cap) break;

        off = json_escape_append(buf, cap, off, e->message);
        if (off + 3 >= cap) break;

        buf[off++] = '"';
        buf[off++] = '}';
        first = 0;
    }

    pthread_mutex_unlock(&s_ring_mtx);

    char suffix[40];
    int n = snprintf(suffix, sizeof(suffix), "],\"seq\":%llu}",
                     (unsigned long long)latest_seq);
    if (n > 0 && (size_t)n < sizeof(suffix)) {
        off = append_str(buf, cap, off, suffix);
    } else {
        if (off + 1 < cap) buf[off++] = ']';
    }

    if (off >= cap) off = cap - 1;
    buf[off] = '\0';
    if (written) *written = off;
    return 0;
}
