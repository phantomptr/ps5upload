/* Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the
 * Free Software Foundation; either version 3, or (at your option) any
 * later version.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>
#include <errno.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <stdarg.h>
#include <pthread.h>
#include <fcntl.h>
#include <limits.h>
#include <ifaddrs.h>
#include <signal.h>
#include <time.h>
#include <sys/syscall.h>
#include <sys/mman.h>
#include <sched.h>
#include <stdatomic.h>

#include <ps5/kernel.h>

/* sys_budget_set - removes resource budget constraints so kernel won't kill us */
static int sys_budget_set(long budget) {
    return (int)syscall(0x23b, budget);
}

/*
 * Some firmware/exploit combinations are sensitive to aggressive ucred/jail edits
 * during early payload init. Keep startup conservative by default.
 */
#ifndef ENABLE_AGGRESSIVE_PRIV_ESC
#define ENABLE_AGGRESSIVE_PRIV_ESC 0
#endif

/*
 * Stability defaults:
 * - Avoid real-time scheduler by default (can starve system services under heavy I/O).
 * - Avoid mlockall(MCL_FUTURE) by default (can amplify memory pressure during uploads).
 * Both can be re-enabled explicitly at build time for advanced tuning.
 */
#ifndef ENABLE_RT_SCHED
#define ENABLE_RT_SCHED 0
#endif

#ifndef ENABLE_MLOCKALL
#define ENABLE_MLOCKALL 0
#endif

#if ENABLE_AGGRESSIVE_PRIV_ESC
/* Full capability mask - grants all permissions */
static const uint8_t g_full_caps[16] = {
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff
};

/* Auth IDs from etaHEN - grants system-level privileges */
#define AUTHID_SYSTEM_PROCESS   0x4800000000010003ULL  /* System process auth ID */
#define AUTHID_SHELLCORE        0x4800000000000007ULL  /* ShellCore auth ID */
#endif

#include "config.h"
#include "storage.h"
#include "protocol.h"
#include "extract.h"
#include "notify.h"
#include "transfer.h"
#include "unrar_handler.h"
#include "extract_queue.h"

// Binary upload protocol (PS5-Upload-Suite compatible)
#define CMD_START_UPLOAD 0x10
#define CMD_UPLOAD_CHUNK 0x11
#define CMD_END_UPLOAD   0x12

#define RESP_OK    0x01
#define RESP_ERROR 0x02
#define RESP_DATA  0x03
#define RESP_READY 0x04
#define RESP_PROGRESS 0x05
#define BINARY_UPLOAD_CTRL_MAX (64 * 1024)
#define BINARY_UPLOAD_CHUNK_MAX (64 * 1024 * 1024)

typedef struct FileMutexEntry {
    char path[PATH_MAX];
    pthread_mutex_t mutex;
    int ref_count;
    struct FileMutexEntry *next;
} FileMutexEntry;

static pthread_mutex_t g_file_mutex_list_lock = PTHREAD_MUTEX_INITIALIZER;
static FileMutexEntry *g_file_mutex_list = NULL;

static pthread_mutex_t *get_file_mutex(const char *path) {
    if (!path) return NULL;
    pthread_mutex_lock(&g_file_mutex_list_lock);
    FileMutexEntry *entry = g_file_mutex_list;
    while (entry) {
        if (strcmp(entry->path, path) == 0) {
            entry->ref_count++;
            pthread_mutex_unlock(&g_file_mutex_list_lock);
            return &entry->mutex;
        }
        entry = entry->next;
    }
    entry = (FileMutexEntry *)malloc(sizeof(FileMutexEntry));
    if (!entry) {
        pthread_mutex_unlock(&g_file_mutex_list_lock);
        return NULL;
    }
    memset(entry, 0, sizeof(*entry));
    strncpy(entry->path, path, sizeof(entry->path) - 1);
    pthread_mutex_init(&entry->mutex, NULL);
    entry->ref_count = 1;
    entry->next = g_file_mutex_list;
    g_file_mutex_list = entry;
    pthread_mutex_unlock(&g_file_mutex_list_lock);
    return &entry->mutex;
}

static void release_file_mutex(const char *path) {
    if (!path) return;
    pthread_mutex_lock(&g_file_mutex_list_lock);
    FileMutexEntry **pp = &g_file_mutex_list;
    while (*pp) {
        FileMutexEntry *entry = *pp;
        if (strcmp(entry->path, path) == 0) {
            entry->ref_count--;
            if (entry->ref_count <= 0) {
                *pp = entry->next;
                pthread_mutex_destroy(&entry->mutex);
                free(entry);
            }
            break;
        }
        pp = &entry->next;
    }
    pthread_mutex_unlock(&g_file_mutex_list_lock);
}

static void normalize_path(char *path) {
    if (!path) return;
    char *src = path;
    char *dst = path;
    int prev_slash = 0;
    while (*src) {
        if (*src == '/') {
            if (!prev_slash) *dst++ = *src;
            prev_slash = 1;
        } else {
            *dst++ = *src;
            prev_slash = 0;
        }
        src++;
    }
    *dst = '\0';
}

static int mkdir_recursive(const char *path) {
    if (!path || !*path) return -1;
    char tmp[PATH_MAX];
    snprintf(tmp, sizeof(tmp), "%s", path);
    normalize_path(tmp);
    size_t len = strlen(tmp);
    if (len == 0) return -1;
    if (tmp[len - 1] == '/') tmp[len - 1] = '\0';
    for (char *p = tmp + 1; *p; p++) {
        if (*p == '/') {
            *p = '\0';
            if (mkdir(tmp, 0777) != 0 && errno != EEXIST) {
                return -1;
            }
            chmod(tmp, 0777);
            *p = '/';
        }
    }
    if (mkdir(tmp, 0777) != 0 && errno != EEXIST) {
        return -1;
    }
    chmod(tmp, 0777);
    return 0;
}

static int send_all_bytes(int sock, const void *data, size_t len) {
    const uint8_t *p = (const uint8_t *)data;
    size_t sent = 0;
    while (sent < len) {
        ssize_t n = send(sock, p + sent, len - sent, 0);
        if (n < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (n == 0) return -1;
        sent += (size_t)n;
    }
    return 0;
}

// Forward declaration used by socket helpers below.
void payload_touch_activity(void);

static int read_exact_bytes(int sock, void *buf, size_t len) {
    uint8_t *p = (uint8_t *)buf;
    size_t got = 0;
    while (got < len) {
        ssize_t n = recv(sock, p + got, len - got, 0);
        if (n < 0) {
            if (errno == EINTR) continue;
            if (errno == 0 || errno == EEXIST) {
                // On PS5/FreeBSD we occasionally observe recv() failures with an
                // unrelated stale errno value (for example EEXIST) after peer drops.
                // Prefer socket-level error and otherwise normalize to ECONNRESET.
                int soerr = 0;
                socklen_t slen = sizeof(soerr);
                if (getsockopt(sock, SOL_SOCKET, SO_ERROR, &soerr, &slen) == 0 && soerr != 0) {
                    errno = soerr;
                } else {
                    errno = ECONNRESET;
                }
            }
            return -1;
        }
        if (n == 0) {
            // Peer performed an orderly shutdown.
            errno = ECONNRESET;
            return -1;
        }
        got += (size_t)n;
        // Keep idle watchdog from killing long-running binary uploads while
        // we are actively receiving data.
        payload_touch_activity();
    }
    return 0;
}

static int discard_exact_bytes(int sock, uint32_t len) {
    uint8_t scratch[1024];
    uint32_t remaining = len;
    while (remaining > 0) {
        size_t take = remaining > (uint32_t)sizeof(scratch) ? sizeof(scratch) : (size_t)remaining;
        if (read_exact_bytes(sock, scratch, take) != 0) {
            return -1;
        }
        remaining -= (uint32_t)take;
    }
    return 0;
}

static int send_response(int sock, uint8_t code, const void *data, uint32_t len) {
    uint8_t header[5];
    header[0] = code;
    memcpy(header + 1, &len, 4);
    if (send_all_bytes(sock, header, sizeof(header)) != 0) return -1;
    if (len > 0 && data) {
        if (send_all_bytes(sock, data, len) != 0) return -1;
    }
    return 0;
}

static pthread_mutex_t g_log_mutex = PTHREAD_MUTEX_INITIALIZER;
static char g_last_op[64] = {0};
static char g_last_path[PATH_MAX] = {0};
static char g_last_path2[PATH_MAX] = {0};
static const char *g_pid_file = "/data/ps5upload/payload.pid";
static const char *g_kill_file = "/data/ps5upload/kill.req";
static const char *g_run_state_file = "/data/ps5upload/run_state.log";
static const char *g_heartbeat_file = "/data/ps5upload/heartbeat.log";
static volatile time_t g_last_activity = 0;
static const int g_idle_timeout_sec = IDLE_TIMEOUT_SEC;
static const int g_transfer_stall_sec = 60;
static const int g_extract_stall_sec = 60;
static atomic_int g_binary_sessions_active = 0;
static atomic_int g_command_threads_active = 0;
static pthread_mutex_t g_upload_snapshot_mutex = PTHREAD_MUTEX_INITIALIZER;
static char g_upload_snapshot_path[PATH_MAX] = {0};
static uint64_t g_upload_snapshot_received = 0;
static uint64_t g_upload_snapshot_size = 0;
static volatile time_t g_upload_snapshot_updated = 0;

void payload_log(const char *fmt, ...);
void payload_touch_activity(void);
#if defined(PS5UPLOAD_DEBUGGER_BUILD)
static void debugger_log_stream_broadcast(const char *line);
#endif

static void write_state_file_line(const char *path, const char *line) {
    if (!path || !line) return;
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0) return;
    write(fd, line, strlen(line));
    close(fd);
}

static void append_state_file_line(const char *path, const char *line) {
    if (!path || !line) return;
    int fd = open(path, O_WRONLY | O_CREAT | O_APPEND, 0666);
    if (fd < 0) return;
    write(fd, line, strlen(line));
    close(fd);
}

