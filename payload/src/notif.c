#include "notif.h"
#include "sony_api_lock.h"

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

/* The toast on the TV.
 *
 * This used to be `sceNotificationSend(level, text)`. That function takes THREE arguments (user id,
 * a flag, a JSON template: runtime.c's toast.send and the SDK's notify sample call it so), so the
 * text went in as the flag and the template was whatever the third argument register held: no
 * toast was ever shown (cheat on/off, the crash-guard warning, the Remote Play PIN), and a stale
 * pointer there was read as a string inside the helper. It was also a link-time import of a
 * library not every firmware has.
 *
 * So the same request the helper's other toasts use (main.c pop_notification), looked up at run
 * time: a firmware without it shows nothing and the entry is still kept in the ring. No lock is
 * taken here: the caller holds sony_api_lock (notif.h). */
#ifndef NOTIF_SHOW
#include <dlfcn.h>
typedef struct {
    char _reserved[45];
    char message[3075];
} notif_req_t;
typedef int (*notif_send_request_fn)(int, notif_req_t *, size_t, int);
static void notif_show(const char *text) {
    static notif_send_request_fn send_fn;
    notif_send_request_fn fn = send_fn;
    notif_req_t *req;
    if (!fn) {
        fn = (notif_send_request_fn)dlsym(RTLD_DEFAULT, "sceKernelSendNotificationRequest");
        if (!fn) return;
        send_fn = fn; /* the same value from whichever thread gets here first */
    }
    req = calloc(1, sizeof *req); /* 3 KiB: not on a caller's stack */
    if (!req) return;
    strncpy(req->message, text, sizeof req->message - 1);
    (void)fn(0, req, sizeof *req, 0);
    free(req);
}
#define NOTIF_SHOW(text) notif_show(text)
#endif

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
#ifndef NOTIF_STORE_PATH
#define NOTIF_STORE_PATH PS5UPLOAD2_RUNTIME_ROOT "/notifications.dat"
#define NOTIF_STORE_TMP  PS5UPLOAD2_RUNTIME_ROOT "/notifications.dat.tmp"
#endif

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
        /* No "\n" at the end of the format: in scanf that is "any run of whitespace", and it ate
         * the spaces, tabs and newlines a message STARTS with. The fread below then ran on into the
         * next entry's header, and that entry and every one after it came back wrong or not at all.
         * Exactly one byte ends the header line. */
        int got = fscanf(f, "%llu %llu %d %zu", &seq, &ts, &level, &mlen);
        if (got != 4 || fgetc(f) != '\n') break;
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

/* The length of the whole, valid UTF-8 character at `p` (2-4), or 0: a stray or cut byte, an
 * overlong form, an encoded surrogate, a code point past U+10FFFF. The NUL ends every check. */
static size_t notif_utf8_len(const unsigned char *p) {
    unsigned char c = p[0];
    if (c >= 0xC2 && c <= 0xDF) return (p[1] & 0xC0) == 0x80 ? 2 : 0;
    if (c >= 0xE0 && c <= 0xEF) {
        if ((p[1] & 0xC0) != 0x80 || (p[2] & 0xC0) != 0x80) return 0;
        if ((c == 0xE0 && p[1] < 0xA0) || (c == 0xED && p[1] >= 0xA0)) return 0;
        return 3;
    }
    if (c >= 0xF0 && c <= 0xF4) {
        if ((p[1] & 0xC0) != 0x80 || (p[2] & 0xC0) != 0x80 || (p[3] & 0xC0) != 0x80) return 0;
        if ((c == 0xF0 && p[1] < 0x90) || (c == 0xF4 && p[1] >= 0x90)) return 0;
        return 4;
    }
    return 0;
}

