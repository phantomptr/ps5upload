#include "mgmt_page_keep.h"

#include <pthread.h>
#include <stdlib.h>
#include <string.h>

static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static const void *g_fn;
static uint64_t g_user;
static uint64_t g_at_ms;
static char *g_body;
static size_t g_len;

static void drop_locked(void) {
    free(g_body);
    g_body = NULL;
    g_len = 0;
    g_fn = NULL;
}

void mgmt_page_keep_store(const void *fn, uint64_t user, const char *body, size_t len, uint64_t now_ms) {
    char *copy = malloc(len ? len : 1);
    if (copy && len) memcpy(copy, body, len);
    pthread_mutex_lock(&g_lock);
    drop_locked();
    if (copy) {
        g_body = copy;
        g_len = len;
        g_fn = fn;
        g_user = user;
        g_at_ms = now_ms;
    }
    pthread_mutex_unlock(&g_lock);
}

int mgmt_page_keep_take(const void *fn, uint64_t user, uint64_t now_ms, char **out, size_t *len) {
    int hit = 0;
    pthread_mutex_lock(&g_lock);
    int fresh = g_body && now_ms >= g_at_ms && now_ms - g_at_ms <= MGMT_PAGE_KEEP_MS;
    if (g_body && !fresh) drop_locked();
    if (fresh && g_fn == fn && g_user == user) {
        char *copy = malloc(g_len ? g_len : 1);
        if (copy) {
            if (g_len) memcpy(copy, g_body, g_len);
            *out = copy;
            *len = g_len;
            hit = 1;
        }
    }
    pthread_mutex_unlock(&g_lock);
    return hit;
}

void mgmt_page_keep_clear(void) {
    pthread_mutex_lock(&g_lock);
    drop_locked();
    pthread_mutex_unlock(&g_lock);
}