static void mark_run_state(const char *state, const char *detail) {
    char line[768];
    time_t now = time(NULL);
    snprintf(line, sizeof(line), "ts=%ld pid=%d state=%s detail=%s\n",
             (long)now, (int)getpid(), state ? state : "unknown", detail ? detail : "");
    append_state_file_line(g_run_state_file, line);
}

static void update_upload_snapshot(const char *path, uint64_t received, uint64_t size) {
    pthread_mutex_lock(&g_upload_snapshot_mutex);
    if (path) {
        snprintf(g_upload_snapshot_path, sizeof(g_upload_snapshot_path), "%s", path);
    } else {
        g_upload_snapshot_path[0] = '\0';
    }
    g_upload_snapshot_received = received;
    g_upload_snapshot_size = size;
    g_upload_snapshot_updated = time(NULL);
    pthread_mutex_unlock(&g_upload_snapshot_mutex);
}

static void *heartbeat_thread(void *arg) {
    (void)arg;
    while (1) {
        char path[PATH_MAX];
        uint64_t received = 0;
        uint64_t size = 0;
        time_t updated = 0;
        pthread_mutex_lock(&g_upload_snapshot_mutex);
        snprintf(path, sizeof(path), "%s", g_upload_snapshot_path);
        received = g_upload_snapshot_received;
        size = g_upload_snapshot_size;
        updated = g_upload_snapshot_updated;
        pthread_mutex_unlock(&g_upload_snapshot_mutex);

        time_t now = time(NULL);
        char line[1024];
        snprintf(line, sizeof(line),
                 "ts=%ld pid=%d transfer_active=%d bin_sessions=%d cmd_threads=%d last_activity=%ld upload_path=%s upload_received=%llu upload_size=%llu upload_updated=%ld\n",
                 (long)now,
                 (int)getpid(),
                 transfer_is_active(),
                 atomic_load(&g_binary_sessions_active),
                 atomic_load(&g_command_threads_active),
                 (long)g_last_activity,
                 path[0] ? path : "(none)",
                 (unsigned long long)received,
                 (unsigned long long)size,
                 (long)updated);
        write_state_file_line(g_heartbeat_file, line);
        usleep(1000 * 1000);
    }
    return NULL;
}

static void *kill_watch_thread(void *arg) {
    (void)arg;
    while (1) {
        if (access(g_kill_file, F_OK) == 0) {
            unlink(g_kill_file);
            notify_info("PS5 Upload Server", "Kill request received.");
            transfer_cleanup();
            extract_queue_reset();
            unlink(g_pid_file);
            exit(EXIT_SUCCESS);
        }
        usleep(250 * 1000);
    }
    return NULL;
}

static void *idle_watch_thread(void *arg) {
    (void)arg;
    static int transfer_stall_count = 0;
    static int abort_in_progress = 0;
    static time_t abort_request_time = 0;
    static unsigned long long abort_last_recv = 0;
    static unsigned long long abort_last_written = 0;
    static time_t abort_last_progress_time = 0;
    const int g_stall_grace_period_sec = 15; // 15s grace period
    const int g_abort_stall_sec = 30; // 30s stuck after abort => force exit

    while (1) {
        time_t now = time(NULL);

        if (abort_in_progress) {
            TransferStats stats;
            transfer_get_stats(&stats);
            if (abort_last_progress_time == 0) {
                abort_last_progress_time = now;
                abort_last_recv = stats.bytes_received;
                abort_last_written = stats.bytes_written;
            }
            if (stats.bytes_received != abort_last_recv || stats.bytes_written != abort_last_written) {
                abort_last_recv = stats.bytes_received;
                abort_last_written = stats.bytes_written;
                abort_last_progress_time = now;
                abort_request_time = now; // extend grace if progress resumes
            }

            if ((now - abort_request_time) < g_stall_grace_period_sec) {
                // In grace period, do nothing but sleep
            } else if (transfer_is_active() && (now - abort_last_progress_time) >= g_abort_stall_sec) {
                payload_log("[WATCHDOG] Abort stuck: no progress for %ds after abort (sessions=%zu packs=%zu queue=%zu). Forcing exit.",
                            g_abort_stall_sec, stats.active_sessions, stats.pack_in_use, stats.queue_count);
                notify_error("PS5 Upload Server", "Transfer stuck after abort. Restarting.");
                transfer_cleanup();
                extract_queue_reset();
                unlink(g_pid_file);
                exit(EXIT_FAILURE);
            } else if (!transfer_is_active()) {
                abort_in_progress = 0;
                abort_last_progress_time = 0;
            }
        } else {
            abort_in_progress = 0; // Grace period over, resume normal checks

            if (transfer_is_active()) {
                time_t last = transfer_last_progress();
                if (last > 0 && (now - last) > g_transfer_stall_sec) {
                    transfer_stall_count++;
                    payload_log("[WATCHDOG] Transfer stall detected (count=%d): %ld sec without progress",
                                transfer_stall_count, (long)(now - last));

                    if (transfer_stall_count > 1) {
                        TransferStats stats;
                        transfer_get_stats(&stats);
                        payload_log("[WATCHDOG] Unrecoverable stall. State: sessions=%zu packs=%zu queue=%zu pool=%d",
                                    stats.active_sessions, stats.pack_in_use, stats.queue_count, stats.pool_count);
                        payload_log("[WATCHDOG] Forcing payload exit.");
                        notify_error("PS5 Upload Server", "Unrecoverable stall. Restarting.");
                        transfer_cleanup();
                        extract_queue_reset();
                        unlink(g_pid_file);
                        exit(EXIT_FAILURE);
                    } else {
                        payload_log("[WATCHDOG] Attempting to abort stalled transfer. Entering %ds grace period.", g_stall_grace_period_sec);
                        notify_info("PS5 Upload Server", "Transfer stalled. Attempting recovery...");
                        transfer_request_abort_with_reason("watchdog_stall");
                        abort_in_progress = 1;
                        abort_request_time = now;
                        abort_last_progress_time = now;
                        TransferStats stats;
                        transfer_get_stats(&stats);
                        abort_last_recv = stats.bytes_received;
                        abort_last_written = stats.bytes_written;
                    }
                } else {
                    if (transfer_stall_count > 0) {
                        payload_log("[WATCHDOG] Transfer has recovered from stall.");
                        notify_info("PS5 Upload Server", "Transfer recovered from stall.");
                    }
                    transfer_stall_count = 0;
                }
            } else {
                transfer_stall_count = 0;
            }
        }

        if (extract_queue_is_running()) {
            time_t last = extract_queue_get_last_progress();
            if (last > 0 && (now - last) > g_extract_stall_sec) {
                payload_log("[WATCHDOG] Extract stall: %ld sec without progress", (long)(now - last));
            }
        }

        if (g_idle_timeout_sec > 0 &&
            g_last_activity > 0 &&
            (now - g_last_activity) > g_idle_timeout_sec) {
            if (transfer_is_active() || extract_queue_is_running() || atomic_load(&g_binary_sessions_active) > 0) {
                payload_log("[WATCHDOG] Idle timeout but active transfer/extract. activity_age=%ld", (long)(now - g_last_activity));
                g_last_activity = now;
            } else {
                payload_log("[WATCHDOG] Idle timeout hit (%d sec). Exiting.", g_idle_timeout_sec);
                notify_info("PS5 Upload Server", "Idle watchdog timeout.");
                transfer_cleanup();
                extract_queue_reset();
                unlink(g_pid_file);
                exit(EXIT_SUCCESS);
            }
        }
        usleep(1000 * 1000); // Check every 1 second
    }
    return NULL;
}

static void payload_log_rotate(void) {
    mkdir("/data/ps5upload", 0777);
    mkdir("/data/ps5upload/logs", 0777);

    time_t now = time(NULL);
    struct tm tm_info;
    localtime_r(&now, &tm_info);
    char stamp[32];
    strftime(stamp, sizeof(stamp), "%Y%m%d_%H%M%S", &tm_info);

    char archived_main[256];
    char archived_logs[256];
    snprintf(archived_main, sizeof(archived_main),
             "/data/ps5upload/logs/payload_%s.log", stamp);
    snprintf(archived_logs, sizeof(archived_logs),
             "/data/ps5upload/logs/payload_latest_%s.log", stamp);

    if (access("/data/ps5upload/payload.log", F_OK) == 0) {
        rename("/data/ps5upload/payload.log", archived_main);
    }
    if (access("/data/ps5upload/logs/payload.log", F_OK) == 0) {
        rename("/data/ps5upload/logs/payload.log", archived_logs);
    }
}

static void payload_log_write(const char *path, const char *line) {
    int fd = open(path, O_WRONLY | O_CREAT | O_APPEND, 0666);
    if (fd >= 0) {
        write(fd, line, strlen(line));
        write(fd, "\n", 1);
        close(fd);
    }
}

void payload_log(const char *fmt, ...) {
    pthread_mutex_lock(&g_log_mutex);
    mkdir("/data/ps5upload", 0777);
    mkdir("/data/ps5upload/logs", 0777);
    char line[512];
    va_list args;
    va_start(args, fmt);
    vsnprintf(line, sizeof(line), fmt, args);
    va_end(args);
    payload_log_write("/data/ps5upload/payload.log", line);
    payload_log_write("/data/ps5upload/logs/payload.log", line);
#if defined(PS5UPLOAD_DEBUGGER_BUILD)
    debugger_log_stream_broadcast(line);
#endif
    pthread_mutex_unlock(&g_log_mutex);
}

void payload_touch_activity(void) {
    g_last_activity = time(NULL);
}

void payload_set_crash_context(const char *op, const char *path, const char *path2) {
    if (op) {
        snprintf(g_last_op, sizeof(g_last_op), "%s", op);
    } else {
        g_last_op[0] = '\0';
    }
    if (path) {
        snprintf(g_last_path, sizeof(g_last_path), "%s", path);
    } else {
        g_last_path[0] = '\0';
    }
    if (path2) {
        snprintf(g_last_path2, sizeof(g_last_path2), "%s", path2);
    } else {
        g_last_path2[0] = '\0';
    }
}

