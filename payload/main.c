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
#include <signal.h>
#include <stdarg.h>
#include <pthread.h>
#include <fcntl.h>
#include <limits.h>
#include <ifaddrs.h>
#include <time.h>

#include <ps5/kernel.h>

#include "config.h"
#include "storage.h"
#include "protocol.h"
#include "extract.h"
#include "notify.h"
#include "transfer.h"
#include "unrar_handler.h"
#include "extract_queue.h"


static pthread_mutex_t g_log_mutex = PTHREAD_MUTEX_INITIALIZER;

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

static void payload_log(const char *fmt, ...) {
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
    pthread_mutex_unlock(&g_log_mutex);
}

static void crash_handler(int sig) {
    // Minimal, signal-safe logging to avoid deadlocks on mutex.
    int fd = open("/data/ps5upload/payload_crash.log", O_WRONLY | O_CREAT | O_APPEND, 0666);
    if (fd >= 0) {
        char buf[128];
        int len = snprintf(buf, sizeof(buf), "[CRASH] signal=%d\n", sig);
        if (len > 0) {
            write(fd, buf, (size_t)len);
        }
        close(fd);
    }
    _exit(1);
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

    // Increased to 2MB to improve throughput
    int buf_size = 2 * 1024 * 1024; // 2MB
    setsockopt(sock, SOL_SOCKET, SO_RCVBUF, &buf_size, sizeof(buf_size));
    setsockopt(sock, SOL_SOCKET, SO_SNDBUF, &buf_size, sizeof(buf_size));
    
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

static int read_command_line(int sock, char *out, size_t cap, size_t *out_len);
static int is_comm_command(const char *cmd);
static void enqueue_comm_request(const struct CommandRequest *req);
static void *comm_worker_thread(void *arg);
static int parse_first_token(const char *src, char *out, size_t out_cap, const char **rest);

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

static int parse_first_token(const char *src, char *out, size_t out_cap, const char **rest) {
    if (!src || !out || out_cap == 0) {
        return -1;
    }
    while (*src == ' ') {
        src++;
    }
    if (*src == '\0') {
        return -1;
    }
    const char *end = src;
    while (*end != '\0' && *end != ' ') {
        end++;
    }
    size_t len = (size_t)(end - src);
    if (len == 0 || len >= out_cap) {
        return -1;
    }
    memcpy(out, src, len);
    out[len] = '\0';
    if (rest) {
        while (*end == ' ') {
            end++;
        }
        *rest = end;
    }
    return 0;
}

static void *command_thread(void *arg) {
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

    if (read_command_line(req.sock, req.buffer, sizeof(req.buffer), &req.len) != 0) {
        payload_log("[CONN] Failed to read command line");
        close(req.sock);
        return NULL;
    }

    if (is_comm_command(req.buffer)) {
        payload_log("[COMM] %s", req.buffer);
        enqueue_comm_request(&req);
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
    process_command(&conn);
    close_connection(&conn);
    return NULL;
}

static void set_socket_buffers(int sock) {
    // Increased to 2MB to improve throughput
    int buf_size = 2 * 1024 * 1024; // 2MB
    setsockopt(sock, SOL_SOCKET, SO_RCVBUF, &buf_size, sizeof(buf_size));
    setsockopt(sock, SOL_SOCKET, SO_SNDBUF, &buf_size, sizeof(buf_size));

    // Enable TCP_NODELAY for lower latency
    int nodelay = 1;
    setsockopt(sock, IPPROTO_TCP, TCP_NODELAY, &nodelay, sizeof(nodelay));

    // Keepalive to detect dead peers
    int keepalive = 1;
    setsockopt(sock, SOL_SOCKET, SO_KEEPALIVE, &keepalive, sizeof(keepalive));

#ifdef SO_NOSIGPIPE
    int no_sigpipe = 1;
    setsockopt(sock, SOL_SOCKET, SO_NOSIGPIPE, &no_sigpipe, sizeof(no_sigpipe));
#endif
}

struct LegacyUploadArgs {
    int sock;
    char args[CMD_BUFFER_SIZE];
};

struct UploadWorkerArgs {
    int sock;
    char dest_path[PATH_MAX];
    int use_temp;
};

static void *upload_worker_thread(void *arg) {
    struct UploadWorkerArgs *args = (struct UploadWorkerArgs *)arg;
    if (!args) {
        return NULL;
    }
    payload_log("[UPLOAD] worker start dest=%s temp=%d", args->dest_path, args->use_temp);
    if (set_blocking(args->sock) != 0) {
        payload_log("[UPLOAD] set_blocking failed");
        close(args->sock);
        free(args);
        return NULL;
    }
    struct timeval tv;
    tv.tv_sec = 1;
    tv.tv_usec = 0;
    setsockopt(args->sock, SOL_SOCKET, SO_RCVTIMEO, (const char *)&tv, sizeof(tv));
    UploadSession *session = upload_session_create(args->dest_path, args->use_temp);
    if (!session) {
        const char *err = "ERROR: Upload init failed\n";
        send(args->sock, err, strlen(err), 0);
        payload_log("[UPLOAD] session create failed");
        close(args->sock);
        free(args);
        return NULL;
    }

    uint8_t *buffer = malloc(64 * 1024);
    if (!buffer) {
        const char *err = "ERROR: Upload init failed\n";
        send(args->sock, err, strlen(err), 0);
        payload_log("[UPLOAD] buffer alloc failed");
        close(args->sock);
        free(args);
        return NULL;
    }
    int done = 0;
    int error = 0;
    while (!done && !error) {
        if (transfer_abort_requested()) {
            payload_log("[UPLOAD] abort requested");
            error = 1;
            break;
        }
        if (upload_session_backpressure(session)) {
            upload_session_feed(session, NULL, 0, &done, &error);
            if (error) break;
            if (upload_session_backpressure(session)) {
                usleep(1000);
                continue;
            }
        }
        ssize_t n = recv(args->sock, buffer, 64 * 1024, 0);
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            if (errno == EAGAIN || errno == EWOULDBLOCK) {
                continue;
            }
            payload_log("[UPLOAD] recv error: %s", strerror(errno));
            error = 1;
            break;
        }
        if (n == 0) {
            payload_log("[UPLOAD] recv EOF");
            error = 1;
            break;
        }
        upload_session_feed(session, buffer, (size_t)n, &done, &error);
    }

    if (error) {
        free(buffer);
        upload_session_destroy(session);
        const char *err = "ERROR: Upload failed\n";
        send(args->sock, err, strlen(err), 0);
        payload_log("[UPLOAD] failed");
        close(args->sock);
        free(args);
        return NULL;
    }

    int files = 0;
    long long bytes = 0;
    int finalize_ok = (upload_session_finalize(session) == 0);
    upload_session_stats(session, &files, &bytes);
    upload_session_destroy(session);
    free(buffer);

    if (!finalize_ok) {
        const char *err = "ERROR: Upload finalize failed\n";
        send(args->sock, err, strlen(err), 0);
        payload_log("[UPLOAD] finalize failed");
        close(args->sock);
        free(args);
        return NULL;
    }

    char response[256];
    snprintf(response, sizeof(response), "SUCCESS %d %lld\n", files, bytes);
    send(args->sock, response, strlen(response), 0);
    payload_log("[UPLOAD] complete files=%d bytes=%lld", files, bytes);
    close(args->sock);
    free(args);
    return NULL;
}

static void *legacy_upload_thread(void *arg) {
    struct LegacyUploadArgs *args = (struct LegacyUploadArgs *)arg;
    if (!args) {
        return NULL;
    }
    handle_upload(args->sock, args->args);
    close(args->sock);
    free(args);
    return NULL;
}

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
        struct CommandRequest *heap_req = malloc(sizeof(*heap_req));
        if (!heap_req) {
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
            continue;
        }
        *heap_req = req;
        pthread_t tid;
        if (pthread_create(&tid, NULL, comm_worker_thread, heap_req) == 0) {
            pthread_detach(tid);
        } else {
            free(heap_req);
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
    }
    return NULL;
}