static size_t json_escape_append(char *buf, size_t cap, size_t off,
                                 const char *src) {
    for (size_t i = 0; src[i] != '\0'; i++) {
        char c = src[i];
        char tmp[8];
        const char *esc;
        size_t esc_len;

        /* A message can name a cheat from a Latin-1 or Shift-JIS file, and is cut at 255 bytes:
         * a byte that is not part of a whole UTF-8 character reads '?'. Sent as it was, the
         * engine could not parse the list at all, and the message stays in the saved store. */
        if ((unsigned char)c >= 0x80) {
            size_t k = notif_utf8_len((const unsigned char *)src + i);
            if (k == 0) {
                if (off + 2 >= cap) return off;
                buf[off++] = '?';
                continue;
            }
            if (off + k + 1 >= cap) return off;
            memcpy(buf + off, src + i, k);
            off += k;
            i += k - 1;
            continue;
        }

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

/* Empty the ring and the on-disk snapshot.
 *
 * The sequence counter deliberately keeps counting rather than
 * resetting to zero. Clients poll with `since_seq`, so rewinding it
 * would make already-seen sequence numbers valid again and a client
 * holding a high watermark would go silent until the counter caught
 * back up.
 *
 * Returns the number of entries removed. */
int notif_clear(void) {
    pthread_mutex_lock(&s_ring_mtx);
    int removed = (int)s_count;
    memset(s_ring, 0, sizeof(s_ring));
    s_write_idx = 0;
    s_count = 0;
    /* Persist the now-empty ring so the clear survives a payload
     * reload; falling back to unlink keeps us empty either way. */
    notif_persist_locked();
    pthread_mutex_unlock(&s_ring_mtx);
    return removed;
}

int notif_send(const char *msg, int level) {
    return notif_send_kept(msg, msg, level);
}

int notif_send_kept(const char *shown, const char *msg, int level) {
    if (!msg || !shown) return -1;
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

    NOTIF_SHOW(shown);
    return 0;
}

int notif_send_serialised(const char *msg, int level) {
    pthread_mutex_lock(&sony_api_lock);
    int rc = notif_send(msg, level);
    pthread_mutex_unlock(&sony_api_lock);
    return rc;
}

/* The longest tail: "],\"seq\":" and 20 digits, "}" and the NUL. */
#define NOTIF_LIST_TAIL 32
/* One entry: its head (a comma, two 20-digit numbers and a level), the message with every byte
 * escaped ("\u00xx", six for one), the closing quote and brace, and the escaper's spare byte. */
#define NOTIF_LIST_HEAD 96
#define NOTIF_LIST_ENTRY (NOTIF_LIST_HEAD + (NOTIF_MSG_MAX - 1) * 6 + 2 + 2)

int notif_list(uint64_t since_seq, char *buf, size_t cap,
               size_t *written) {
    static const char head[] = "{\"notifications\":[";
    if (!buf || cap == 0) return -1;

    /* Room for the head and the tail is kept before anything else, and an entry goes in whole or
     * not at all: a full buffer means fewer entries, never a reply cut in the middle of one. (It
     * used to leave a comma and half an entry before the tail, which the engine could not parse:
     * the list then stayed unreadable until it was cleared.) */
    if (cap < sizeof(head) + NOTIF_LIST_TAIL) {
        buf[0] = '\0';
        if (written) *written = 0;
        return 0;
    }
    memcpy(buf, head, sizeof(head) - 1);
    size_t off = sizeof(head) - 1;
    const size_t lim = cap - NOTIF_LIST_TAIL;

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

        char ent[NOTIF_LIST_ENTRY];
        int n = snprintf(ent, NOTIF_LIST_HEAD,
            "%s{\"seq\":%llu,\"ts\":%llu,\"level\":\"%s\",\"msg\":\"",
            first ? "" : ",",
            (unsigned long long)e->seq,
            (unsigned long long)e->timestamp,
            level_to_str(e->level));
        if (n < 0 || n >= NOTIF_LIST_HEAD) break;
        /* 2 kept back for the quote and the brace (the escaper stops a byte short of its cap). */
        size_t el = json_escape_append(ent, sizeof(ent) - 2, (size_t)n, e->message);
        ent[el++] = '"';
        ent[el++] = '}';
        if (off + el > lim) break;
        memcpy(buf + off, ent, el);
        off += el;
        first = 0;
    }

    pthread_mutex_unlock(&s_ring_mtx);

    int n = snprintf(buf + off, cap - off, "],\"seq\":%llu}",
                     (unsigned long long)latest_seq);
    if (n > 0 && (size_t)n < cap - off) off += (size_t)n;
    buf[off] = '\0';
    if (written) *written = off;
    return 0;
}