static const char *signal_name(int sig) {
    switch (sig) {
        case SIGSEGV: return "SIGSEGV";
        case SIGABRT: return "SIGABRT";
        case SIGBUS:  return "SIGBUS";
        case SIGILL:  return "SIGILL";
        case SIGFPE:  return "SIGFPE";
        case SIGPIPE: return "SIGPIPE";
        case SIGTERM: return "SIGTERM";
        case SIGKILL: return "SIGKILL";
        default:      return "UNKNOWN";
    }
}

static void crash_handler(int sig) {
    mark_run_state("crash", signal_name(sig));
    // Minimal, signal-safe logging to avoid deadlocks on mutex.
    int fd = open("/data/ps5upload/payload_crash.log", O_WRONLY | O_CREAT | O_APPEND, 0666);
    if (fd >= 0) {
        char buf[512];
        time_t now = time(NULL);
        int len = snprintf(buf, sizeof(buf),
            "\n========== CRASH REPORT ==========\n"
            "Time: %ld\n"
            "Signal: %d (%s)\n"
            "PID: %d\n"
            "Transfer active: %d\n"
            "Last progress: %ld sec ago\n"
            "Last op: %s\n"
            "Last path: %s\n"
            "Last path2: %s\n"
            "===================================\n",
            (long)now,
            sig, signal_name(sig),
            (int)getpid(),
            transfer_is_active(),
            (long)(now - transfer_last_progress()),
            g_last_op,
            g_last_path,
            g_last_path2
        );
        if (len > 0) {
            write(fd, buf, (size_t)len);
        }
        fsync(fd);
        close(fd);
    }
    // Also write to main log
    int fd2 = open("/data/ps5upload/payload.log", O_WRONLY | O_CREAT | O_APPEND, 0666);
    if (fd2 >= 0) {
        char buf[128];
        int len = snprintf(buf, sizeof(buf), "[CRASH] signal=%d (%s)\n", sig, signal_name(sig));
        if (len > 0) {
            write(fd2, buf, (size_t)len);
        }
        fsync(fd2);
        close(fd2);
    }
    int fd3 = open("/data/ps5upload/logs/payload.log", O_WRONLY | O_CREAT | O_APPEND, 0666);
    if (fd3 >= 0) {
        char buf[128];
        int len = snprintf(buf, sizeof(buf), "[CRASH] signal=%d (%s)\n", sig, signal_name(sig));
        if (len > 0) {
            write(fd3, buf, (size_t)len);
        }
        fsync(fd3);
        close(fd3);
    }
    _exit(1);
}

static void exit_handler(void) {
    mark_run_state("exit", "atexit");
    // Log clean exits for debugging
    int fd = open("/data/ps5upload/payload_exit.log", O_WRONLY | O_CREAT | O_APPEND, 0666);
    if (fd >= 0) {
        char buf[256];
        time_t now = time(NULL);
        int len = snprintf(buf, sizeof(buf),
            "[EXIT] time=%ld pid=%d transfer_active=%d\n",
            (long)now, (int)getpid(), transfer_is_active()
        );
        if (len > 0) {
            write(fd, buf, (size_t)len);
        }
        close(fd);
    }
}

static void write_pid_file(void) {
    int fd = open(g_pid_file, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0) {
        return;
    }
    char buf[32];
    int len = snprintf(buf, sizeof(buf), "%d\n", (int)getpid());
    if (len > 0) {
        write(fd, buf, (size_t)len);
    }
    close(fd);
}

static int create_server_socket(int port) {
    int sock = socket(AF_INET, SOCK_STREAM, 0);
    if(sock < 0) {
        perror("socket");
        return -1;
    }

    // Allow reuse of address
    int opt = 1;
    setsockopt(sock, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));

    // Large socket buffers for high throughput on GigE networks
    int rcv_buf = SOCKET_RCVBUF_SIZE;
    int snd_buf = SOCKET_SNDBUF_SIZE;
    setsockopt(sock, SOL_SOCKET, SO_RCVBUF, &rcv_buf, sizeof(rcv_buf));
    setsockopt(sock, SOL_SOCKET, SO_SNDBUF, &snd_buf, sizeof(snd_buf));

    // Prevent SIGPIPE on write to closed socket (BSD/PS5 specific)
    int no_sigpipe = 1;
    setsockopt(sock, SOL_SOCKET, SO_NOSIGPIPE, &no_sigpipe, sizeof(no_sigpipe));

    struct sockaddr_in addr = {0};
    addr.sin_family = AF_INET;
    addr.sin_port = htons(port);
    addr.sin_addr.s_addr = INADDR_ANY;

    if(bind(sock, (struct sockaddr*)&addr, sizeof(addr)) < 0) {
        perror("bind");
        close(sock);
        return -1;
    }

    if(listen(sock, MAX_CONNECTIONS) < 0) {
        perror("listen");
        close(sock);
        return -1;
    }

    return sock;
}


static int is_localhost(const struct sockaddr_in *addr) {
    return addr->sin_addr.s_addr == htonl(INADDR_LOOPBACK);
}

static void get_local_ip(char *out, size_t out_len) {
    if (!out || out_len == 0) {
        return;
    }
    out[0] = '\0';

    struct ifaddrs *ifaddr = NULL;
    if (getifaddrs(&ifaddr) != 0) {
        return;
    }

    for (struct ifaddrs *ifa = ifaddr; ifa; ifa = ifa->ifa_next) {
        if (!ifa->ifa_addr || ifa->ifa_addr->sa_family != AF_INET) {
            continue;
        }
        struct sockaddr_in *sa = (struct sockaddr_in *)ifa->ifa_addr;
        if (sa->sin_addr.s_addr == htonl(INADDR_LOOPBACK)) {
            continue;
        }
        inet_ntop(AF_INET, &sa->sin_addr, out, out_len);
        break;
    }
    freeifaddrs(ifaddr);
}

static int request_shutdown(void) {
    int sock = socket(AF_INET, SOCK_STREAM, 0);
    if(sock < 0) {
        return -1;
    }

    struct sockaddr_in addr = {0};
    addr.sin_family = AF_INET;
    addr.sin_port = htons(SERVER_PORT);
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);

    if(connect(sock, (struct sockaddr*)&addr, sizeof(addr)) < 0) {
        close(sock);
        return -1;
    }

    const char *cmd = "SHUTDOWN\n";
    send(sock, cmd, strlen(cmd), 0);

    struct timeval tv;
    tv.tv_sec = 2; // 2s timeout
    tv.tv_usec = 0;
    setsockopt(sock, SOL_SOCKET, SO_RCVTIMEO, (const char*)&tv, sizeof tv);

    char buffer[64] = {0};
    int bytes = recv(sock, buffer, sizeof(buffer) - 1, 0);
    close(sock);

    if(bytes <= 0) {
        return -1;
    }

    buffer[bytes] = '\0';
    return (strncmp(buffer, "OK", 2) == 0) ? 0 : -1;
}

typedef enum {
    CONN_CMD = 0,
    CONN_UPLOAD = 1,
} ConnMode;

struct ClientConnection {
    int sock;
    struct sockaddr_in addr;
    ConnMode mode;
    char cmd_buffer[CMD_BUFFER_SIZE];
    size_t cmd_len;
};

static void process_command(struct ClientConnection *conn);

struct CommandRequest {
    int sock;
    struct sockaddr_in addr;
    char buffer[CMD_BUFFER_SIZE];
    size_t len;
};

struct DispatchArgs {
    int sock;
    struct sockaddr_in addr;
};

struct CommQueue {
    struct CommandRequest items[64];
    int head;
    int tail;
    int count;
    pthread_mutex_t mutex;
    pthread_cond_t cond;
};

static struct CommQueue g_comm_queue = {
    .head = 0,
    .tail = 0,
    .count = 0,
    .mutex = PTHREAD_MUTEX_INITIALIZER,
    .cond = PTHREAD_COND_INITIALIZER
};

static int pthread_create_detached_with_stack(void *(*fn)(void *), void *arg) {
    pthread_t tid;
    pthread_attr_t attr;
    if (pthread_attr_init(&attr) != 0) {
        return pthread_create(&tid, NULL, fn, arg);
    }
    // Bound per-thread virtual memory usage; important on PS5/FreeBSD 11 budgets.
    (void)pthread_attr_setstacksize(&attr, THREAD_STACK_SIZE);
    (void)pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    int rc = pthread_create(&tid, &attr, fn, arg);
    pthread_attr_destroy(&attr);
    return rc;
}

static int read_command_line(int sock, char *out, size_t cap, size_t *out_len);
static int is_comm_command(const char *cmd);
static int is_noisy_comm_command(const char *cmd);
static void enqueue_comm_request(const struct CommandRequest *req);
__attribute__((unused)) static void *comm_worker_thread(void *arg);
static int parse_first_token(const char *src, char *out, size_t out_cap, const char **rest);

typedef struct UploadSession {
    int sock;
    int upload_fd;
    char upload_path[PATH_MAX];
    uint64_t upload_size;
    uint64_t upload_received;
    uint64_t current_offset;
    pthread_mutex_t *file_mutex;
    uint8_t *chunk_buf;
    size_t chunk_cap;
    uint8_t *io_buf;
    size_t io_cap;
    uint64_t next_progress_log_at;
    time_t last_progress_log_at;
} UploadSession;

static void send_error_msg(int sock, const char *msg) {
    uint32_t len = msg ? (uint32_t)strlen(msg) : 0;
    send_response(sock, RESP_ERROR, msg, len);
}

static void send_ok_msg(int sock, const char *msg) {
    uint32_t len = msg ? (uint32_t)strlen(msg) : 0;
    send_response(sock, RESP_OK, msg, len);
}