static void *comm_worker_thread(void *arg) {
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
            const char *ok = "OK\n";
            send(conn->sock, ok, strlen(ok), 0);
            close_connection(conn);
            notify_info("PS5 Upload Server", "Shutting down...");
            transfer_cleanup();
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
    if (strncmp(conn->cmd_buffer, "QUEUE_CLEAR_FAILED", 18) == 0) {
        handle_queue_clear_failed(conn->sock);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "LIST_DIR ", 9) == 0) {
        handle_list_dir(conn->sock, conn->cmd_buffer + 9);
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
        handle_extract_archive(conn->sock, conn->cmd_buffer + 16);
        close_connection(conn);
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
        handle_upload_rar(conn->sock, conn->cmd_buffer + 16, UNRAR_MODE_SAFE);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "UPLOAD_RAR_TURBO ", 17) == 0) {
        handle_upload_rar(conn->sock, conn->cmd_buffer + 17, UNRAR_MODE_TURBO);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "UPLOAD_RAR ", 11) == 0) {
        handle_upload_rar(conn->sock, conn->cmd_buffer + 11, UNRAR_MODE_FAST);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "UPLOAD_V2 ", 10) == 0) {
        char dest_path[PATH_MAX];
        char mode[16] = {0};
        const char *rest = NULL;
        if (parse_first_token(conn->cmd_buffer + 10, dest_path, sizeof(dest_path), &rest) != 0) {
            const char *error = "ERROR: Invalid UPLOAD_V2 format\n";
            send(conn->sock, error, strlen(error), 0);
            close_connection(conn);
            return;
        }
        if (rest && *rest) {
            strncpy(mode, rest, sizeof(mode) - 1);
            mode[sizeof(mode) - 1] = '\0';
            char *space = strchr(mode, ' ');
            if (space) {
                *space = '\0';
            }
        }
        if (!is_path_safe(dest_path)) {
            const char *error = "ERROR: Invalid path\n";
            send(conn->sock, error, strlen(error), 0);
            close_connection(conn);
            return;
        }
        int use_temp = 1;
        if (mode[0] && strcasecmp(mode, "DIRECT") == 0) {
            use_temp = 0;
        }
        payload_log("[UPLOAD] V2 start dest=%s temp=%d", dest_path, use_temp);
        const char *ready = "READY\n";
        send(conn->sock, ready, strlen(ready), 0);
        struct UploadWorkerArgs *args = malloc(sizeof(*args));
        if (!args) {
            const char *error = "ERROR: Upload init failed\n";
            send(conn->sock, error, strlen(error), 0);
            close_connection(conn);
            return;
        }
        args->sock = conn->sock;
        args->use_temp = use_temp;
        strncpy(args->dest_path, dest_path, sizeof(args->dest_path) - 1);
        args->dest_path[sizeof(args->dest_path) - 1] = '\0';

        pthread_t tid;
        if (pthread_create(&tid, NULL, upload_worker_thread, args) == 0) {
            pthread_detach(tid);
            conn->sock = -1;
            return;
        }

        free(args);
        const char *error = "ERROR: Upload init failed\n";
        send(conn->sock, error, strlen(error), 0);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "UPLOAD ", 7) == 0) {
        pthread_t tid;
        struct LegacyUploadArgs *args = malloc(sizeof(*args));
        if (!args) {
            const char *error = "ERROR: Legacy upload failed\n";
            send(conn->sock, error, strlen(error), 0);
            close_connection(conn);
            return;
        }
        args->sock = conn->sock;
        strncpy(args->args, conn->cmd_buffer + 7, sizeof(args->args) - 1);
        args->args[sizeof(args->args) - 1] = '\0';

        char dest_path[PATH_MAX];
        if (parse_first_token(args->args, dest_path, sizeof(dest_path), NULL) != 0) {
            const char *error = "ERROR: Invalid UPLOAD format\n";
            send(conn->sock, error, strlen(error), 0);
            close_connection(conn);
            free(args);
            return;
        }

        if (!is_path_safe(dest_path)) {
            const char *error = "ERROR: Invalid path\n";
            send(conn->sock, error, strlen(error), 0);
            close_connection(conn);
            free(args);
            return;
        }

        if (pthread_create(&tid, NULL, legacy_upload_thread, args) == 0) {
            pthread_detach(tid);
            conn->sock = -1;
            return;
        }

        free(args);
        const char *error = "ERROR: Legacy upload failed\n";
        send(conn->sock, error, strlen(error), 0);
        close_connection(conn);
        return;
    }

    const char *error = "ERROR: Unknown command\n";
    send(conn->sock, error, strlen(error), 0);
    close_connection(conn);
}

