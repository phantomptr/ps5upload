#include "icon_keeper.h"
#include "proc_identity.h"

#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#include "content_db.h"

#define KEEPER_PATH "/data/ps5upload/hidden_icons.txt"
#define KEEPER_TMP KEEPER_PATH ".tmp"
#define KEEPER_MAX 256
#define KEEPER_POLL_S 5

static pthread_mutex_t g_keeper_mu = PTHREAD_MUTEX_INITIALIZER;

/* Title ids are short and plain: [A-Z0-9_], at most 15 characters. */
static int keeper_valid(const char *id) {
    size_t n = id ? strlen(id) : 0;
    if (n == 0 || n > 15) return 0;
    for (size_t i = 0; i < n; i++) {
        char c = id[i];
        if (!((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '_'))
            return 0;
    }
    return 1;
}

/* Caller holds g_keeper_mu. Reads at most `max` ids. */
static int keeper_load(char ids[][16], int max) {
    FILE *f = fopen(KEEPER_PATH, "r");
    if (!f) return 0;
    int n = 0;
    char line[64];
    while (n < max && fgets(line, sizeof line, f)) {
        line[strcspn(line, "\r\n")] = 0;
        if (keeper_valid(line)) snprintf(ids[n++], 16, "%s", line);
    }
    fclose(f);
    return n;
}

/* Caller holds g_keeper_mu. Written aside and renamed in the same directory. */
static void keeper_save(char ids[][16], int n) {
    if (n == 0) {
        unlink(KEEPER_PATH);
        return;
    }
    FILE *f = fopen(KEEPER_TMP, "w");
    if (!f) return;
    for (int i = 0; i < n; i++) fprintf(f, "%s\n", ids[i]);
    if (fclose(f) == 0) rename(KEEPER_TMP, KEEPER_PATH);
    else unlink(KEEPER_TMP);
}

void icon_keeper_note(const char *title_id, int hidden) {
    static char ids[KEEPER_MAX][16];
    if (!keeper_valid(title_id)) return;
    pthread_mutex_lock(&g_keeper_mu);
    int n = keeper_load(ids, KEEPER_MAX), at = -1;
    for (int i = 0; i < n; i++)
        if (strcmp(ids[i], title_id) == 0) at = i;
    if (hidden && at < 0 && n < KEEPER_MAX) {
        snprintf(ids[n++], 16, "%s", title_id);
        keeper_save(ids, n);
    } else if (!hidden && at >= 0) {
        memmove(ids[at], ids[at + 1], (size_t)(n - at - 1) * sizeof ids[0]);
        keeper_save(ids, n - 1);
    }
    pthread_mutex_unlock(&g_keeper_mu);
}

/* app.db and its write-ahead log: the shell may write either. */
static time_t appdb_mtime(void) {
    struct stat st;
    time_t m = 0;
    if (stat(CONTENT_DB_APP, &st) == 0) m = st.st_mtime;
    if (stat(CONTENT_DB_APP "-wal", &st) == 0 && st.st_mtime > m) m = st.st_mtime;
    return m;
}

#define KEEPER_SEEDED "/data/ps5upload/hidden_icons.seeded"

/* Once per console: the icons hidden before the helper kept a list (7.0.7 to 7.0.22) are the
 * ones app.db has hidden now, leaving out the system's own (content_db_hidden_json drops them). */
static void keeper_seed_once(void) {
    static char json[8192];
    static char ids[KEEPER_MAX][16];
    if (access(KEEPER_SEEDED, F_OK) == 0) return;
    size_t len = 0;
    if (content_db_hidden_json(json, sizeof json, &len) != 0 || !strstr(json, "\"ok\":true")) return;
    const char *p = strstr(json, "\"hidden\":[");
    const char *stop = p ? strchr(p, ']') : NULL;
    if (!p || !stop) return;
    p = strchr(p, '[');
    pthread_mutex_lock(&g_keeper_mu);
    int n = keeper_load(ids, KEEPER_MAX);
    /* Each element is a quoted id: an opening quote, the id, a closing quote. */
    while ((p = strchr(p + 1, '"')) != NULL && p < stop) {
        const char *end = strchr(p + 1, '"');
        if (!end || end > stop || (size_t)(end - p - 1) >= 16) break;
        char id[16];
        memcpy(id, p + 1, (size_t)(end - p - 1));
        id[end - p - 1] = 0;
        p = end;
        if (!keeper_valid(id)) continue;
        int have = 0;
        for (int i = 0; i < n; i++) have |= strcmp(ids[i], id) == 0;
        if (!have && n < KEEPER_MAX) snprintf(ids[n++], 16, "%s", id);
    }
    keeper_save(ids, n);
    pthread_mutex_unlock(&g_keeper_mu);
    FILE *f = fopen(KEEPER_SEEDED, "w");
    if (f) fclose(f);
    if (n > 0) fprintf(stderr, "[icons] keeping %d hidden icon(s) hidden\n", n);
}

static void *icon_keeper_thread(void *arg) {
    static char ids[KEEPER_MAX][16];
    static const char *ptrs[KEEPER_MAX];
    (void)arg;
    proc_name_set_self("ps5upload-icons");
    keeper_seed_once();
    time_t seen = 0;
    for (;;) {
        time_t m = appdb_mtime();
        if (m != 0 && m != seen) {
            /* Held across the write: an icon_keeper_note(id, 0) (the tile turned on again, which
             * then shows its icon) waits for it, so a pass that loaded the old list cannot hide
             * the icon again right after it was shown. */
            pthread_mutex_lock(&g_keeper_mu);
            int n = keeper_load(ids, KEEPER_MAX);
            if (n > 0) {
                for (int i = 0; i < n; i++) ptrs[i] = ids[i];
                char err[160] = {0};
                int hid = content_db_icons_rehide(ptrs, n, err, sizeof err);
                if (hid > 0)
                    fprintf(stderr, "[icons] hid %d icon(s) again that the system showed\n", hid);
                else if (hid < 0)
                    fprintf(stderr, "[icons] could not hide icons again: %s\n", err);
                /* Our own write moved the mtime: that change is not one to answer. */
                m = appdb_mtime();
            }
            pthread_mutex_unlock(&g_keeper_mu);
            seen = m;
        }
        sleep(KEEPER_POLL_S);
    }
    return NULL;
}

void icon_keeper_start(void) {
    pthread_t t;
    pthread_attr_t a;
    if (pthread_attr_init(&a) != 0) return;
    (void)pthread_attr_setdetachstate(&a, PTHREAD_CREATE_DETACHED);
    (void)pthread_attr_setstacksize(&a, 256u * 1024u);
    if (pthread_create(&t, &a, icon_keeper_thread, NULL) != 0)
        fprintf(stderr, "[icons] failed to start the icon keeper\n");
    pthread_attr_destroy(&a);
}