static int handle_start_upload(UploadSession *session, const uint8_t *data, uint32_t data_len) {
    if (!session || !data || data_len < 1 + 8 + 8) {
        send_error_msg(session ? session->sock : -1, "Invalid upload request");
        return -1;
    }

    const uint8_t *nul = memchr(data, '\0', data_len);
    if (!nul) {
        send_error_msg(session->sock, "Invalid upload path");
        return -1;
    }
    size_t path_len = (size_t)(nul - data);
    if (path_len == 0 || path_len >= PATH_MAX) {
        send_error_msg(session->sock, "Upload path too long");
        return -1;
    }
    size_t tail_off = path_len + 1;
    if (tail_off + 16 > data_len) {
        send_error_msg(session->sock, "Invalid upload metadata");
        return -1;
    }

    char norm_path[PATH_MAX];
    memcpy(norm_path, data, path_len);
    norm_path[path_len] = '\0';
    normalize_path(norm_path);

    if (!is_path_safe(norm_path)) {
        send_error_msg(session->sock, "Invalid path");
        return -1;
    }

    uint64_t file_size = 0;
    uint64_t chunk_offset = 0;
    memcpy(&file_size, data + tail_off, 8);
    memcpy(&chunk_offset, data + tail_off + 8, 8);
    if (chunk_offset > file_size) {
        send_error_msg(session->sock, "Invalid chunk offset");
        return -1;
    }

    if (session->upload_fd >= 0) {
        close(session->upload_fd);
        session->upload_fd = -1;
    }
    if (session->file_mutex) {
        release_file_mutex(session->upload_path);
        session->file_mutex = NULL;
    }

    char parent[PATH_MAX];
    strncpy(parent, norm_path, sizeof(parent) - 1);
    parent[sizeof(parent) - 1] = '\0';
    char *slash = strrchr(parent, '/');
    if (slash && slash != parent) {
        *slash = '\0';
        if (mkdir_recursive(parent) != 0) {
            send_error_msg(session->sock, "Failed to create parent directory");
            return -1;
        }
    }

    session->file_mutex = get_file_mutex(norm_path);
    if (!session->file_mutex) {
        send_error_msg(session->sock, "Cannot allocate file mutex");
        return -1;
    }

    pthread_mutex_lock(session->file_mutex);
    if (chunk_offset > 0) {
        session->upload_fd = open(norm_path, O_WRONLY);
    } else {
        session->upload_fd = open(norm_path, O_WRONLY | O_CREAT | O_TRUNC, 0777);
        if (session->upload_fd >= 0 && file_size > (uint64_t)(100 * 1024 * 1024)) {
            if (lseek(session->upload_fd, (off_t)(file_size - 1), SEEK_SET) < 0 ||
                write(session->upload_fd, "", 1) != 1) {
                close(session->upload_fd);
                session->upload_fd = -1;
                pthread_mutex_unlock(session->file_mutex);
                release_file_mutex(norm_path);
                session->file_mutex = NULL;
                unlink(norm_path);
                send_error_msg(session->sock, "Disk full - cannot pre-allocate file");
                return -1;
            }
        }
    }
    pthread_mutex_unlock(session->file_mutex);

    if (session->upload_fd < 0) {
        release_file_mutex(norm_path);
        session->file_mutex = NULL;
        send_error_msg(session->sock, "Cannot open file");
        return -1;
    }

    strncpy(session->upload_path, norm_path, sizeof(session->upload_path) - 1);
    session->upload_path[sizeof(session->upload_path) - 1] = '\0';
    session->upload_size = file_size;
    session->upload_received = chunk_offset;
    session->current_offset = chunk_offset;
    {
        const uint64_t step = 16ULL * 1024ULL * 1024ULL;
        uint64_t base = (session->upload_received / step) + 1;
        session->next_progress_log_at = base * step;
    }
    session->last_progress_log_at = time(NULL);
    payload_log("[UPLOAD] start path=%s size=%llu offset=%llu",
                session->upload_path,
                (unsigned long long)session->upload_size,
                (unsigned long long)session->current_offset);
    update_upload_snapshot(session->upload_path, session->upload_received, session->upload_size);

    // Bulk upload sockets can benefit from a larger receive buffer, but keep it bounded.
    int upload_buf = UPLOAD_RCVBUF_SIZE;
    setsockopt(session->sock, SOL_SOCKET, SO_RCVBUF, &upload_buf, sizeof(upload_buf));

    send_response(session->sock, RESP_READY, NULL, 0);
    return 0;
}

static int handle_upload_chunk_stream(UploadSession *session, int sock, uint32_t data_len) {
    if (!session || session->upload_fd < 0 || !session->file_mutex) {
        send_error_msg(session ? session->sock : -1, "No upload in progress");
        return -1;
    }
    if (data_len == 0) {
        return 0;
    }
    if (session->upload_size > 0 && session->current_offset + data_len > session->upload_size) {
        payload_log("[UPLOAD] chunk exceeds size path=%s off=%llu len=%u size=%llu",
                    session->upload_path,
                    (unsigned long long)session->current_offset,
                    (unsigned int)data_len,
                    (unsigned long long)session->upload_size);
        send_error_msg(session->sock, "Chunk exceeds declared size");
        close(session->upload_fd);
        session->upload_fd = -1;
        release_file_mutex(session->upload_path);
        session->file_mutex = NULL;
        return -1;
    }

    if (!session->io_buf || session->io_cap < UPLOAD_RECV_CHUNK_SIZE) {
        uint8_t *next = (uint8_t *)realloc(session->io_buf, UPLOAD_RECV_CHUNK_SIZE);
        if (!next) {
            payload_log("[UPLOAD] OOM io_buf path=%s cap=%zu",
                        session->upload_path,
                        (size_t)UPLOAD_RECV_CHUNK_SIZE);
            send_error_msg(session->sock, "Out of memory");
            close(session->upload_fd);
            session->upload_fd = -1;
            release_file_mutex(session->upload_path);
            session->file_mutex = NULL;
            return -1;
        }
        session->io_buf = next;
        session->io_cap = UPLOAD_RECV_CHUNK_SIZE;
    }

    uint32_t remaining = data_len;
    uint64_t file_off = session->current_offset;

    pthread_mutex_lock(session->file_mutex);
    while (remaining > 0) {
        size_t take = (remaining > (uint32_t)session->io_cap) ? session->io_cap : (size_t)remaining;
        if (read_exact_bytes(sock, session->io_buf, take) != 0) {
            payload_log("[UPLOAD] read_exact failed path=%s off=%llu take=%zu errno=%d (%s)",
                        session->upload_path,
                        (unsigned long long)file_off,
                        take,
                        errno,
                        strerror(errno));
            pthread_mutex_unlock(session->file_mutex);
            send_error_msg(session->sock, "Connection lost");
            close(session->upload_fd);
            session->upload_fd = -1;
            release_file_mutex(session->upload_path);
            session->file_mutex = NULL;
            return -1;
        }

        size_t written_total = 0;
        while (written_total < take) {
            ssize_t written = pwrite(
                session->upload_fd,
                session->io_buf + written_total,
                take - written_total,
                (off_t)(file_off + written_total)
            );
            if (written < 0) {
                if (errno == EINTR) continue;
                payload_log("[UPLOAD] pwrite failed path=%s off=%llu len=%zu errno=%d (%s)",
                            session->upload_path,
                            (unsigned long long)(file_off + written_total),
                            (size_t)(take - written_total),
                            errno,
                            strerror(errno));
                pthread_mutex_unlock(session->file_mutex);
                send_error_msg(session->sock, "Write failed");
                close(session->upload_fd);
                session->upload_fd = -1;
                release_file_mutex(session->upload_path);
                session->file_mutex = NULL;
                return -1;
            }
            if (written == 0) {
                payload_log("[UPLOAD] pwrite returned 0 path=%s off=%llu len=%zu",
                            session->upload_path,
                            (unsigned long long)(file_off + written_total),
                            (size_t)(take - written_total));
                pthread_mutex_unlock(session->file_mutex);
                send_error_msg(session->sock, "Write failed");
                close(session->upload_fd);
                session->upload_fd = -1;
                release_file_mutex(session->upload_path);
                session->file_mutex = NULL;
                return -1;
            }
            written_total += (size_t)written;
        }

        file_off += take;
        remaining -= (uint32_t)take;
        payload_touch_activity();
        usleep(100);
    }
    pthread_mutex_unlock(session->file_mutex);

    session->current_offset += (uint64_t)data_len;
    session->upload_received += (uint64_t)data_len;
    update_upload_snapshot(session->upload_path, session->upload_received, session->upload_size);
    time_t now = time(NULL);
    int should_log_progress = 0;
    if (session->next_progress_log_at > 0 &&
        session->upload_received >= session->next_progress_log_at) {
        should_log_progress = 1;
    } else if (session->last_progress_log_at > 0 && (now - session->last_progress_log_at) >= 2) {
        should_log_progress = 1;
    }
    if (should_log_progress) {
        payload_log("[UPLOAD] progress path=%s received=%llu/%llu",
                    session->upload_path,
                    (unsigned long long)session->upload_received,
                    (unsigned long long)session->upload_size);
        if (session->next_progress_log_at > 0 &&
            session->upload_received >= session->next_progress_log_at) {
            while (session->upload_received >= session->next_progress_log_at) {
                session->next_progress_log_at += 16ULL * 1024ULL * 1024ULL;
            }
        }
        session->last_progress_log_at = now;
    }
    payload_touch_activity();
    return 0;
}

static int handle_end_upload(UploadSession *session) {
    if (!session || session->upload_fd < 0) {
        send_error_msg(session ? session->sock : -1, "No upload in progress");
        return -1;
    }
    close(session->upload_fd);
    session->upload_fd = -1;
    if (session->file_mutex) {
        release_file_mutex(session->upload_path);
        session->file_mutex = NULL;
    }
    chmod(session->upload_path, 0777);
    payload_log("[UPLOAD] complete path=%s size=%llu",
                session->upload_path,
                (unsigned long long)session->upload_received);
    update_upload_snapshot(NULL, 0, 0);
    send_ok_msg(session->sock, "Upload complete");
    payload_touch_activity();
    return 0;
}