int main(void) {
    signal(SIGPIPE, SIG_IGN);
    signal(SIGSEGV, crash_handler);
    signal(SIGABRT, crash_handler);
    signal(SIGBUS, crash_handler);
    signal(SIGILL, crash_handler);
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
    
    // Use direct mkdir instead of system() for speed
    // Set root vnode once for the lifetime of the server to ensure full FS access.
    pid_t pid = getpid();
    kernel_set_proc_rootdir(pid, kernel_get_root_vnode());
    
    mkdir("/data/ps5upload", 0777);
    mkdir("/data/ps5upload/logs", 0777);
    mkdir("/data/ps5upload/requests", 0777);
    payload_log_rotate();
    payload_log("[INIT] Payload start v%s", PS5_UPLOAD_VERSION);
    payload_log("[INIT] pid=%d port=%d", (int)pid, SERVER_PORT);
    
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
        snprintf(notify_msg, sizeof(notify_msg), "v%s Ready on %s:%d", PS5_UPLOAD_VERSION, ip_buf, SERVER_PORT);
        notify_info("PS5 Upload Server (PhantomPtr)", notify_msg);
    } else {
        printf("Server listening on port %d\n", SERVER_PORT);
        char notify_msg[128];
        snprintf(notify_msg, sizeof(notify_msg), "v%s Ready on port %s", PS5_UPLOAD_VERSION, SERVER_PORT_STR);
        notify_info("PS5 Upload Server (PhantomPtr)", notify_msg);
    }

    pthread_t comm_tid;
    if (pthread_create(&comm_tid, NULL, comm_thread, NULL) == 0) {
        pthread_detach(comm_tid);
    } else {
        printf("[INIT] Failed to create communication thread\n");
    }

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
        payload_log("[CONN] accept %s:%d", ipstr, ntohs(client_addr.sin_port));

        struct DispatchArgs *args = malloc(sizeof(*args));
        if (!args) {
            close(client);
            continue;
        }
        args->sock = client;
        args->addr = client_addr;

        pthread_t tid;
        if (pthread_create(&tid, NULL, command_thread, args) == 0) {
            pthread_detach(tid);
        } else {
            close(client);
            free(args);
        }
    }

    close(server_sock);
    return EXIT_SUCCESS;
}
