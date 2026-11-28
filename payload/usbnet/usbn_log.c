/* See usbn_log.h. */
#include "usbn_log.h"

#include "usbnet_proto.h"

#include <fcntl.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#define LOG_MAX_BYTES (256 * 1024)

static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static int g_fd = -1;
static long long g_start_ms;

static long long mono_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

void usbn_log_open(void) {
    struct stat st;
    time_t now = time(NULL);
    struct tm tm;
    pthread_mutex_lock(&g_mu);
    g_start_ms = mono_ms();
    if (stat(USBNET_LOG_PATH, &st) == 0 && st.st_size > LOG_MAX_BYTES / 2) rename(USBNET_LOG_PATH, USBNET_LOG_PATH ".old");
    g_fd = open(USBNET_LOG_PATH, O_WRONLY | O_CREAT | O_APPEND, 0644);
    pthread_mutex_unlock(&g_mu);
    gmtime_r(&now, &tm);
    say("start, %04d-%02d-%02d %02d:%02d:%02d UTC, pid %d", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour,
        tm.tm_min, tm.tm_sec, (int)getpid());
}

void say(const char *fmt, ...) {
    char line[512];
    long long t;
    int k, n;
    va_list ap;
    pthread_mutex_lock(&g_mu);
    t = mono_ms() - g_start_ms;
    k = snprintf(line, sizeof line, "%lld.%03lld ", t / 1000, t % 1000);
    va_start(ap, fmt);
    n = vsnprintf(line + k, sizeof line - (size_t)k - 1, fmt, ap);
    va_end(ap);
    if (n < 0) n = 0;
    if ((size_t)(k + n) > sizeof line - 2) n = (int)(sizeof line - 2) - k;
    line[k + n] = '\n';
    if (g_fd >= 0) {
        struct stat st;
        if (fstat(g_fd, &st) == 0 && st.st_size > LOG_MAX_BYTES) {
            close(g_fd);
            rename(USBNET_LOG_PATH, USBNET_LOG_PATH ".old");
            g_fd = open(USBNET_LOG_PATH, O_WRONLY | O_CREAT | O_APPEND, 0644);
        }
        if (g_fd >= 0) (void)!write(g_fd, line, (size_t)(k + n + 1));
    }
    pthread_mutex_unlock(&g_mu);
}

void checkpoint(void) {
    pthread_mutex_lock(&g_mu);
    if (g_fd >= 0) fsync(g_fd);
    pthread_mutex_unlock(&g_mu);
}