static int handle_binary_session(int sock) {
    UploadSession session;
    memset(&session, 0, sizeof(session));
    session.sock = sock;
    session.upload_fd = -1;

    // Match proven behavior from PS5-Upload-Suite: no socket timeout for large uploads.
    struct timeval tv = {0};
    setsockopt(sock, SOL_SOCKET, SO_RCVTIMEO, (const char *)&tv, sizeof(tv));
    setsockopt(sock, SOL_SOCKET, SO_SNDTIMEO, (const char *)&tv, sizeof(tv));

    int keepalive = 1;
    setsockopt(sock, SOL_SOCKET, SO_KEEPALIVE, &keepalive, sizeof(keepalive));
#ifdef TCP_KEEPIDLE
    int keepidle = 10;
    setsockopt(sock, IPPROTO_TCP, TCP_KEEPIDLE, &keepidle, sizeof(keepidle));
#endif
#ifdef TCP_KEEPINTVL
    int keepintvl = 5;
    setsockopt(sock, IPPROTO_TCP, TCP_KEEPINTVL, &keepintvl, sizeof(keepintvl));
#endif
#ifdef TCP_KEEPCNT
    int keepcnt = 3;
    setsockopt(sock, IPPROTO_TCP, TCP_KEEPCNT, &keepcnt, sizeof(keepcnt));
#endif

    atomic_fetch_add(&g_binary_sessions_active, 1);
    int close_session = 0;
    const char *end_reason = "client_closed";
    int end_errno = 0;
    int upload_completed = 0;
    while (!close_session) {
        uint8_t header[5];
        if (read_exact_bytes(sock, header, sizeof(header)) != 0) {
            int err = errno;
            payload_log("[UPLOAD] session header read failed errno=%d (%s)", err, strerror(err));
            end_reason = "header_read_failed";
            end_errno = err;
            if (err == EAGAIN || err == EWOULDBLOCK) {
                send_error_msg(sock, "Receive timeout");
            } else {
                send_error_msg(sock, "Connection lost");
            }
            break;
        }
        uint8_t cmd = header[0];
        uint32_t data_len = 0;
        memcpy(&data_len, header + 1, 4);

        switch (cmd) {
            case CMD_START_UPLOAD:
                if (data_len == 0 || data_len > BINARY_UPLOAD_CTRL_MAX) {
                    if (data_len > 0 && discard_exact_bytes(sock, data_len) != 0) {
                        close_session = 1;
                    }
                    send_error_msg(sock, "Invalid start upload request");
                    break;
                }
                if (data_len > session.chunk_cap) {
                    uint8_t *next = (uint8_t *)realloc(session.chunk_buf, data_len);
                    if (!next) {
                        if (discard_exact_bytes(sock, data_len) != 0) {
                            close_session = 1;
                        }
                        send_error_msg(sock, "Out of memory");
                        close_session = 1;
                        break;
                    }
                    session.chunk_buf = next;
                    session.chunk_cap = data_len;
                }
                if (read_exact_bytes(sock, session.chunk_buf, data_len) != 0) {
                    int err = errno;
                    end_reason = "start_upload_read_failed";
                    end_errno = err;
                    if (err == EAGAIN || err == EWOULDBLOCK) {
                        send_error_msg(sock, "Receive timeout");
                    } else {
                        send_error_msg(sock, "Connection lost");
                    }
                    close_session = 1;
                    break;
                }
                if (handle_start_upload(&session, session.chunk_buf, data_len) != 0) {
                    // Error already sent
                }
                break;
            case CMD_UPLOAD_CHUNK:
                if (data_len > BINARY_UPLOAD_CHUNK_MAX) {
                    payload_log("[UPLOAD] chunk too large len=%u max=%u",
                                (unsigned int)data_len,
                                (unsigned int)BINARY_UPLOAD_CHUNK_MAX);
                    if (discard_exact_bytes(sock, data_len) != 0) {
                        close_session = 1;
                    }
                    send_error_msg(sock, "Chunk too large");
                    end_reason = "chunk_too_large";
                    close_session = 1;
                    break;
                }
                if (handle_upload_chunk_stream(&session, sock, data_len) != 0) {
                    // Error already sent
                    end_reason = "chunk_stream_failed";
                    end_errno = errno;
                    close_session = 1;
                }
                break;
            case CMD_END_UPLOAD:
                if (data_len != 0) {
                    if (discard_exact_bytes(sock, data_len) != 0) {
                        close_session = 1;
                    }
                    send_error_msg(sock, "Invalid end upload request");
                    end_reason = "invalid_end_upload";
                    close_session = 1;
                    break;
                }
                if (handle_end_upload(&session) != 0) {
                    // Error already sent
                    end_reason = "end_upload_failed";
                    end_errno = errno;
                } else {
                    upload_completed = 1;
                }
                break;
            default:
                if (data_len > 0) {
                    if (discard_exact_bytes(sock, data_len) != 0) {
                        close_session = 1;
                    }
                }
                send_error_msg(sock, "Unknown command");
                end_reason = "unknown_command";
                close_session = 1;
                break;
        }
    }

    if (session.upload_fd >= 0) {
        close(session.upload_fd);
    }
    if (session.file_mutex) {
        release_file_mutex(session.upload_path);
    }
    if (session.chunk_buf) {
        free(session.chunk_buf);
    }
    if (session.io_buf) {
        free(session.io_buf);
    }

    const char *err_str = strerror(end_errno);
    if (upload_completed && strcmp(end_reason, "header_read_failed") == 0 &&
        (end_errno == ECONNRESET || end_errno == 0)) {
        end_reason = "upload_complete";
    }
    payload_log("[UPLOAD] session end reason=%s errno=%d (%s) path=%s received=%llu/%llu",
                end_reason,
                end_errno,
                err_str,
                session.upload_path[0] ? session.upload_path : "(none)",
                (unsigned long long)session.upload_received,
                (unsigned long long)session.upload_size);

    atomic_fetch_sub(&g_binary_sessions_active, 1);
    return 0;
}

static int run_binary_session_in_worker_process(int sock) {
    if (sock < 0) {
        errno = EINVAL;
        return -1;
    }

    pid_t pid = fork();
    if (pid < 0) {
        payload_log("[UPLOAD] failed to fork binary worker errno=%d (%s)", errno, strerror(errno));
        return handle_binary_session(sock);
    }

    if (pid == 0) {
        int rc = handle_binary_session(sock);
        close(sock);
        _exit(rc == 0 ? 0 : 1);
    }

    // Parent keeps only supervisor role for this connection.
    close(sock);

    int status = 0;
    while (waitpid(pid, &status, 0) < 0) {
        if (errno == EINTR) continue;
        payload_log("[UPLOAD] waitpid failed for worker=%d errno=%d (%s)",
                    (int)pid, errno, strerror(errno));
        return -1;
    }

    if (WIFSIGNALED(status)) {
        payload_log("[UPLOAD] worker crashed pid=%d signal=%d", (int)pid, WTERMSIG(status));
        return -1;
    }
    if (WIFEXITED(status) && WEXITSTATUS(status) != 0) {
        payload_log("[UPLOAD] worker exited non-zero pid=%d code=%d",
                    (int)pid, WEXITSTATUS(status));
        return -1;
    }
    return 0;
}

static void close_connection(struct ClientConnection *conn) {
    if (!conn) {
        return;
    }
    if (conn->sock >= 0) {
        close(conn->sock);
        conn->sock = -1;
    }
}

static int set_blocking(int sock) {
    int flags = fcntl(sock, F_GETFL, 0);
    if (flags < 0) {
        return -1;
    }
    if (fcntl(sock, F_SETFL, flags & ~O_NONBLOCK) < 0) {
        return -1;
    }
    return 0;
}

__attribute__((unused)) static int parse_first_token(const char *src, char *out, size_t out_cap, const char **rest) {
    if (!src || !out || out_cap == 0) {
        return -1;
    }
    while (*src == ' ' || *src == '\t') {
        src++;
    }
    if (*src == '\0') {
        return -1;
    }
    size_t len = 0;
    if (*src == '"') {
        src++;
        while (*src && *src != '"') {
            char ch = *src++;
            if (ch == '\\') {
                char esc = *src++;
                if (esc == '\0') return -1;
                switch (esc) {
                    case 'n': ch = '\n'; break;
                    case 'r': ch = '\r'; break;
                    case 't': ch = '\t'; break;
                    case '\\': ch = '\\'; break;
                    case '"': ch = '"'; break;
                    default: ch = esc; break;
                }
            }
            if (len + 1 >= out_cap) return -1;
            out[len++] = ch;
        }
        if (*src != '"') return -1;
        src++;
    } else {
        while (*src != '\0' && *src != ' ' && *src != '\t') {
            if (len + 1 >= out_cap) return -1;
            out[len++] = *src++;
        }
    }
    out[len] = '\0';
    if (rest) {
        while (*src == ' ' || *src == '\t') {
            src++;
        }
        *rest = src;
    }
    return 0;
}

static void *command_thread_impl(void *arg) {
    struct DispatchArgs *args = (struct DispatchArgs *)arg;
    if (!args) {
        return NULL;
    }

    struct CommandRequest req;
    memset(&req, 0, sizeof(req));
    req.sock = args->sock;
    req.addr = args->addr;
    free(args);

    struct timeval tv;
    tv.tv_sec = 5;
    tv.tv_usec = 0;
    setsockopt(req.sock, SOL_SOCKET, SO_RCVTIMEO, (const char *)&tv, sizeof(tv));

    uint8_t peek = 0;
    ssize_t peeked = recv(req.sock, &peek, 1, MSG_PEEK);
    if (peeked <= 0) {
        close(req.sock);
        return NULL;
    }
    if (peek == CMD_START_UPLOAD || peek == CMD_UPLOAD_CHUNK || peek == CMD_END_UPLOAD) {
        run_binary_session_in_worker_process(req.sock);
        return NULL;
    }

    if (read_command_line(req.sock, req.buffer, sizeof(req.buffer), &req.len) != 0) {
        payload_log("[CONN] Failed to read command line");
        close(req.sock);
        return NULL;
    }
    g_last_activity = time(NULL);

    if (is_comm_command(req.buffer)) {
        if (!is_noisy_comm_command(req.buffer)) {
            payload_log("[COMM] %s", req.buffer);
        }
        enqueue_comm_request(&req);
        g_last_activity = time(NULL);
        return NULL;
    }

    struct ClientConnection conn;
    memset(&conn, 0, sizeof(conn));
    conn.sock = req.sock;
    conn.addr = req.addr;
    conn.mode = CONN_CMD;
    conn.cmd_len = req.len;
    if (conn.cmd_len >= sizeof(conn.cmd_buffer)) {
        conn.cmd_len = sizeof(conn.cmd_buffer) - 1;
    }
    memcpy(conn.cmd_buffer, req.buffer, conn.cmd_len);
    conn.cmd_buffer[conn.cmd_len] = '\0';

    payload_log("[CMD] %s", conn.cmd_buffer);
    g_last_activity = time(NULL);
    process_command(&conn);
    close_connection(&conn);
    return NULL;
}

static void *command_thread(void *arg) {
    atomic_fetch_add(&g_command_threads_active, 1);
    void *ret = command_thread_impl(arg);
    int prev = atomic_fetch_sub(&g_command_threads_active, 1);
    if (prev <= 0) {
        atomic_store(&g_command_threads_active, 0);
    }
    return ret;
}

static void set_socket_buffers(int sock) {
    // Large socket buffers for high throughput on GigE networks
    int rcv_buf = SOCKET_RCVBUF_SIZE;
    int snd_buf = SOCKET_SNDBUF_SIZE;
    setsockopt(sock, SOL_SOCKET, SO_RCVBUF, &rcv_buf, sizeof(rcv_buf));
    setsockopt(sock, SOL_SOCKET, SO_SNDBUF, &snd_buf, sizeof(snd_buf));

    struct timeval snd_to;
    snd_to.tv_sec = RECV_TIMEOUT_SEC;  // Keep in sync with transfer recv timeout for long-running ops.
    snd_to.tv_usec = 0;
    setsockopt(sock, SOL_SOCKET, SO_SNDTIMEO, &snd_to, sizeof(snd_to));

    struct timeval rcv_to;
    rcv_to.tv_sec = RECV_TIMEOUT_SEC;
    rcv_to.tv_usec = 0;
    setsockopt(sock, SOL_SOCKET, SO_RCVTIMEO, &rcv_to, sizeof(rcv_to));

    // Enable TCP_NODELAY for lower latency on small packets
    int nodelay = 1;
    setsockopt(sock, IPPROTO_TCP, TCP_NODELAY, &nodelay, sizeof(nodelay));

    // Keepalive to detect dead peers
    int keepalive = 1;
    setsockopt(sock, SOL_SOCKET, SO_KEEPALIVE, &keepalive, sizeof(keepalive));

#ifdef TCP_KEEPIDLE
    int keepidle = 10;
    setsockopt(sock, IPPROTO_TCP, TCP_KEEPIDLE, &keepidle, sizeof(keepidle));
#endif
#ifdef TCP_KEEPINTVL
    int keepintvl = 5;
    setsockopt(sock, IPPROTO_TCP, TCP_KEEPINTVL, &keepintvl, sizeof(keepintvl));
#endif
#ifdef TCP_KEEPCNT
    int keepcnt = 3;
    setsockopt(sock, IPPROTO_TCP, TCP_KEEPCNT, &keepcnt, sizeof(keepcnt));
#endif

#ifdef TCP_MAXSEG
    int maxseg = 1460;
    setsockopt(sock, IPPROTO_TCP, TCP_MAXSEG, &maxseg, sizeof(maxseg));
#endif

#ifdef SO_NOSIGPIPE
    int no_sigpipe = 1;
    setsockopt(sock, SOL_SOCKET, SO_NOSIGPIPE, &no_sigpipe, sizeof(no_sigpipe));
#endif
}

#if defined(PS5UPLOAD_DEBUGGER_BUILD)
#define DEBUGGER_LOG_STREAM_PORT (SERVER_PORT + 1)
#define DEBUGGER_LOG_MAX_CLIENTS 4

static pthread_mutex_t g_debugger_log_clients_mutex = PTHREAD_MUTEX_INITIALIZER;
static int g_debugger_log_clients[DEBUGGER_LOG_MAX_CLIENTS] = { -1, -1, -1, -1 };

static void debugger_log_client_remove_locked(int idx) {
    if (idx < 0 || idx >= DEBUGGER_LOG_MAX_CLIENTS) {
        return;
    }
    if (g_debugger_log_clients[idx] >= 0) {
        close(g_debugger_log_clients[idx]);
        g_debugger_log_clients[idx] = -1;
    }
}

static void debugger_log_stream_broadcast(const char *line) {
    if (!line || line[0] == '\0') {
        return;
    }
    char out[576];
    int len = snprintf(out, sizeof(out), "%s\n", line);
    if (len <= 0) {
        return;
    }
    size_t total = (size_t)len;
    pthread_mutex_lock(&g_debugger_log_clients_mutex);
    for (int i = 0; i < DEBUGGER_LOG_MAX_CLIENTS; i++) {
        int client = g_debugger_log_clients[i];
        if (client < 0) {
            continue;
        }
        ssize_t sent = send(client, out, total, 0);
        if (sent < 0 || (size_t)sent != total) {
            debugger_log_client_remove_locked(i);
        }
    }
    pthread_mutex_unlock(&g_debugger_log_clients_mutex);
}

static void *debugger_log_stream_thread(void *arg) {
    (void)arg;
    int stream_sock = create_server_socket(DEBUGGER_LOG_STREAM_PORT);
    if (stream_sock < 0) {
        payload_log("[DEBUGGER] Failed to open log stream port %d", DEBUGGER_LOG_STREAM_PORT);
        return NULL;
    }
    payload_log("[DEBUGGER] Log stream listening on port %d", DEBUGGER_LOG_STREAM_PORT);

    while (1) {
        struct sockaddr_in client_addr = {0};
        socklen_t addr_len = sizeof(client_addr);
        int client = accept(stream_sock, (struct sockaddr *)&client_addr, &addr_len);
        if (client < 0) {
            if (errno == EINTR) {
                continue;
            }
            payload_log("[DEBUGGER] Log stream accept error: %s", strerror(errno));
            usleep(200000);
            continue;
        }

#ifdef SO_NOSIGPIPE
        int no_sigpipe = 1;
        setsockopt(client, SOL_SOCKET, SO_NOSIGPIPE, &no_sigpipe, sizeof(no_sigpipe));
#endif
        struct timeval send_to = {0};
        send_to.tv_sec = 0;
        send_to.tv_usec = 200000;
        setsockopt(client, SOL_SOCKET, SO_SNDTIMEO, &send_to, sizeof(send_to));

        int slot = -1;
        pthread_mutex_lock(&g_debugger_log_clients_mutex);
        for (int i = 0; i < DEBUGGER_LOG_MAX_CLIENTS; i++) {
            if (g_debugger_log_clients[i] < 0) {
                slot = i;
                g_debugger_log_clients[i] = client;
                break;
            }
        }
        pthread_mutex_unlock(&g_debugger_log_clients_mutex);

        char ipstr[INET_ADDRSTRLEN] = {0};
        inet_ntop(AF_INET, &client_addr.sin_addr, ipstr, sizeof(ipstr));
        if (slot < 0) {
            payload_log("[DEBUGGER] Log stream full, rejecting %s:%d", ipstr, ntohs(client_addr.sin_port));
            close(client);
            continue;
        }
        payload_log("[DEBUGGER] Log stream client connected %s:%d", ipstr, ntohs(client_addr.sin_port));
    }
}
#endif

static int read_command_line(int sock, char *out, size_t cap, size_t *out_len) {
    if (!out || cap == 0) return -1;
    size_t len = 0;
    while (len + 1 < cap) {
        char ch = 0;
        ssize_t n = recv(sock, &ch, 1, 0);
        if (n < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (n == 0) {
            break;
        }
        out[len++] = ch;
        if (ch == '\n') {
            break;
        }
    }
    if (out_len) *out_len = len;
    out[len] = '\0';
    return (len > 0) ? 0 : -1;
}

static int is_comm_command(const char *cmd) {
    if (!cmd) return 0;
    if (strncmp(cmd, "RESET", 5) == 0) return 1;
    if (strncmp(cmd, "MAINTENANCE", 11) == 0) return 1;
    if (strncmp(cmd, "DELETE_ASYNC ", 13) == 0) return 1;
    if (strncmp(cmd, "PAYLOAD_STATUS", 14) == 0) return 1;
    if (strncmp(cmd, "STATUS", 6) == 0) return 1;
    if (strncmp(cmd, "QUEUE_", 6) == 0) return 1;
    if (strncmp(cmd, "VERSION", 7) == 0) return 1;
    if (strncmp(cmd, "GET_SPACE ", 10) == 0) return 1;
    if (strncmp(cmd, "LIST_STORAGE", 12) == 0) return 1;
    if (strncmp(cmd, "SYNC_INFO", 9) == 0) return 1;
    if (strncmp(cmd, "UPLOAD_QUEUE_", 13) == 0) return 1;
    if (strncmp(cmd, "HISTORY_", 8) == 0) return 1;
    if (strncmp(cmd, "CLEAR_TMP", 9) == 0) return 1;
    if (strncmp(cmd, "QUEUE_CLEAR_FAILED", 18) == 0) return 1;
    return 0;
}

static int is_noisy_comm_command(const char *cmd) {
    if (!cmd) return 0;
    if (strncmp(cmd, "PAYLOAD_STATUS", 14) == 0) return 1;
    if (strncmp(cmd, "STATUS", 6) == 0) return 1;
    if (strncmp(cmd, "SYNC_INFO", 9) == 0) return 1;
    if (strncmp(cmd, "UPLOAD_QUEUE_SYNC ", 18) == 0) return 1;
    if (strncmp(cmd, "UPLOAD_QUEUE_GET", 16) == 0) return 1;
    if (strncmp(cmd, "HISTORY_SYNC ", 13) == 0) return 1;
    if (strncmp(cmd, "HISTORY_GET", 11) == 0) return 1;
    return 0;
}

static void handle_maintenance(int client_sock) {
    if (transfer_is_active() || extract_queue_is_running() || extract_queue_has_pending()) {
        const char *busy = "BUSY\n";
        send(client_sock, busy, strlen(busy), 0);
        return;
    }
    int cleaned = transfer_idle_cleanup();
    int cleared = 0;
    int errors = 0;
    char last_err[256] = {0};
    clear_tmp_all(&cleared, &errors, last_err, sizeof(last_err));
    payload_log_rotate();

    char msg[256];
    snprintf(
        msg,
        sizeof(msg),
        "OK cleaned=%d tmp_cleared=%d tmp_errors=%d logs_rotated=1\n",
        cleaned,
        cleared,
        errors
    );
    send(client_sock, msg, strlen(msg), 0);
}

static void enqueue_comm_request(const struct CommandRequest *req) {
    pthread_mutex_lock(&g_comm_queue.mutex);
    if (g_comm_queue.count >= (int)(sizeof(g_comm_queue.items) / sizeof(g_comm_queue.items[0]))) {
        pthread_mutex_unlock(&g_comm_queue.mutex);
        struct ClientConnection conn;
        memset(&conn, 0, sizeof(conn));
        conn.sock = req->sock;
        const char *err = "ERROR: Server busy\n";
        send(conn.sock, err, strlen(err), 0);
        close_connection(&conn);
        return;
    }
    g_comm_queue.items[g_comm_queue.tail] = *req;
    g_comm_queue.tail = (g_comm_queue.tail + 1) % (int)(sizeof(g_comm_queue.items) / sizeof(g_comm_queue.items[0]));
    g_comm_queue.count++;
    pthread_cond_signal(&g_comm_queue.cond);
    pthread_mutex_unlock(&g_comm_queue.mutex);
}

static void *comm_thread(void *arg) {
    (void)arg;
    while (1) {
        pthread_mutex_lock(&g_comm_queue.mutex);
        while (g_comm_queue.count == 0) {
            pthread_cond_wait(&g_comm_queue.cond, &g_comm_queue.mutex);
        }
        struct CommandRequest req = g_comm_queue.items[g_comm_queue.head];
        g_comm_queue.head = (g_comm_queue.head + 1) % (int)(sizeof(g_comm_queue.items) / sizeof(g_comm_queue.items[0]));
        g_comm_queue.count--;
        pthread_mutex_unlock(&g_comm_queue.mutex);
        struct ClientConnection conn;
        memset(&conn, 0, sizeof(conn));
        conn.sock = req.sock;
        conn.addr = req.addr;
        conn.mode = CONN_CMD;
        conn.cmd_len = req.len;
        if (conn.cmd_len >= sizeof(conn.cmd_buffer)) {
            conn.cmd_len = sizeof(conn.cmd_buffer) - 1;
        }
        memcpy(conn.cmd_buffer, req.buffer, conn.cmd_len);
        conn.cmd_buffer[conn.cmd_len] = '\0';
        process_command(&conn);
        close_connection(&conn);
    }
    return NULL;
}

__attribute__((unused)) static void *comm_worker_thread(void *arg) {
    struct CommandRequest *req = (struct CommandRequest *)arg;
    if (!req) return NULL;
    struct ClientConnection conn;
    memset(&conn, 0, sizeof(conn));
    conn.sock = req->sock;
    conn.addr = req->addr;
    conn.mode = CONN_CMD;
    conn.cmd_len = req->len;
    if (conn.cmd_len >= sizeof(conn.cmd_buffer)) {
        conn.cmd_len = sizeof(conn.cmd_buffer) - 1;
    }
    memcpy(conn.cmd_buffer, req->buffer, conn.cmd_len);
    conn.cmd_buffer[conn.cmd_len] = '\0';
    free(req);
    g_last_activity = time(NULL);
    process_command(&conn);
    close_connection(&conn);
    return NULL;
}
static void process_command(struct ClientConnection *conn) {
    conn->cmd_buffer[conn->cmd_len] = '\0';

#if DEBUG_LOG
    printf("Received command: %s\n", conn->cmd_buffer);
#endif

    if (strncmp(conn->cmd_buffer, "SHUTDOWN", 8) == 0) {
        if (!is_localhost(&conn->addr)) {
            const char *error = "ERROR: Unauthorized\n";
            send(conn->sock, error, strlen(error), 0);
            close_connection(conn);
        } else {
            int force = (strncmp(conn->cmd_buffer, "SHUTDOWN_FORCE", 14) == 0);
            if (!force &&
                (transfer_is_active() ||
                 extract_queue_is_running() ||
                 atomic_load(&g_binary_sessions_active) > 0)) {
                const char *busy = "BUSY\n";
                send(conn->sock, busy, strlen(busy), 0);
                close_connection(conn);
                payload_log("[SHUTDOWN] Rejected while busy (transfer=%d extract=%d bin_sessions=%d)",
                            transfer_is_active(),
                            extract_queue_is_running(),
                            atomic_load(&g_binary_sessions_active));
                return;
            }
            const char *ok = "OK\n";
            send(conn->sock, ok, strlen(ok), 0);
            close_connection(conn);
            notify_info("PS5 Upload Server", "Shutting down...");
            transfer_cleanup();
            unlink(g_pid_file);
            exit(EXIT_SUCCESS);
        }
        return;
    }

    if (strncmp(conn->cmd_buffer, "LIST_STORAGE", 12) == 0) {
        handle_list_storage(conn->sock);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "CLEAR_TMP", 9) == 0) {
        handle_clear_tmp(conn->sock);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "MAINTENANCE", 11) == 0) {
        handle_maintenance(conn->sock);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "QUEUE_CLEAR_FAILED", 18) == 0) {
        handle_queue_clear_failed(conn->sock);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "LIST_DIR ", 9) == 0) {
        payload_set_crash_context("LIST_DIR", conn->cmd_buffer + 9, NULL);
        handle_list_dir(conn->sock, conn->cmd_buffer + 9);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "LIST_DIR_RECURSIVE ", 19) == 0) {
        payload_set_crash_context("LIST_DIR_RECURSIVE", conn->cmd_buffer + 19, NULL);
        handle_list_dir_recursive(conn->sock, conn->cmd_buffer + 19);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "TEST_WRITE ", 11) == 0) {
        handle_test_write(conn->sock, conn->cmd_buffer + 11);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "CREATE_PATH ", 12) == 0) {
        handle_create_path(conn->sock, conn->cmd_buffer + 12);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "CHECK_DIR ", 10) == 0) {
        handle_check_dir(conn->sock, conn->cmd_buffer + 10);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "DELETE ", 7) == 0) {
        handle_delete_path(conn->sock, conn->cmd_buffer + 7);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "DELETE_ASYNC ", 13) == 0) {
        handle_delete_path_async(conn->sock, conn->cmd_buffer + 13);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "PROBE_RAR ", 10) == 0) {
        handle_probe_rar(conn->sock, conn->cmd_buffer + 10);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "EXTRACT_ARCHIVE ", 16) == 0) {
        if (handle_extract_archive(conn->sock, conn->cmd_buffer + 16) != 0) {
            // Socket ownership transferred to worker thread
            conn->sock = -1;
        }
        return;
    }
    if (strncmp(conn->cmd_buffer, "MOVE ", 5) == 0) {
        handle_move_path(conn->sock, conn->cmd_buffer + 5);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "COPY ", 5) == 0) {
        handle_copy_path(conn->sock, conn->cmd_buffer + 5);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "CHMOD777 ", 9) == 0) {
        handle_chmod_777(conn->sock, conn->cmd_buffer + 9);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "DOWNLOAD_V2 ", 12) == 0) {
        handle_download_v2(conn->sock, conn->cmd_buffer + 12);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "DOWNLOAD_RAW_FROM ", 18) == 0) {
        payload_set_crash_context("DOWNLOAD_RAW_FROM", conn->cmd_buffer + 18, NULL);
        handle_download_raw_from(conn->sock, conn->cmd_buffer + 18);
        // Handler owns/closed the socket.
        conn->sock = -1;
        return;
    }
    if (strncmp(conn->cmd_buffer, "DOWNLOAD_RAW_RANGE ", 19) == 0) {
        payload_set_crash_context("DOWNLOAD_RAW_RANGE", conn->cmd_buffer + 19, NULL);
        handle_download_raw_range(conn->sock, conn->cmd_buffer + 19);
        // Handler owns/closed the socket.
        conn->sock = -1;
        return;
    }
    if (strncmp(conn->cmd_buffer, "DOWNLOAD_RAW ", 13) == 0) {
        payload_set_crash_context("DOWNLOAD_RAW", conn->cmd_buffer + 13, NULL);
        handle_download_raw(conn->sock, conn->cmd_buffer + 13);
        // Handler owns/closed the socket.
        conn->sock = -1;
        return;
    }
    if (strncmp(conn->cmd_buffer, "DOWNLOAD ", 9) == 0) {
        handle_download_file(conn->sock, conn->cmd_buffer + 9);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "DOWNLOAD_DIR ", 13) == 0) {
        handle_download_dir(conn->sock, conn->cmd_buffer + 13);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "HASH_FILE ", 10) == 0) {
        handle_hash_file(conn->sock, conn->cmd_buffer + 10);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "VERSION", 7) == 0) {
        handle_version(conn->sock);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "GET_SPACE ", 10) == 0) {
        handle_get_space(conn->sock, conn->cmd_buffer + 10);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "PAYLOAD_STATUS", 14) == 0) {
        handle_payload_status(conn->sock);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "RESET", 5) == 0) {
        handle_payload_reset(conn->sock);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "QUEUE_EXTRACT ", 14) == 0) {
        handle_queue_extract(conn->sock, conn->cmd_buffer + 14);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "QUEUE_CANCEL ", 13) == 0) {
        handle_queue_cancel(conn->sock, conn->cmd_buffer + 13);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "QUEUE_CLEAR_ALL", 15) == 0) {
        handle_queue_clear_all(conn->sock);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "QUEUE_CLEAR", 11) == 0) {
        handle_queue_clear(conn->sock);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "QUEUE_REORDER ", 14) == 0) {
        handle_queue_reorder(conn->sock, conn->cmd_buffer + 14);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "QUEUE_PROCESS", 13) == 0) {
        handle_queue_process(conn->sock);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "QUEUE_PAUSE ", 12) == 0) {
        handle_queue_pause(conn->sock, conn->cmd_buffer + 12);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "QUEUE_RETRY ", 12) == 0) {
        handle_queue_retry(conn->sock, conn->cmd_buffer + 12);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "QUEUE_REMOVE ", 13) == 0) {
        handle_queue_remove(conn->sock, conn->cmd_buffer + 13);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "SYNC_INFO", 9) == 0) {
        handle_sync_info(conn->sock);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "UPLOAD_QUEUE_SYNC ", 18) == 0) {
        handle_upload_queue_sync(conn->sock, conn->cmd_buffer + 18);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "UPLOAD_QUEUE_GET", 16) == 0) {
        handle_upload_queue_get(conn->sock);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "HISTORY_SYNC ", 13) == 0) {
        handle_history_sync(conn->sock, conn->cmd_buffer + 13);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "HISTORY_GET", 11) == 0) {
        handle_history_get(conn->sock);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "UPLOAD_RAR_SAFE ", 16) == 0) {
        handle_upload_rar(conn->sock, conn->cmd_buffer + 16, UNRAR_MODE_TURBO);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "UPLOAD_RAR_TURBO ", 17) == 0) {
        handle_upload_rar(conn->sock, conn->cmd_buffer + 17, UNRAR_MODE_TURBO);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "UPLOAD_RAR ", 11) == 0) {
        handle_upload_rar(conn->sock, conn->cmd_buffer + 11, UNRAR_MODE_TURBO);
        close_connection(conn);
        return;
    }

    const char *error = "ERROR: Unknown command\n";
    send(conn->sock, error, strlen(error), 0);
    close_connection(conn);
}

int main(void) {
    // Register exit handler to log clean exits
    atexit(exit_handler);

    // Signal handlers
    signal(SIGPIPE, SIG_IGN);
    signal(SIGSEGV, crash_handler);
    signal(SIGABRT, crash_handler);
    signal(SIGBUS, crash_handler);
    signal(SIGILL, crash_handler);
    signal(SIGFPE, crash_handler);
    signal(SIGTERM, crash_handler);  // Log when killed
    printf("╔════════════════════════════════════════╗\n");
    printf("║     PS5 Upload Server v%s      ║\n", PS5_UPLOAD_VERSION);
    printf("║                                        ║\n");
    printf("║         Author: PhantomPtr            ║\n");
    printf("║   Fast game transfer over LAN         ║\n");
    printf("║         Port: %d                      ║\n", SERVER_PORT);
    printf("╚════════════════════════════════════════╝\n");
    printf("\n");

    // Create logging directory
    printf("[INIT] Creating log directories...\n");
    
    pid_t pid = getpid();
    printf("[INIT] Applying safe startup profile...\n");

#if ENABLE_AGGRESSIVE_PRIV_ESC
    printf("[INIT] Aggressive privilege escalation enabled.\n");
    kernel_set_proc_rootdir(pid, kernel_get_root_vnode());
    kernel_set_proc_jaildir(pid, 0);
    kernel_set_ucred_uid(pid, 0);
    kernel_set_ucred_ruid(pid, 0);
    kernel_set_ucred_svuid(pid, 0);
    kernel_set_ucred_rgid(pid, 0);
    kernel_set_ucred_svgid(pid, 0);
    kernel_set_ucred_caps(pid, g_full_caps);
    kernel_set_ucred_authid(pid, AUTHID_SHELLCORE);
#endif

    // Keep only low-risk tuning enabled by default.
    if (sys_budget_set(0) < 0) {
        printf("[INIT] Warning: sys_budget_set failed (may be unsupported)\n");
    }

    // Optional: lock current/future pages (disabled by default for system stability).
#if ENABLE_MLOCKALL
#ifdef MCL_CURRENT
    if (mlockall(MCL_CURRENT | MCL_FUTURE) < 0) {
        printf("[INIT] Warning: mlockall failed (non-critical)\n");
    }
#endif
#endif

    // Optional: real-time scheduling (disabled by default to avoid system/UI starvation).
#if ENABLE_RT_SCHED
    struct sched_param sp;
    sp.sched_priority = sched_get_priority_max(SCHED_RR);
    if (sp.sched_priority > 0) {
        if (sched_setscheduler(0, SCHED_RR, &sp) < 0) {
            printf("[INIT] Warning: sched_setscheduler failed (non-critical)\n");
        }
    }
#endif

    printf("[INIT] Startup profile applied.\n");
    
    mkdir("/data/ps5upload", 0777);
    mkdir("/data/ps5upload/logs", 0777);
    mkdir("/data/ps5upload/requests", 0777);
    if (access(g_heartbeat_file, F_OK) == 0) {
        int fd_prev = open(g_heartbeat_file, O_RDONLY);
        if (fd_prev >= 0) {
            char prev[512];
            ssize_t n = read(fd_prev, prev, sizeof(prev) - 1);
            close(fd_prev);
            if (n > 0) {
                prev[n] = '\0';
                payload_log("[FORENSICS] Previous heartbeat before restart: %s", prev);
            }
        }
    }
    mark_run_state("start", "main");
    unlink(g_kill_file);
    payload_log_rotate();
    write_pid_file();
    g_last_activity = time(NULL);
    payload_log("[INIT] Payload start v%s", PS5_UPLOAD_VERSION);
    payload_log("[INIT] pid=%d port=%d", (int)pid, SERVER_PORT);
    payload_log("[INIT] idle_timeout_sec=%d (0=disabled)", g_idle_timeout_sec);

    if (pthread_create_detached_with_stack(kill_watch_thread, NULL) == 0) {
        // detached
    } else {
        payload_log("[INIT] Failed to create kill watch thread");
    }
    if (pthread_create_detached_with_stack(idle_watch_thread, NULL) == 0) {
        // detached
    } else {
        payload_log("[INIT] Failed to create idle watch thread");
    }
    if (pthread_create_detached_with_stack(heartbeat_thread, NULL) == 0) {
        // detached
    } else {
        payload_log("[INIT] Failed to create heartbeat thread");
    }
    
    // Temp cleanup handled per-upload based on destination root.

    // Initialize extraction queue
    extract_queue_init();

    printf("[INIT] Log directory: /data/ps5upload/logs/\n");
    printf("[INIT] Request directory: /data/ps5upload/requests/\n");
    printf("[INIT] Extraction queue: initialized\n");

    int server_sock = create_server_socket(SERVER_PORT);
    if(server_sock < 0) {
        if(errno == EADDRINUSE) {
            printf("Port %d in use, attempting to stop existing server...\n", SERVER_PORT);
            if(request_shutdown() == 0) {
                usleep(200000);
                server_sock = create_server_socket(SERVER_PORT);
            }
        }
        if(server_sock < 0) {
            fprintf(stderr, "Failed to create server socket\n");
            return EXIT_FAILURE;
        }
    }

    char ip_buf[INET_ADDRSTRLEN] = {0};
    get_local_ip(ip_buf, sizeof(ip_buf));
    if (ip_buf[0] != '\0') {
        printf("Server listening on %s:%d\n", ip_buf, SERVER_PORT);
        char notify_msg[128];
#if defined(PS5UPLOAD_DEBUGGER_BUILD)
        snprintf(notify_msg, sizeof(notify_msg), "Debug v%s Ready on %s:%d", PS5_UPLOAD_VERSION, ip_buf, SERVER_PORT);
        notify_info("PS5 Upload Server Debug (PhantomPtr)", notify_msg);
#else
        snprintf(notify_msg, sizeof(notify_msg), "v%s Ready on %s:%d", PS5_UPLOAD_VERSION, ip_buf, SERVER_PORT);
        notify_info("PS5 Upload Server (PhantomPtr)", notify_msg);
#endif
    } else {
        printf("Server listening on port %d\n", SERVER_PORT);
        char notify_msg[128];
#if defined(PS5UPLOAD_DEBUGGER_BUILD)
        snprintf(notify_msg, sizeof(notify_msg), "Debug v%s Ready on port %s", PS5_UPLOAD_VERSION, SERVER_PORT_STR);
        notify_info("PS5 Upload Server Debug (PhantomPtr)", notify_msg);
#else
        snprintf(notify_msg, sizeof(notify_msg), "v%s Ready on port %s", PS5_UPLOAD_VERSION, SERVER_PORT_STR);
        notify_info("PS5 Upload Server (PhantomPtr)", notify_msg);
#endif
    }

    if (pthread_create_detached_with_stack(comm_thread, NULL) == 0) {
        // detached
    } else {
        printf("[INIT] Failed to create communication thread\n");
    }

#if defined(PS5UPLOAD_DEBUGGER_BUILD)
    if (pthread_create_detached_with_stack(debugger_log_stream_thread, NULL) == 0) {
        // detached
    } else {
        payload_log("[INIT] Failed to create debugger log stream thread");
    }
#endif

    while (1) {
        struct sockaddr_in client_addr = {0};
        socklen_t addr_len = sizeof(client_addr);
        int client = accept(server_sock, (struct sockaddr*)&client_addr, &addr_len);
        if (client < 0) {
            if (errno == EINTR) {
                continue;
            }
            payload_log("[ACCEPT] error: %s", strerror(errno));
            usleep(200000);
            continue;
        }

        set_socket_buffers(client);
        if (set_blocking(client) != 0) {
            close(client);
            continue;
        }
        char ipstr[INET_ADDRSTRLEN] = {0};
        inet_ntop(AF_INET, &client_addr.sin_addr, ipstr, sizeof(ipstr));
#if !defined(PS5UPLOAD_DEBUGGER_BUILD)
        payload_log("[CONN] accept %s:%d", ipstr, ntohs(client_addr.sin_port));
#endif

        if (atomic_load(&g_command_threads_active) >= MAX_COMMAND_DISPATCH_THREADS) {
            payload_log("[CONN] busy: dispatch thread cap reached (%d)", MAX_COMMAND_DISPATCH_THREADS);
            const char *busy = "ERROR: Server busy\n";
            send(client, busy, strlen(busy), 0);
            close(client);
            continue;
        }

        struct DispatchArgs *args = malloc(sizeof(*args));
        if (!args) {
            close(client);
            continue;
        }
        args->sock = client;
        args->addr = client_addr;

        if (pthread_create_detached_with_stack(command_thread, args) == 0) {
            // detached
        } else {
            close(client);
            free(args);
        }
    }

    close(server_sock);
    return EXIT_SUCCESS;
}
