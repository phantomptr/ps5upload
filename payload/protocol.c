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
#include <sys/stat.h>
#include <limits.h>
#include <errno.h>
#include <dirent.h>
#include <fcntl.h>
#include <sys/mount.h>
#include <time.h>
#include <sys/time.h>
#include <ctype.h>
#include <strings.h>
#include <pthread.h>

#include <ps5/kernel.h>

#include "protocol.h"
#include "extract.h"
#include "notify.h"
#include "config.h"
#include "transfer.h"
#include "protocol_defs.h"
#include "lz4.h"
#include "zstd.h"
#include "LzmaLib.h"
#include "sha256.h"
#include "third_party/unrar/unrar_wrapper.h"
#include "extract_queue.h"

void payload_set_crash_context(const char *op, const char *path, const char *path2);

#define DOWNLOAD_PACK_BUFFER_SIZE (4 * 1024 * 1024)
#define PROBE_RAR_MAX_LINE 128

static pthread_mutex_t g_payload_file_mutex = PTHREAD_MUTEX_INITIALIZER;
#define UPLOAD_QUEUE_FILE "/data/ps5upload/upload_queue.json"
#define HISTORY_FILE "/data/ps5upload/history.json"

static int send_all(int sock, const void *buf, size_t len);
static int recv_exact(int sock, void *buf, size_t len);
static time_t get_file_mtime(const char *path);
static int write_text_file(const char *path, const char *data, size_t len);
static char *read_text_file(const char *path, size_t *out_len);
static int parse_quoted_token(const char *src, char *out, size_t out_cap, const char **rest);

static int pthread_create_detached_with_stack(void *(*fn)(void *), void *arg) {
    pthread_t tid;
    pthread_attr_t attr;
    if (pthread_attr_init(&attr) != 0) {
        return pthread_create(&tid, NULL, fn, arg);
    }
    (void)pthread_attr_setstacksize(&attr, THREAD_STACK_SIZE);
    (void)pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    int rc = pthread_create(&tid, &attr, fn, arg);
    pthread_attr_destroy(&attr);
    return rc;
}

static long long get_json_rev(const char *path) {
    size_t len = 0;
    char *data = read_text_file(path, &len);
    if (!data) {
        return 0;
    }
    long long rev = 0;
    char *p = strstr(data, "\"rev\"");
    if (!p) {
        free(data);
        return 0;
    }
    p = strchr(p, ':');
    if (!p) {
        free(data);
        return 0;
    }
    p++;
    while (*p && isspace((unsigned char)*p)) p++;
    rev = strtoll(p, NULL, 10);
    free(data);
    return rev > 0 ? rev : 0;
}

static int mkdir_p(const char *path, mode_t mode, char *err, size_t err_len) {
    char tmp[PATH_MAX];
    size_t len;

    if(!path || !*path) {
        snprintf(err, err_len, "Empty path");
        return -1;
    }

    len = strlen(path);
    if(len >= sizeof(tmp)) {
        snprintf(err, err_len, "Path too long");
        return -1;
    }

    strncpy(tmp, path, sizeof(tmp) - 1);
    tmp[sizeof(tmp) - 1] = '\0';

    if(tmp[len - 1] == '/') {
        tmp[len - 1] = '\0';
    }

    for(char *p = tmp + 1; *p; p++) {
        if(*p == '/') {
            *p = '\0';
            if(mkdir(tmp, mode) != 0 && errno != EEXIST) {
                snprintf(err, err_len, "mkdir %s failed: %s", tmp, strerror(errno));
                return -1;
            }
            *p = '/';
        }
    }

    if(mkdir(tmp, mode) != 0 && errno != EEXIST) {
        snprintf(err, err_len, "mkdir %s failed: %s", tmp, strerror(errno));
        return -1;
    }

    return 0;
}

static void trim_newline(char *path) {
    size_t len = strlen(path);
    while (len > 0 && (path[len - 1] == '\n' || path[len - 1] == '\r')) {
        path[len - 1] = '\0';
        len--;
    }
}



struct RemoveNode {
    char path[PATH_MAX];
    int visited;
};

static int remove_recursive(const char *path, char *err, size_t err_len) {
    size_t cap = 64;
    size_t len = 0;
    struct RemoveNode *stack = (struct RemoveNode *)malloc(cap * sizeof(*stack));
    if (!stack) {
        snprintf(err, err_len, "Out of memory");
        return -1;
    }
    snprintf(stack[0].path, sizeof(stack[0].path), "%s", path);
    stack[0].visited = 0;
    len = 1;

    while (len > 0) {
        struct RemoveNode node = stack[--len];
        payload_set_crash_context("REMOVE_NODE", node.path, NULL);
        struct stat st;
        if (lstat(node.path, &st) != 0) {
            if (errno == ENOENT) {
                continue;
            }
            snprintf(err, err_len, "lstat %s failed: %s", node.path, strerror(errno));
            free(stack);
            return -1;
        }

        if (S_ISDIR(st.st_mode)) {
            if (!node.visited) {
                if (len == cap) {
                    cap *= 2;
                    struct RemoveNode *next = (struct RemoveNode *)realloc(stack, cap * sizeof(*stack));
                    if (!next) {
                        snprintf(err, err_len, "Out of memory");
                        free(stack);
                        return -1;
                    }
                    stack = next;
                }
                stack[len].visited = 1;
                snprintf(stack[len].path, sizeof(stack[len].path), "%s", node.path);
                len++;

                DIR *dir = opendir(node.path);
                if (!dir) {
                    snprintf(err, err_len, "opendir %s failed: %s", node.path, strerror(errno));
                    free(stack);
                    return -1;
                }
                struct dirent *entry;
                while ((entry = readdir(dir)) != NULL) {
                    if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
                        continue;
                    }
                    char child[PATH_MAX];
                    snprintf(child, sizeof(child), "%s/%s", node.path, entry->d_name);
                    payload_set_crash_context("REMOVE_CHILD", child, NULL);
                    if (len == cap) {
                        cap *= 2;
                        struct RemoveNode *next = (struct RemoveNode *)realloc(stack, cap * sizeof(*stack));
                        if (!next) {
                            closedir(dir);
                            snprintf(err, err_len, "Out of memory");
                            free(stack);
                            return -1;
                        }
                        stack = next;
                    }
                    stack[len].visited = 0;
                    snprintf(stack[len].path, sizeof(stack[len].path), "%s", child);
                    len++;
                }
                closedir(dir);
                continue;
            }
            if (rmdir(node.path) != 0) {
                snprintf(err, err_len, "rmdir %s failed: %s", node.path, strerror(errno));
                free(stack);
                return -1;
            }
            continue;
        }

        if (unlink(node.path) != 0) {
            snprintf(err, err_len, "unlink %s failed: %s", node.path, strerror(errno));
            free(stack);
            return -1;
        }
    }

    free(stack);
    return 0;
}

struct DeleteTask {
    char path[PATH_MAX];
};

static void *delete_worker(void *arg) {
    struct DeleteTask *task = (struct DeleteTask *)arg;
    if (!task) return NULL;
    char err[256] = {0};
    if (remove_recursive(task->path, err, sizeof(err)) != 0) {
        printf("[DELETE] Failed: %s (%s)\n", task->path, err);
    } else {
        printf("[DELETE] Completed: %s\n", task->path);
    }
    free(task);
    return NULL;
}

struct CopyProgressCtx {
    int sock;
    const char *prefix;
    unsigned long long total;
    unsigned long long processed;
    unsigned long long bytes_since_send;
    time_t last_send;
    int cancelled;
    time_t last_notify;
    unsigned int notify_interval_sec;
};

static void copy_progress_check_cancel(struct CopyProgressCtx *ctx) {
    if (!ctx || ctx->sock < 0 || ctx->cancelled) {
        return;
    }
    char buf[64];
    ssize_t n = recv(ctx->sock, buf, sizeof(buf) - 1, MSG_DONTWAIT);
    if (n == 0) {
        ctx->cancelled = 1;
        return;
    }
    if (n < 0) {
        if (errno == EWOULDBLOCK || errno == EAGAIN || errno == EINTR) {
            return;
        }
        ctx->cancelled = 1;
        return;
    }
    buf[n] = '\0';
    if (strstr(buf, "CANCEL") != NULL) {
        ctx->cancelled = 1;
    }
}

static void copy_progress_send(struct CopyProgressCtx *ctx, int force) {
    if (!ctx || ctx->sock < 0) {
        return;
    }
    copy_progress_check_cancel(ctx);
    if (ctx->cancelled) {
        return;
    }
    time_t now = time(NULL);
    if (!force) {
        if (ctx->bytes_since_send < COPY_PROGRESS_BYTES &&
            now - ctx->last_send < COPY_PROGRESS_INTERVAL_SEC) {
            return;
        }
    }
    char msg[128];
    int len = snprintf(msg, sizeof(msg), "%s %llu %llu\n",
                       ctx->prefix, ctx->processed, ctx->total);
    if (len > 0) {
        if (send_all(ctx->sock, msg, (size_t)len) != 0) {
            payload_log("[%s] Progress send failed", ctx->prefix ? ctx->prefix : "COPY");
            ctx->cancelled = 1;
            return;
        }
        ctx->last_send = now;
        ctx->bytes_since_send = 0;
    }
    (void)force;
}

static void copy_progress_file(struct CopyProgressCtx *ctx, const char *path) {
    if (!ctx || ctx->sock < 0 || !path) {
        return;
    }
    copy_progress_check_cancel(ctx);
    if (ctx->cancelled) {
        return;
    }
    const char *prefix = ctx->prefix ? ctx->prefix : "COPY_PROGRESS";
    char msg[PATH_MAX + 32];
    int len = snprintf(msg, sizeof(msg), "%s_FILE %s\n", prefix, path);
    if (len > 0) {
        if (send_all(ctx->sock, msg, (size_t)len) != 0) {
            payload_log("[%s] File send failed", prefix);
            ctx->cancelled = 1;
        }
    }
}

static void copy_progress_add(struct CopyProgressCtx *ctx, unsigned long long bytes) {
    if (!ctx) {
        return;
    }
    copy_progress_check_cancel(ctx);
    if (ctx->cancelled) {
        return;
    }
    ctx->processed += bytes;
    ctx->bytes_since_send += bytes;
    copy_progress_send(ctx, 0);
}

static void copy_progress_heartbeat(struct CopyProgressCtx *ctx) {
    if (!ctx) {
        return;
    }
    copy_progress_check_cancel(ctx);
    if (ctx->cancelled) {
        return;
    }
    time_t now = time(NULL);
    if (now - ctx->last_send >= COPY_PROGRESS_INTERVAL_SEC) {
        copy_progress_send(ctx, 1);
    }
}

struct ScanNode {
    char path[PATH_MAX];
};

static int scan_size_recursive(const char *src, unsigned long long *total,
                               struct CopyProgressCtx *ctx, char *err, size_t err_len) {
    size_t cap = 64;
    size_t len = 0;
    struct ScanNode *stack = (struct ScanNode *)malloc(cap * sizeof(*stack));
    if (!stack) {
        snprintf(err, err_len, "Out of memory");
        return -1;
    }
    snprintf(stack[0].path, sizeof(stack[0].path), "%s", src);
    len = 1;

    while (len > 0) {
        struct ScanNode node = stack[--len];
        payload_set_crash_context("SCAN_SIZE", node.path, NULL);
        struct stat st;
        if (lstat(node.path, &st) != 0) {
            snprintf(err, err_len, "lstat %s failed: %s", node.path, strerror(errno));
            free(stack);
            return -1;
        }
        if (ctx && ctx->cancelled) {
            snprintf(err, err_len, "Cancelled");
            free(stack);
            return -1;
        }
        if (S_ISDIR(st.st_mode)) {
            DIR *dir = opendir(node.path);
            if (!dir) {
                snprintf(err, err_len, "opendir %s failed: %s", node.path, strerror(errno));
                free(stack);
                return -1;
            }
            struct dirent *entry;
            while ((entry = readdir(dir)) != NULL) {
                if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
                    continue;
                }
                char child_src[PATH_MAX];
                snprintf(child_src, sizeof(child_src), "%s/%s", node.path, entry->d_name);
                payload_set_crash_context("SCAN_SIZE", child_src, NULL);
                if (lstat(child_src, &st) != 0) {
                    closedir(dir);
                    snprintf(err, err_len, "lstat %s failed: %s", child_src, strerror(errno));
                    free(stack);
                    return -1;
                }
                if (S_ISDIR(st.st_mode)) {
                    if (len == cap) {
                        cap *= 2;
                        struct ScanNode *next = (struct ScanNode *)realloc(stack, cap * sizeof(*stack));
                        if (!next) {
                            closedir(dir);
                            snprintf(err, err_len, "Out of memory");
                            free(stack);
                            return -1;
                        }
                        stack = next;
                    }
                    snprintf(stack[len].path, sizeof(stack[len].path), "%s", child_src);
                    len++;
                } else if (S_ISREG(st.st_mode)) {
                    *total += (unsigned long long)st.st_size;
                } else {
                    closedir(dir);
                    snprintf(err, err_len, "Unsupported file type: %s", child_src);
                    free(stack);
                    return -1;
                }
            }
            closedir(dir);
            copy_progress_heartbeat(ctx);
            continue;
        }
        if (S_ISREG(st.st_mode)) {
            *total += (unsigned long long)st.st_size;
            continue;
        }
        snprintf(err, err_len, "Unsupported file type: %s", node.path);
        free(stack);
        return -1;
    }

    free(stack);
    return 0;
}

static int copy_file(const char *src, const char *dst, mode_t mode,
                     char *err, size_t err_len, struct CopyProgressCtx *ctx) {
    payload_set_crash_context("COPY_FILE", src, dst);
    copy_progress_file(ctx, src);
    int in_fd = open(src, O_RDONLY);
    if (in_fd < 0) {
        snprintf(err, err_len, "open %s failed: %s", src, strerror(errno));
        return -1;
    }

    int out_fd = open(dst, O_WRONLY | O_CREAT | O_TRUNC, mode);
    if (out_fd < 0) {
        snprintf(err, err_len, "open %s failed: %s", dst, strerror(errno));
        close(in_fd);
        return -1;
    }

    const size_t buffer_len = 256 * 1024;
    char *buffer = (char *)malloc(buffer_len);
    if (!buffer) {
        snprintf(err, err_len, "Out of memory");
        close(in_fd);
        close(out_fd);
        return -1;
    }
    ssize_t n;
    while ((n = read(in_fd, buffer, buffer_len)) > 0) {
        if (ctx && ctx->cancelled) {
            snprintf(err, err_len, "Cancelled");
            free(buffer);
            close(in_fd);
            close(out_fd);
            return -1;
        }
        ssize_t written = 0;
        while (written < n) {
            ssize_t w = write(out_fd, buffer + written, (size_t)(n - written));
            if (w < 0) {
                snprintf(err, err_len, "write %s failed: %s", dst, strerror(errno));
                free(buffer);
                close(in_fd);
                close(out_fd);
                return -1;
            }
            written += w;
            copy_progress_add(ctx, (unsigned long long)w);
            if (ctx && ctx->cancelled) {
                snprintf(err, err_len, "Cancelled");
                free(buffer);
                close(in_fd);
                close(out_fd);
                return -1;
            }
        }
    }
    if (n < 0) {
        snprintf(err, err_len, "read %s failed: %s", src, strerror(errno));
        free(buffer);
        close(in_fd);
        close(out_fd);
        return -1;
    }

    free(buffer);
    close(in_fd);
    close(out_fd);
    return 0;
}

struct CopyNode {
    char src[PATH_MAX];
    char dst[PATH_MAX];
};

static int copy_recursive(const char *src, const char *dst, char *err, size_t err_len,
                          struct CopyProgressCtx *ctx) {
    size_t cap = 64;
    size_t len = 0;
    struct CopyNode *stack = (struct CopyNode *)malloc(cap * sizeof(*stack));
    if (!stack) {
        snprintf(err, err_len, "Out of memory");
        return -1;
    }
    snprintf(stack[0].src, sizeof(stack[0].src), "%s", src);
    snprintf(stack[0].dst, sizeof(stack[0].dst), "%s", dst);
    len = 1;

    while (len > 0) {
        struct CopyNode node = stack[--len];
        payload_set_crash_context("COPY_NODE", node.src, node.dst);
        struct stat st;
        if (lstat(node.src, &st) != 0) {
            snprintf(err, err_len, "lstat %s failed: %s", node.src, strerror(errno));
            free(stack);
            return -1;
        }
        if (ctx && ctx->cancelled) {
            snprintf(err, err_len, "Cancelled");
            free(stack);
            return -1;
        }
        if (S_ISDIR(st.st_mode)) {
            if (mkdir(node.dst, st.st_mode & 0777) != 0 && errno != EEXIST) {
                snprintf(err, err_len, "mkdir %s failed: %s", node.dst, strerror(errno));
                free(stack);
                return -1;
            }
            DIR *dir = opendir(node.src);
            if (!dir) {
                snprintf(err, err_len, "opendir %s failed: %s", node.src, strerror(errno));
                free(stack);
                return -1;
            }
            struct dirent *entry;
            while ((entry = readdir(dir)) != NULL) {
                if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
                    continue;
                }
                if (ctx && ctx->cancelled) {
                    closedir(dir);
                    snprintf(err, err_len, "Cancelled");
                    free(stack);
                    return -1;
                }
                char child_src[PATH_MAX];
                char child_dst[PATH_MAX];
                snprintf(child_src, sizeof(child_src), "%s/%s", node.src, entry->d_name);
                snprintf(child_dst, sizeof(child_dst), "%s/%s", node.dst, entry->d_name);
                payload_set_crash_context("COPY_CHILD", child_src, child_dst);
                if (lstat(child_src, &st) != 0) {
                    closedir(dir);
                    snprintf(err, err_len, "lstat %s failed: %s", child_src, strerror(errno));
                    free(stack);
                    return -1;
                }
                if (S_ISDIR(st.st_mode)) {
                    if (len == cap) {
                        cap *= 2;
                        struct CopyNode *next = (struct CopyNode *)realloc(stack, cap * sizeof(*stack));
                        if (!next) {
                            closedir(dir);
                            snprintf(err, err_len, "Out of memory");
                            free(stack);
                            return -1;
                        }
                        stack = next;
                    }
                    snprintf(stack[len].src, sizeof(stack[len].src), "%s", child_src);
                    snprintf(stack[len].dst, sizeof(stack[len].dst), "%s", child_dst);
                    len++;
                } else if (S_ISREG(st.st_mode)) {
                    if (copy_file(child_src, child_dst, st.st_mode & 0777, err, err_len, ctx) != 0) {
                        closedir(dir);
                        free(stack);
                        return -1;
                    }
                } else {
                    closedir(dir);
                    snprintf(err, err_len, "Unsupported file type: %s", child_src);
                    free(stack);
                    return -1;
                }
            }
            closedir(dir);
            continue;
        }
        if (S_ISREG(st.st_mode)) {
            if (copy_file(node.src, node.dst, st.st_mode & 0777, err, err_len, ctx) != 0) {
                free(stack);
                return -1;
            }
            continue;
        }
        snprintf(err, err_len, "Unsupported file type: %s", node.src);
        free(stack);
        return -1;
    }

    free(stack);
    return 0;
}

static int chmod_recursive(const char *path, mode_t mode, char *err, size_t err_len) {
    struct stat st;
    if (lstat(path, &st) != 0) {
        snprintf(err, err_len, "lstat %s failed: %s", path, strerror(errno));
        return -1;
    }

    if (chmod(path, mode) != 0) {
        snprintf(err, err_len, "chmod %s failed: %s", path, strerror(errno));
        return -1;
    }

    if (S_ISDIR(st.st_mode)) {
        DIR *dir = opendir(path);
        if (!dir) {
            snprintf(err, err_len, "opendir %s failed: %s", path, strerror(errno));
            return -1;
        }
        struct dirent *entry;
        while ((entry = readdir(dir)) != NULL) {
            if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
                continue;
            }
            char child[PATH_MAX];
            snprintf(child, sizeof(child), "%s/%s", path, entry->d_name);
            if (chmod_recursive(child, mode, err, err_len) != 0) {
                closedir(dir);
                return -1;
            }
        }
        closedir(dir);
    }

    return 0;
}






enum DownloadCompression {
    DL_COMP_NONE = 0,
    DL_COMP_LZ4 = 1,
    DL_COMP_ZSTD = 2,
    DL_COMP_LZMA = 3
};

struct DownloadPack {
    uint8_t *buf;
    size_t len;
    uint32_t records;
    enum DownloadCompression compress;
};

static int send_all(int sock, const void *buf, size_t len) {
    const uint8_t *p = (const uint8_t *)buf;
    size_t sent = 0;
    time_t last_ok = time(NULL);
    while (sent < len) {
        ssize_t n = send(sock, p + sent, len - sent, 0);
        if (n > 0) {
            sent += (size_t)n;
            last_ok = time(NULL);
            continue;
        }
        if (n < 0 && (errno == EWOULDBLOCK || errno == EAGAIN || errno == EINTR)) {
            if (time(NULL) - last_ok > 60) {
                return -1;
            }
            usleep(1000);
            continue;
        }
        return -1;
    }
    return 0;
}

static int recv_exact(int sock, void *buf, size_t len) {
    size_t read_total = 0;
    char *ptr = (char *)buf;
    while (read_total < len) {
        ssize_t n = recv(sock, ptr + read_total, len - read_total, 0);
        if (n < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (n == 0) {
            return -1;
        }
        read_total += (size_t)n;
    }
    return 0;
}

static time_t get_file_mtime(const char *path) {
    struct stat st;
    if (stat(path, &st) != 0) {
        return 0;
    }
    return st.st_mtime;
}

static int write_text_file(const char *path, const char *data, size_t len) {
    FILE *fp = fopen(path, "wb");
    if (!fp) return -1;
    if (len > 0 && fwrite(data, 1, len, fp) != len) {
        fclose(fp);
        return -1;
    }
    fclose(fp);
    return 0;
}

static char *read_text_file(const char *path, size_t *out_len) {
    FILE *fp = fopen(path, "rb");
    if (!fp) return NULL;
    fseek(fp, 0, SEEK_END);
    long size = ftell(fp);
    fseek(fp, 0, SEEK_SET);
    if (size < 0) {
        fclose(fp);
        return NULL;
    }
    char *buf = malloc((size_t)size + 1);
    if (!buf) {
        fclose(fp);
        return NULL;
    }
    size_t read_len = fread(buf, 1, (size_t)size, fp);
    fclose(fp);
    buf[read_len] = '\0';
    if (out_len) *out_len = read_len;
    return buf;
}

static int send_frame_header(int sock, uint32_t type, uint64_t len) {
    struct FrameHeader hdr;
    hdr.magic = MAGIC_FTX1;
    hdr.type = type;
    hdr.body_len = len;
    return send_all(sock, &hdr, sizeof(hdr));
}

static void send_error_frame(int sock, const char *msg) {
    if (!msg) {
        return;
    }
    size_t len = strlen(msg);
    if (send_frame_header(sock, FRAME_ERROR, len) != 0) {
        return;
    }
    send_all(sock, msg, len);
}

static int download_pack_init(struct DownloadPack *pack, enum DownloadCompression compress) {
    pack->buf = malloc(DOWNLOAD_PACK_BUFFER_SIZE);
    if (!pack->buf) {
        return -1;
    }
    pack->len = 4;
    pack->records = 0;
    pack->compress = compress;
    memset(pack->buf, 0, 4);
    return 0;
}

static void download_pack_reset(struct DownloadPack *pack) {
    pack->len = 4;
    pack->records = 0;
    memset(pack->buf, 0, 4);
}

static int download_pack_send_raw(int sock, struct DownloadPack *pack) {
    if (send_frame_header(sock, FRAME_PACK_V4, pack->len) != 0) {
        return -1;
    }
        if (send_all(sock, pack->buf, pack->len) != 0) {
            payload_log("[DOWNLOAD_DIR] send failed (%s)", strerror(errno));
            return -1;
        }
    return 0;
}

static int download_pack_flush(int sock, struct DownloadPack *pack) {
    if (pack->records == 0) {
        return 0;
    }
    memcpy(pack->buf, &pack->records, 4);
    if (pack->compress == DL_COMP_LZ4) {
        int max_dst = LZ4_compressBound((int)pack->len);
        uint8_t *tmp = malloc((size_t)max_dst + 4);
        if (!tmp) {
            return -1;
        }
        uint32_t raw_len = (uint32_t)pack->len;
        memcpy(tmp, &raw_len, 4);
        int compressed = LZ4_compress_default((const char *)pack->buf, (char *)tmp + 4, (int)pack->len, max_dst);
        if (compressed <= 0 || (size_t)compressed + 4 >= pack->len) {
            free(tmp);
            return download_pack_send_raw(sock, pack);
        }
        size_t out_len = (size_t)compressed + 4;
        if (send_frame_header(sock, FRAME_PACK_LZ4_V4, out_len) != 0) {
            free(tmp);
            return -1;
        }
        if (send_all(sock, tmp, out_len) != 0) {
            payload_log("[DOWNLOAD_DIR] send failed (%s)", strerror(errno));
            free(tmp);
            return -1;
        }
        payload_touch_activity();
        free(tmp);
    } else if (pack->compress == DL_COMP_ZSTD) {
        size_t max_dst = ZSTD_compressBound(pack->len);
        uint8_t *tmp = malloc(max_dst + 4);
        if (!tmp) {
            return -1;
        }
        uint32_t raw_len = (uint32_t)pack->len;
        memcpy(tmp, &raw_len, 4);
        size_t compressed = ZSTD_compress(tmp + 4, max_dst, pack->buf, pack->len, 19);
        if (ZSTD_isError(compressed) || compressed + 4 >= pack->len) {
            free(tmp);
            return download_pack_send_raw(sock, pack);
        }
        size_t out_len = compressed + 4;
        if (send_frame_header(sock, FRAME_PACK_ZSTD_V4, out_len) != 0) {
            free(tmp);
            return -1;
        }
        if (send_all(sock, tmp, out_len) != 0) {
            payload_log("[DOWNLOAD_DIR] send failed (%s)", strerror(errno));
            free(tmp);
            return -1;
        }
        payload_touch_activity();
        free(tmp);
    } else if (pack->compress == DL_COMP_LZMA) {
        size_t max_dst = pack->len + (pack->len / 3) + 256;
        uint8_t *tmp = malloc(max_dst + 17);
        if (!tmp) {
            return -1;
        }
        uint32_t raw_len = (uint32_t)pack->len;
        memcpy(tmp, &raw_len, 4);
        unsigned char props[LZMA_PROPS_SIZE];
        size_t props_size = LZMA_PROPS_SIZE;
        size_t dest_len = max_dst;
        int res = LzmaCompress(
            tmp + 17, &dest_len, pack->buf, pack->len,
            props, &props_size, 9, 0, -1, -1, -1, -1, 1
        );
        if (res != SZ_OK || props_size != LZMA_PROPS_SIZE || dest_len + 17 >= pack->len) {
            free(tmp);
            return download_pack_send_raw(sock, pack);
        }
        memcpy(tmp + 4, props, LZMA_PROPS_SIZE);
        uint64_t raw64 = pack->len;
        memcpy(tmp + 9, &raw64, 8);
        size_t out_len = dest_len + 17;
        if (send_frame_header(sock, FRAME_PACK_LZMA_V4, out_len) != 0) {
            free(tmp);
            return -1;
        }
        if (send_all(sock, tmp, out_len) != 0) {
            payload_log("[DOWNLOAD_DIR] send failed (%s)", strerror(errno));
            free(tmp);
            return -1;
        }
        payload_touch_activity();
        free(tmp);
    } else {
        if (download_pack_send_raw(sock, pack) != 0) {
            return -1;
        }
        payload_touch_activity();
    }
    download_pack_reset(pack);
    return 0;
}

static int download_pack_add_record(int sock, struct DownloadPack *pack, const char *rel_path,
                                    const uint8_t *data, uint64_t data_len) {
    size_t path_len = strlen(rel_path);
    size_t overhead = 2 + 2 + path_len + 8;
    if (overhead + data_len > DOWNLOAD_PACK_BUFFER_SIZE - 4) {
        return -1;
    }
    if (pack->len + overhead + data_len > DOWNLOAD_PACK_BUFFER_SIZE) {
        if (download_pack_flush(sock, pack) != 0) {
            return -1;
        }
    }

    uint16_t path_len_u16 = (uint16_t)path_len;
    memcpy(pack->buf + pack->len, &path_len_u16, 2);
    pack->len += 2;
    uint16_t flags = 0;
    memcpy(pack->buf + pack->len, &flags, 2);
    pack->len += 2;
    memcpy(pack->buf + pack->len, rel_path, path_len);
    pack->len += path_len;
    memcpy(pack->buf + pack->len, &data_len, 8);
    pack->len += 8;
    memcpy(pack->buf + pack->len, data, data_len);
    pack->len += (size_t)data_len;

    pack->records++;
    return 0;
}

static size_t sample_dir_data(const char *base_path, uint8_t *buf, size_t max_len) {
    DIR *dir = opendir(base_path);
    if (!dir) {
        return 0;
    }

    size_t written = 0;
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL && written < max_len) {
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
            continue;
        }
        char full_path[PATH_MAX];
        snprintf(full_path, sizeof(full_path), "%s/%s", base_path, entry->d_name);

        struct stat st;
        if (stat(full_path, &st) != 0) {
            continue;
        }
        if (S_ISDIR(st.st_mode)) {
            size_t nested = sample_dir_data(full_path, buf + written, max_len - written);
            written += nested;
            continue;
        }
        if (!S_ISREG(st.st_mode)) {
            continue;
        }

        FILE *fp = fopen(full_path, "rb");
        if (!fp) {
            continue;
        }
        size_t to_read = max_len - written;
        if (to_read > 64 * 1024) {
            to_read = 64 * 1024;
        }
        size_t n = fread(buf + written, 1, to_read, fp);
        fclose(fp);
        if (n > 0) {
            written += n;
        }
    }
    closedir(dir);
    return written;
}

static enum DownloadCompression pick_auto_compression(const char *path) {
    const size_t sample_max = 512 * 1024;
    uint8_t *sample = malloc(sample_max);
    if (!sample) {
        return DL_COMP_NONE;
    }
    size_t sample_len = sample_dir_data(path, sample, sample_max);
    if (sample_len == 0) {
        free(sample);
        return DL_COMP_NONE;
    }

    enum DownloadCompression best = DL_COMP_NONE;

    int max_lz4 = LZ4_compressBound((int)sample_len);
    if (max_lz4 > 0) {
        uint8_t *tmp = malloc((size_t)max_lz4 + 4);
        if (tmp) {
            int compressed = LZ4_compress_default((const char *)sample, (char *)tmp + 4, (int)sample_len, max_lz4);
            if (compressed > 0) {
                size_t out_len = (size_t)compressed + 4;
                if (out_len < sample_len) {
                    best = DL_COMP_LZ4;
                }
            }
            free(tmp);
        }
    }

    free(sample);
    return best;
}

static int send_file_records_fd(int sock, struct DownloadPack *pack, const char *rel_path, int fd, uint64_t *out_bytes) {
    if (fd < 0) {
        return -1;
    }

    size_t path_len = strlen(rel_path);
    size_t overhead = 2 + 2 + path_len + 8;
    size_t max_chunk = DOWNLOAD_PACK_BUFFER_SIZE - 4 - overhead;
    if (max_chunk == 0) {
        return -1;
    }

    uint8_t *buf = malloc(max_chunk);
    if (!buf) {
        return -1;
    }

    ssize_t n;
    uint64_t total_read = 0;
    while ((n = read(fd, buf, max_chunk)) > 0) {
        if (download_pack_add_record(sock, pack, rel_path, buf, (uint64_t)n) != 0) {
            free(buf);
            return -1;
        }
        total_read += (uint64_t)n;
        payload_touch_activity();
    }

    free(buf);
    payload_touch_activity();
    if (n < 0) {
        printf("[DOWNLOAD] read failed (%s)\n", strerror(errno));
    }
    if (out_bytes) {
        *out_bytes = total_read;
    }
    return (n < 0) ? -1 : 0;
}

static int send_file_records(int sock, struct DownloadPack *pack, const char *rel_path, const char *path) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    int result = send_file_records_fd(sock, pack, rel_path, fd, NULL);
    close(fd);
    payload_touch_activity();
    return result;
}


static int download_raw_stream(int sock, const char *path, uint64_t start_offset, int include_offset_in_ready) {
    payload_set_crash_context("DOWNLOAD_RAW_STREAM", path, NULL);
    payload_log("[DOWNLOAD_RAW] Stream start for %s", path);

    // Set socket to have shorter send timeout for responsiveness
    struct timeval snd_to;
    snd_to.tv_sec = 15;
    snd_to.tv_usec = 0;
    setsockopt(sock, SOL_SOCKET, SO_SNDTIMEO, &snd_to, sizeof(snd_to));

    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: open failed: %s\n", strerror(errno));
        send(sock, error_msg, strlen(error_msg), 0);
        close(sock);
        return -1;
    }

    struct stat st;
    if (fstat(fd, &st) != 0 || !S_ISREG(st.st_mode)) {
        const char *error = "ERROR: Not a file\n";
        send(sock, error, strlen(error), 0);
        close(fd);
        close(sock);
        return -1;
    }

    if (start_offset > (uint64_t)st.st_size) {
        const char *error = "ERROR: Invalid offset\n";
        send(sock, error, strlen(error), 0);
        close(fd);
        close(sock);
        return -1;
    }
    if (start_offset > 0) {
        if (lseek(fd, (off_t)start_offset, SEEK_SET) < 0) {
            char error_msg[320];
            snprintf(error_msg, sizeof(error_msg), "ERROR: seek failed: %s\n", strerror(errno));
            send(sock, error_msg, strlen(error_msg), 0);
            close(fd);
            close(sock);
            return -1;
        }
    }

    char ready[128];
    if (include_offset_in_ready) {
        snprintf(ready, sizeof(ready), "READY %llu %llu\n",
            (unsigned long long)st.st_size,
            (unsigned long long)start_offset);
    } else {
        snprintf(ready, sizeof(ready), "READY %llu\n", (unsigned long long)st.st_size);
    }
    if (send_all(sock, ready, strlen(ready)) != 0) {
        close(fd);
        close(sock);
        return -1;
    }
    payload_touch_activity();

    // Use heap allocation instead of stack to avoid stack overflow
    // (default thread stack on PS5 may be smaller than 64KB + call chain overhead)
    const size_t bufsize = 64 * 1024;
    uint8_t *buffer = (uint8_t *)malloc(bufsize);
    if (!buffer) {
        payload_log("[DOWNLOAD_RAW] malloc failed for %s", path);
        close(fd);
        close(sock);
        return -1;
    }

    ssize_t n;
    while ((n = read(fd, buffer, bufsize)) > 0) {
        if (send_all(sock, buffer, (size_t)n) != 0) {
            payload_log("[DOWNLOAD_RAW] Send failed for %s", path);
            free(buffer);
            close(fd);
            close(sock);
            return -1;
        }
        payload_touch_activity();
    }

    free(buffer);
    close(fd);
    payload_log("[DOWNLOAD_RAW] Complete %s", path);
    close(sock);
    return 0;
}

void handle_download_raw(int client_sock, const char *path_arg) {
    char path[PATH_MAX];
    strncpy(path, path_arg, PATH_MAX-1);
    path[PATH_MAX-1] = '\0';
    trim_newline(path);

    payload_log("[DOWNLOAD_RAW] %s", path);
    payload_touch_activity();

    if (!is_path_safe(path)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        close(client_sock);
        return;
    }

    // Quick stat check before spawning thread
    struct stat st;
    if (stat(path, &st) != 0 || !S_ISREG(st.st_mode)) {
        const char *error = "ERROR: Not a file\n";
        send(client_sock, error, strlen(error), 0);
        close(client_sock);
        return;
    }

    // Stream directly on this connection for stability
    download_raw_stream(client_sock, path, 0, 0);
}

void handle_download_raw_from(int client_sock, const char *args) {
    if (!args) {
        const char *error = "ERROR: Invalid DOWNLOAD_RAW_FROM format\n";
        send(client_sock, error, strlen(error), 0);
        close(client_sock);
        return;
    }

    // Format: DOWNLOAD_RAW_FROM <offset>\t<path>
    const char *tab = strchr(args, '\t');
    if (!tab) {
        const char *error = "ERROR: Invalid DOWNLOAD_RAW_FROM format\n";
        send(client_sock, error, strlen(error), 0);
        close(client_sock);
        return;
    }

    char offset_buf[32];
    size_t offset_len = (size_t)(tab - args);
    if (offset_len == 0 || offset_len >= sizeof(offset_buf)) {
        const char *error = "ERROR: Invalid offset\n";
        send(client_sock, error, strlen(error), 0);
        close(client_sock);
        return;
    }
    memcpy(offset_buf, args, offset_len);
    offset_buf[offset_len] = '\0';

    char *endptr = NULL;
    unsigned long long parsed = strtoull(offset_buf, &endptr, 10);
    if (endptr == offset_buf || (endptr && *endptr != '\0')) {
        const char *error = "ERROR: Invalid offset\n";
        send(client_sock, error, strlen(error), 0);
        close(client_sock);
        return;
    }
    uint64_t start_offset = (uint64_t)parsed;

    char path[PATH_MAX];
    strncpy(path, tab + 1, PATH_MAX - 1);
    path[PATH_MAX - 1] = '\0';
    trim_newline(path);

    payload_log("[DOWNLOAD_RAW_FROM] offset=%llu path=%s",
        (unsigned long long)start_offset, path);
    payload_touch_activity();

    if (!is_path_safe(path)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        close(client_sock);
        return;
    }

    struct stat st;
    if (stat(path, &st) != 0 || !S_ISREG(st.st_mode)) {
        const char *error = "ERROR: Not a file\n";
        send(client_sock, error, strlen(error), 0);
        close(client_sock);
        return;
    }

    download_raw_stream(client_sock, path, start_offset, 1);
}

void handle_download_raw_range(int client_sock, const char *args) {
    if (!args) {
        const char *error = "ERROR: Invalid DOWNLOAD_RAW_RANGE format\n";
        send(client_sock, error, strlen(error), 0);
        close(client_sock);
        return;
    }

    // Format: DOWNLOAD_RAW_RANGE <offset> <length>\t<path>
    const char *tab = strchr(args, '\t');
    if (!tab) {
        const char *error = "ERROR: Invalid DOWNLOAD_RAW_RANGE format\n";
        send(client_sock, error, strlen(error), 0);
        close(client_sock);
        return;
    }

    char meta[96];
    size_t meta_len = (size_t)(tab - args);
    if (meta_len == 0 || meta_len >= sizeof(meta)) {
        const char *error = "ERROR: Invalid range\n";
        send(client_sock, error, strlen(error), 0);
        close(client_sock);
        return;
    }
    memcpy(meta, args, meta_len);
    meta[meta_len] = '\0';

    unsigned long long offset_ull = 0;
    unsigned long long length_ull = 0;
    if (sscanf(meta, "%llu %llu", &offset_ull, &length_ull) != 2) {
        const char *error = "ERROR: Invalid range\n";
        send(client_sock, error, strlen(error), 0);
        close(client_sock);
        return;
    }

    uint64_t start_offset = (uint64_t)offset_ull;
    uint64_t req_len = (uint64_t)length_ull;
    if (req_len == 0) {
        const char *error = "ERROR: Invalid length\n";
        send(client_sock, error, strlen(error), 0);
        close(client_sock);
        return;
    }

    char path[PATH_MAX];
    strncpy(path, tab + 1, PATH_MAX - 1);
    path[PATH_MAX - 1] = '\0';
    trim_newline(path);

    payload_log("[DOWNLOAD_RAW_RANGE] offset=%llu len=%llu path=%s",
        (unsigned long long)start_offset,
        (unsigned long long)req_len,
        path);
    payload_touch_activity();

    if (!is_path_safe(path)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        close(client_sock);
        return;
    }

    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: open failed: %s\n", strerror(errno));
        send(client_sock, error_msg, strlen(error_msg), 0);
        close(client_sock);
        return;
    }

    struct stat st;
    if (fstat(fd, &st) != 0 || !S_ISREG(st.st_mode)) {
        const char *error = "ERROR: Not a file\n";
        send(client_sock, error, strlen(error), 0);
        close(fd);
        close(client_sock);
        return;
    }

    uint64_t total_size = (uint64_t)st.st_size;
    if (start_offset > total_size) {
        const char *error = "ERROR: Invalid offset\n";
        send(client_sock, error, strlen(error), 0);
        close(fd);
        close(client_sock);
        return;
    }
    if (lseek(fd, (off_t)start_offset, SEEK_SET) < 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: seek failed: %s\n", strerror(errno));
        send(client_sock, error_msg, strlen(error_msg), 0);
        close(fd);
        close(client_sock);
        return;
    }

    uint64_t remain = total_size - start_offset;
    uint64_t send_len = req_len < remain ? req_len : remain;

    char ready[128];
    snprintf(ready, sizeof(ready), "READY %llu %llu %llu\n",
        (unsigned long long)total_size,
        (unsigned long long)start_offset,
        (unsigned long long)send_len);
    if (send_all(client_sock, ready, strlen(ready)) != 0) {
        close(fd);
        close(client_sock);
        return;
    }
    payload_touch_activity();

    const size_t bufsize = 256 * 1024;
    uint8_t *buffer = (uint8_t *)malloc(bufsize);
    if (!buffer) {
        const char *error = "ERROR: OOM\n";
        send(client_sock, error, strlen(error), 0);
        close(fd);
        close(client_sock);
        return;
    }

    uint64_t sent = 0;
    while (sent < send_len) {
        size_t to_read = (size_t)((send_len - sent) > (uint64_t)bufsize ? bufsize : (send_len - sent));
        ssize_t n = read(fd, buffer, to_read);
        if (n <= 0) {
            free(buffer);
            close(fd);
            close(client_sock);
            return;
        }
        if (send_all(client_sock, buffer, (size_t)n) != 0) {
            free(buffer);
            close(fd);
            close(client_sock);
            return;
        }
        sent += (uint64_t)n;
        payload_touch_activity();
    }

    free(buffer);
    close(fd);
    close(client_sock);
}


static int send_dir_recursive(int sock, struct DownloadPack *pack, const char *base_path, const char *rel_path) {
    char path[PATH_MAX];
    if (rel_path && *rel_path) {
        snprintf(path, sizeof(path), "%s/%s", base_path, rel_path);
    } else {
        snprintf(path, sizeof(path), "%s", base_path);
    }

    DIR *dir = opendir(path);
    if (!dir) {
        payload_log("[DOWNLOAD_DIR] opendir failed: %s (%s)", path, strerror(errno));
        return 0;
    }

    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
            continue;
        }
        char child_rel[PATH_MAX];
        if (rel_path && *rel_path) {
            snprintf(child_rel, sizeof(child_rel), "%s/%s", rel_path, entry->d_name);
        } else {
            snprintf(child_rel, sizeof(child_rel), "%s", entry->d_name);
        }
        char child_path[PATH_MAX];
        snprintf(child_path, sizeof(child_path), "%s/%s", base_path, child_rel);

        struct stat st;
        if (lstat(child_path, &st) != 0) {
            payload_log("[DOWNLOAD_DIR] lstat failed: %s (%s)", child_path, strerror(errno));
            continue;
        }
        if (S_ISLNK(st.st_mode)) {
            continue;
        }
        if (S_ISDIR(st.st_mode)) {
            if (send_dir_recursive(sock, pack, base_path, child_rel) != 0) {
                payload_log("[DOWNLOAD_DIR] recurse failed: %s", child_rel);
                continue;
            }
        } else if (S_ISREG(st.st_mode)) {
            payload_log("[DOWNLOAD_DIR] Sending %s", child_rel);
            if (send_file_records(sock, pack, child_rel, child_path) != 0) {
                payload_log("[DOWNLOAD_DIR] send_file_records failed: %s", child_rel);
                continue;
            }
            usleep(1000);
        }
    }

    closedir(dir);
    return 0;
}

static int calc_dir_size(const char *path, uint64_t *total) {
    DIR *dir = opendir(path);
    if (!dir) {
        payload_log("[DOWNLOAD_DIR] opendir failed (size): %s (%s)", path, strerror(errno));
        return 0;
    }
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
            continue;
        }
        char child[PATH_MAX];
        snprintf(child, sizeof(child), "%s/%s", path, entry->d_name);
        struct stat st;
        if (lstat(child, &st) != 0) {
            payload_log("[DOWNLOAD_DIR] lstat failed (size): %s (%s)", child, strerror(errno));
            continue;
        }
        if (S_ISLNK(st.st_mode)) {
            continue;
        }
        if (S_ISDIR(st.st_mode)) {
            if (calc_dir_size(child, total) != 0) {
                continue;
            }
        } else if (S_ISREG(st.st_mode)) {
            *total += (uint64_t)st.st_size;
        }
    }
    closedir(dir);
    return 0;
}

void handle_check_dir(int client_sock, const char *path_arg) {
    char path[PATH_MAX];
    if (!path_arg) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    if (path_arg[0] == '"') {
        const char *rest = NULL;
        if (parse_quoted_token(path_arg, path, sizeof(path), &rest) != 0) {
            const char *error = "ERROR: Invalid path\n";
            send(client_sock, error, strlen(error), 0);
            return;
        }
    } else {
        strncpy(path, path_arg, PATH_MAX-1);
        path[PATH_MAX-1] = '\0';
    }
    
    // Remove trailing newline
    trim_newline(path);

    struct stat st;
    int exists = 0;
    if(stat(path, &st) == 0 && S_ISDIR(st.st_mode)) {
        exists = 1;
    }

    if(exists) {
        send(client_sock, "EXISTS\n", 7, 0);
    } else {
        send(client_sock, "NOT_FOUND\n", 10, 0);
    }
}

void handle_test_write(int client_sock, const char *path_arg) {
    char path[PATH_MAX];
    strncpy(path, path_arg, PATH_MAX-1);
    path[PATH_MAX-1] = '\0';

    // Remove trailing newline
    trim_newline(path);

    if (!is_path_safe(path)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    // Create test file path
    char test_file[PATH_MAX];
    snprintf(test_file, PATH_MAX, "%s/.ps5upload_test", path);

    // Try to write test file
    FILE *fp = fopen(test_file, "w");
    if(!fp) {
        const char *error = "ERROR: Cannot write to path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    fprintf(fp, "PS5 Upload Test\n");
    fclose(fp);

    // Try to delete test file
    if(unlink(test_file) != 0) {
        const char *error = "ERROR: Cannot delete test file\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    // Success
    const char *success = "SUCCESS\n";
    send(client_sock, success, strlen(success), 0);
}

void handle_create_path(int client_sock, const char *path_arg) {
    char path[PATH_MAX];
    if (!path_arg) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    if (path_arg[0] == '"') {
        const char *rest = NULL;
        if (parse_quoted_token(path_arg, path, sizeof(path), &rest) != 0) {
            const char *error = "ERROR: Invalid path\n";
            send(client_sock, error, strlen(error), 0);
            return;
        }
    } else {
        strncpy(path, path_arg, PATH_MAX-1);
        path[PATH_MAX-1] = '\0';
    }

    // Remove trailing newline
    trim_newline(path);

    if (!is_path_safe(path)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    char mkdir_err[256] = {0};
    if(mkdir_p(path, 0777, mkdir_err, sizeof(mkdir_err)) != 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", mkdir_err);
        send(client_sock, error_msg, strlen(error_msg), 0);
        return;
    }

    const char *success = "SUCCESS\n";
    send(client_sock, success, strlen(success), 0);
}

void handle_delete_path(int client_sock, const char *path_arg) {
    char path[PATH_MAX];
    strncpy(path, path_arg, PATH_MAX-1);
    path[PATH_MAX-1] = '\0';
    trim_newline(path);

    if (!is_path_safe(path)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    char err[256] = {0};
    if (remove_recursive(path, err, sizeof(err)) != 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", err);
        send(client_sock, error_msg, strlen(error_msg), 0);
        return;
    }

    const char *success = "OK\n";
    send(client_sock, success, strlen(success), 0);
}

void handle_delete_path_async(int client_sock, const char *path_arg) {
    char path[PATH_MAX];
    strncpy(path, path_arg, PATH_MAX-1);
    path[PATH_MAX-1] = '\0';
    trim_newline(path);

    if (!is_path_safe(path)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    struct DeleteTask *task = malloc(sizeof(*task));
    if (!task) {
        const char *error = "ERROR: Out of memory\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    strncpy(task->path, path, sizeof(task->path) - 1);
    task->path[sizeof(task->path) - 1] = '\0';

    if (pthread_create_detached_with_stack(delete_worker, task) != 0) {
        free(task);
        const char *error = "ERROR: Delete thread failed\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    const char *success = "OK\n";
    send(client_sock, success, strlen(success), 0);
}

void handle_move_path(int client_sock, const char *args) {
    char buffer[PATH_MAX * 2];
    strncpy(buffer, args, sizeof(buffer) - 1);
    buffer[sizeof(buffer) - 1] = '\0';
    trim_newline(buffer);

    char *sep = strchr(buffer, '\t');
    if (!sep) {
        const char *error = "ERROR: Invalid MOVE format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    *sep = '\0';
    const char *src = buffer;
    const char *dst = sep + 1;
    if (!*src || !*dst) {
        const char *error = "ERROR: Invalid MOVE format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    if (!is_path_safe(src) || !is_path_safe(dst)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    payload_set_crash_context("MOVE_REQUEST", src, dst);
    payload_log("[MOVE] Request: %s -> %s", src, dst);
    if (rename(src, dst) == 0) {
        const char *success = "OK\n";
        send(client_sock, success, strlen(success), 0);
        payload_log("[MOVE] Rename success: %s -> %s", src, dst);
        return;
    }

    if (errno == EXDEV) {
        payload_set_crash_context("MOVE_CROSS_DEVICE", src, dst);
        payload_log("[MOVE] Cross-device move detected, copying then removing: %s -> %s", src, dst);
        struct CopyProgressCtx progress;
        memset(&progress, 0, sizeof(progress));
        progress.sock = client_sock;
        progress.prefix = "MOVE_PROGRESS";
        progress.last_send = time(NULL);
        progress.notify_interval_sec = 10;
        progress.last_notify = 0;
        copy_progress_send(&progress, 1);

        unsigned long long total_size = 0;
        char err[256] = {0};
        if (scan_size_recursive(src, &total_size, &progress, err, sizeof(err)) != 0) {
            char error_msg[320];
            snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", err);
            send(client_sock, error_msg, strlen(error_msg), 0);
            payload_log("[MOVE] Scan failed: %s (%s)", src, err);
            return;
        }
        progress.total = total_size;
        payload_log("[MOVE] Scan complete: %s bytes=%llu", src, total_size);
        copy_progress_send(&progress, 1);
        if (copy_recursive(src, dst, err, sizeof(err), &progress) != 0) {
            char error_msg[320];
            snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", err);
            send(client_sock, error_msg, strlen(error_msg), 0);
            payload_log("[MOVE] Copy failed: %s -> %s (%s)", src, dst, err);
            return;
        }
        copy_progress_send(&progress, 1);
        payload_log("[MOVE] Copy complete: %s -> %s", src, dst);
        if (remove_recursive(src, err, sizeof(err)) != 0) {
            char error_msg[320];
            snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", err);
            send(client_sock, error_msg, strlen(error_msg), 0);
            payload_log("[MOVE] Remove failed: %s (%s)", src, err);
            return;
        }
        payload_log("[MOVE] Remove complete: %s", src);
        const char *success = "OK\n";
        if (send(client_sock, success, strlen(success), 0) < 0) {
            payload_log("[MOVE] Send OK failed: %s", strerror(errno));
        }
        payload_log("[MOVE] Completed: %s -> %s", src, dst);
        return;
    }

    {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: rename failed: %s\n", strerror(errno));
        send(client_sock, error_msg, strlen(error_msg), 0);
        payload_log("[MOVE] Rename failed: %s -> %s (%s)", src, dst, strerror(errno));
    }
}

void handle_copy_path(int client_sock, const char *args) {
    char buffer[PATH_MAX * 2];
    strncpy(buffer, args, sizeof(buffer) - 1);
    buffer[sizeof(buffer) - 1] = '\0';
    trim_newline(buffer);

    char *sep = strchr(buffer, '\t');
    if (!sep) {
        const char *error = "ERROR: Invalid COPY format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    *sep = '\0';
    const char *src = buffer;
    const char *dst = sep + 1;
    if (!*src || !*dst) {
        const char *error = "ERROR: Invalid COPY format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    if (!is_path_safe(src) || !is_path_safe(dst)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    payload_set_crash_context("COPY_REQUEST", src, dst);
    payload_log("[COPY] Request: %s -> %s", src, dst);
    struct CopyProgressCtx progress;
    memset(&progress, 0, sizeof(progress));
    progress.sock = client_sock;
    progress.prefix = "COPY_PROGRESS";
    progress.last_send = time(NULL);
    progress.notify_interval_sec = 5;
    progress.last_notify = 0;
    copy_progress_send(&progress, 1);

    unsigned long long total_size = 0;
    char err[256] = {0};
    if (scan_size_recursive(src, &total_size, &progress, err, sizeof(err)) != 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", err);
        send(client_sock, error_msg, strlen(error_msg), 0);
        payload_log("[COPY] Scan failed: %s (%s)", src, err);
        return;
    }
    progress.total = total_size;
    payload_log("[COPY] Scan complete: %s bytes=%llu", src, total_size);
    copy_progress_send(&progress, 1);
    if (copy_recursive(src, dst, err, sizeof(err), &progress) != 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", err);
        send(client_sock, error_msg, strlen(error_msg), 0);
        payload_log("[COPY] Copy failed: %s -> %s (%s)", src, dst, err);
        return;
    }
    copy_progress_send(&progress, 1);

    const char *success = "OK\n";
    if (send(client_sock, success, strlen(success), 0) < 0) {
        payload_log("[COPY] Send OK failed: %s", strerror(errno));
    }
    payload_log("[COPY] Completed: %s -> %s", src, dst);
}

int handle_extract_archive(int client_sock, const char *args) {
    char buffer[PATH_MAX * 2];
    strncpy(buffer, args, sizeof(buffer) - 1);
    buffer[sizeof(buffer) - 1] = '\0';
    trim_newline(buffer);

    char *sep = strchr(buffer, '\t');
    if (!sep) {
        const char *error = "ERROR: Invalid EXTRACT_ARCHIVE format\n";
        send(client_sock, error, strlen(error), 0);
        return 0; // Do not hijack socket
    }
    *sep = '\0';
    const char *src = buffer;
    const char *dst = sep + 1;
    if (!*src || !*dst) {
        const char *error = "ERROR: Invalid EXTRACT_ARCHIVE format\n";
        send(client_sock, error, strlen(error), 0);
        return 0; // Do not hijack socket
    }

    if (!is_path_safe(src) || !is_path_safe(dst)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return 0; // Do not hijack socket
    }

    if (start_threaded_extraction(client_sock, src, dst) != 0) {
        // start_threaded_extraction sends its own error message on failure
        return 0; // Do not hijack socket
    }

    return 1; // Hijack socket
}

void handle_probe_rar(int client_sock, const char *args) {
    char src[PATH_MAX];
    if (!args) {
        const char *error = "ERROR: Invalid PROBE_RAR format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    while (*args == ' ' || *args == '\t') {
        args++;
    }
    snprintf(src, sizeof(src), "%s", args);
    size_t len = strlen(src);
    while (len > 0 && (src[len - 1] == '\n' || src[len - 1] == '\r' || src[len - 1] == ' ' || src[len - 1] == '\t')) {
        src[len - 1] = '\0';
        len--;
    }
    if (src[0] == '\0') {
        const char *error = "ERROR: Invalid PROBE_RAR format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    if (!is_path_safe(src)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    char *param_buf = NULL;
    size_t param_size = 0;
    char *cover_buf = NULL;
    size_t cover_size = 0;
    int result = unrar_probe_archive(src, &param_buf, &param_size, &cover_buf, &cover_size);
    if (result != UNRAR_OK) {
        char error_msg[128];
        snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", unrar_strerror(result));
        send(client_sock, error_msg, strlen(error_msg), 0);
        if (param_buf) free(param_buf);
        if (cover_buf) free(cover_buf);
        return;
    }

    char line[PROBE_RAR_MAX_LINE];
    int line_len = snprintf(line, sizeof(line), "META %zu\n", param_size);
    send_all(client_sock, line, (size_t)line_len);
    if (param_size > 0 && param_buf) {
        send_all(client_sock, param_buf, param_size);
        send_all(client_sock, "\n", 1);
    }
    line_len = snprintf(line, sizeof(line), "COVER %zu\n", cover_size);
    send_all(client_sock, line, (size_t)line_len);
    if (cover_size > 0 && cover_buf) {
        send_all(client_sock, cover_buf, cover_size);
        send_all(client_sock, "\n", 1);
    }
    send_all(client_sock, "DONE\n", 5);

    if (param_buf) free(param_buf);
    if (cover_buf) free(cover_buf);
}

void handle_chmod_777(int client_sock, const char *path_arg) {
    char path[PATH_MAX];
    strncpy(path, path_arg, PATH_MAX-1);
    path[PATH_MAX-1] = '\0';
    trim_newline(path);

    if (!is_path_safe(path)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    char err[256] = {0};
    if (chmod_recursive(path, 0777, err, sizeof(err)) != 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", err);
        send(client_sock, error_msg, strlen(error_msg), 0);
        return;
    }

    const char *success = "OK\n";
    send(client_sock, success, strlen(success), 0);
}

void handle_download_file(int client_sock, const char *path_arg) {
    char path[PATH_MAX];
    strncpy(path, path_arg, PATH_MAX-1);
    path[PATH_MAX-1] = '\0';
    trim_newline(path);

    payload_set_crash_context("DOWNLOAD", path, NULL);
    payload_log("[DOWNLOAD] %s", path);
    printf("[DOWNLOAD] %s\n", path);
    if (!is_path_safe(path)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    payload_touch_activity();
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: open failed: %s\n", strerror(errno));
        send(client_sock, error_msg, strlen(error_msg), 0);
        return;
    }
    struct stat st;
    if (fstat(fd, &st) != 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: stat failed: %s\n", strerror(errno));
        send(client_sock, error_msg, strlen(error_msg), 0);
        close(fd);
        return;
    }
    if (!S_ISREG(st.st_mode)) {
        const char *error = "ERROR: Not a file\n";
        send(client_sock, error, strlen(error), 0);
        close(fd);
        return;
    }
    close(fd);

    FILE *fp = fopen(path, "rb");
    if (!fp) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: open failed: %s\n", strerror(errno));
        send(client_sock, error_msg, strlen(error_msg), 0);
        return;
    }

    char header[128];
    snprintf(header, sizeof(header), "OK %llu\n", (unsigned long long)st.st_size);
    if (send_all(client_sock, header, strlen(header)) != 0) {
        fclose(fp);
        return;
    }
    printf("[DOWNLOAD] OK size=%llu\n", (unsigned long long)st.st_size);
    payload_touch_activity();

    // Use heap allocation to avoid stack overflow
    const size_t bufsize = 64 * 1024;
    char *buffer = (char *)malloc(bufsize);
    if (!buffer) {
        printf("[DOWNLOAD] malloc failed\n");
        fclose(fp);
        return;
    }

    size_t n;
    while ((n = fread(buffer, 1, bufsize, fp)) > 0) {
        if (send_all(client_sock, buffer, n) != 0) {
            printf("[DOWNLOAD] send failed (%s)\n", strerror(errno));
            break;
        }
        payload_touch_activity();
    }

    free(buffer);
    fclose(fp);
    payload_log("[DOWNLOAD] Complete %s", path);
    printf("[DOWNLOAD] Complete %s\n", path);
}

void handle_download_v2(int client_sock, const char *path_arg) {
    char path[PATH_MAX];
    strncpy(path, path_arg, PATH_MAX-1);
    path[PATH_MAX-1] = '\0';
    trim_newline(path);

    payload_log("[DOWNLOAD_V2] %s", path);
    payload_touch_activity();

    if (!is_path_safe(path)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: open failed: %s\n", strerror(errno));
        send(client_sock, error_msg, strlen(error_msg), 0);
        return;
    }
    struct stat st;
    if (fstat(fd, &st) != 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: stat failed: %s\n", strerror(errno));
        send(client_sock, error_msg, strlen(error_msg), 0);
        close(fd);
        return;
    }
    if (!S_ISREG(st.st_mode)) {
        const char *error = "ERROR: Not a file\n";
        send(client_sock, error, strlen(error), 0);
        close(fd);
        return;
    }

    char ready[96];
    snprintf(ready, sizeof(ready), "READY %llu\n", (unsigned long long)st.st_size);
    if (send_all(client_sock, ready, strlen(ready)) != 0) {
        close(fd);
        return;
    }
    payload_touch_activity();

    struct DownloadPack pack;
    if (download_pack_init(&pack, DL_COMP_NONE) != 0) {
        send_error_frame(client_sock, "Download failed: OOM");
        close(fd);
        return;
    }

    const char *name = strrchr(path, '/');
    const char *rel = name ? name + 1 : path;
    uint64_t bytes_sent = 0;
    if (send_file_records_fd(client_sock, &pack, rel, fd, &bytes_sent) != 0) {
        payload_log("[DOWNLOAD_V2] Failed while streaming %s", path);
        send_error_frame(client_sock, "Download failed");
        free(pack.buf);
        close(fd);
        return;
    }
    close(fd);
    if ((uint64_t)st.st_size != bytes_sent) {
        payload_log("[DOWNLOAD_V2] Size mismatch %s sent=%llu expected=%llu",
            path,
            (unsigned long long)bytes_sent,
            (unsigned long long)st.st_size);
        send_error_frame(client_sock, "Download size mismatch");
        free(pack.buf);
        return;
    }

    if (download_pack_flush(client_sock, &pack) != 0) {
        payload_log("[DOWNLOAD_V2] Failed flush for %s", path);
        send_error_frame(client_sock, "Download failed");
        free(pack.buf);
        return;
    }
    free(pack.buf);

    struct FrameHeader finish;
    finish.magic = MAGIC_FTX1;
    finish.type = FRAME_FINISH;
    finish.body_len = 0;
    send_all(client_sock, &finish, sizeof(finish));
    payload_log("[DOWNLOAD_V2] Complete %s", path);
}

void handle_download_dir(int client_sock, const char *path_arg) {
    payload_touch_activity();
    char path[PATH_MAX];
    strncpy(path, path_arg, PATH_MAX-1);
    path[PATH_MAX-1] = '\0';
    trim_newline(path);

    payload_log("[DOWNLOAD_DIR] %s", path);
    enum DownloadCompression compression = DL_COMP_NONE;
    int auto_requested = 0;
    char *tag = strstr(path, " LZ4");
    if (tag) {
        *tag = '\0';
        compression = DL_COMP_LZ4;
    }
    tag = strstr(path, " ZSTD");
    if (tag) {
        *tag = '\0';
        compression = DL_COMP_ZSTD;
    }
    tag = strstr(path, " LZMA");
    if (tag) {
        *tag = '\0';
        compression = DL_COMP_LZMA;
    }
    tag = strstr(path, " AUTO");
    if (tag) {
        *tag = '\0';
        auto_requested = 1;
        compression = pick_auto_compression(path);
    }

    if (!is_path_safe(path)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    struct stat st;
    if (stat(path, &st) != 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: stat failed: %s\n", strerror(errno));
        send(client_sock, error_msg, strlen(error_msg), 0);
        return;
    }
    if (!S_ISDIR(st.st_mode)) {
        const char *error = "ERROR: Not a directory\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    uint64_t total_size = 0;
    if (calc_dir_size(path, &total_size) != 0) {
        const char *error = "ERROR: Failed to stat directory\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    char ready[96];
    const char *comp_label = "NONE";
    if (compression == DL_COMP_LZ4) {
        comp_label = "LZ4";
    } else if (compression == DL_COMP_ZSTD) {
        comp_label = "ZSTD";
    } else if (compression == DL_COMP_LZMA) {
        comp_label = "LZMA";
    }
    if (auto_requested) {
        snprintf(ready, sizeof(ready), "READY %llu COMP %s\n",
                 (unsigned long long)total_size, comp_label);
    } else {
        snprintf(ready, sizeof(ready), "READY %llu\n", (unsigned long long)total_size);
    }
    if (send_all(client_sock, ready, strlen(ready)) != 0) {
        return;
    }
    payload_touch_activity();

    struct DownloadPack pack;
    if (download_pack_init(&pack, compression) != 0) {
        send_error_frame(client_sock, "Download failed: OOM");
        return;
    }

    payload_log("[DOWNLOAD_DIR] Streaming %s (total=%llu, comp=%s)", path,
        (unsigned long long)total_size, comp_label);
    if (send_dir_recursive(client_sock, &pack, path, NULL) != 0) {
        payload_log("[DOWNLOAD_DIR] Failed while streaming %s", path);
        send_error_frame(client_sock, "Download failed");
        free(pack.buf);
        return;
    }

    if (download_pack_flush(client_sock, &pack) != 0) {
        payload_log("[DOWNLOAD_DIR] Failed flush for %s", path);
        send_error_frame(client_sock, "Download failed");
        free(pack.buf);
        return;
    }
    payload_touch_activity();
    free(pack.buf);

    struct FrameHeader finish;
    finish.magic = MAGIC_FTX1;
    finish.type = FRAME_FINISH;
    finish.body_len = 0;
    send_all(client_sock, &finish, sizeof(finish));
    payload_log("[DOWNLOAD_DIR] Completed %s", path);
}

void handle_hash_file(int client_sock, const char *path_arg) {
    char path[PATH_MAX];
    strncpy(path, path_arg, PATH_MAX - 1);
    path[PATH_MAX - 1] = '\0';
    trim_newline(path);

    if (!is_path_safe(path)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    struct stat st;
    if (stat(path, &st) != 0 || !S_ISREG(st.st_mode)) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: Not a file\n");
        send(client_sock, error_msg, strlen(error_msg), 0);
        return;
    }

    FILE *fp = fopen(path, "rb");
    if (!fp) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: Failed to open\n");
        send(client_sock, error_msg, strlen(error_msg), 0);
        return;
    }

    Sha256Ctx ctx;
    sha256_init(&ctx);

    // Use heap allocation to avoid stack overflow
    const size_t bufsize = 64 * 1024;
    uint8_t *buffer = (uint8_t *)malloc(bufsize);
    if (!buffer) {
        const char *error = "ERROR: malloc failed\n";
        send(client_sock, error, strlen(error), 0);
        fclose(fp);
        return;
    }

    size_t n = 0;
    while ((n = fread(buffer, 1, bufsize, fp)) > 0) {
        sha256_update(&ctx, buffer, n);
    }
    free(buffer);
    fclose(fp);

    uint8_t hash[32];
    sha256_final(&ctx, hash);

    char hex[65];
    for (int i = 0; i < 32; i++) {
        snprintf(hex + (i * 2), 3, "%02x", hash[i]);
    }
    hex[64] = '\0';

    char msg[96];
    snprintf(msg, sizeof(msg), "OK %s\n", hex);
    send(client_sock, msg, strlen(msg), 0);
}

void handle_version(int client_sock) {
    char msg[64];
    snprintf(msg, sizeof(msg), "VERSION %s\n", PS5_UPLOAD_VERSION);
    send_all(client_sock, msg, strlen(msg));
}

void handle_get_space(int client_sock, const char *path_arg) {
    char path[PATH_MAX];
    strncpy(path, path_arg, PATH_MAX - 1);
    path[PATH_MAX - 1] = '\0';
    trim_newline(path);

    // Get filesystem stats
    struct statfs fs;
    if (statfs(path, &fs) != 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: statfs failed: %s\n", strerror(errno));
        send(client_sock, error_msg, strlen(error_msg), 0);
        return;
    }

    unsigned long long free_bytes = (unsigned long long)fs.f_bavail * fs.f_bsize;
    unsigned long long total_bytes = (unsigned long long)fs.f_blocks * fs.f_bsize;

    char msg[128];
    snprintf(msg, sizeof(msg), "OK %llu %llu\n", free_bytes, total_bytes);
    send_all(client_sock, msg, strlen(msg));
}

int clear_tmp_all(int *cleared, int *errors, char *last_err, size_t last_err_len) {
    const char *mount_points[] = {
        "/data",
        "/mnt/ext0",
        "/mnt/ext1",
        "/mnt/usb0",
        "/mnt/usb1",
        "/mnt/usb2",
        "/mnt/usb3",
        "/mnt/usb4",
        "/mnt/usb5",
        "/mnt/usb6",
        "/mnt/usb7",
        NULL
    };

    int cleared_local = 0;
    int errors_local = 0;
    char last_err_local[256] = {0};

    for (int i = 0; mount_points[i] != NULL; i++) {
        const char *root = mount_points[i];
        struct stat st;
        if (stat(root, &st) != 0 || !S_ISDIR(st.st_mode)) {
            continue;
        }
        char tmp_path[PATH_MAX];
        snprintf(tmp_path, sizeof(tmp_path), "%s/ps5upload/tmp", root);

        char err[256] = {0};
        if (remove_recursive(tmp_path, err, sizeof(err)) != 0) {
            errors_local++;
            if (last_err_local[0] == '\0') {
                snprintf(last_err_local, sizeof(last_err_local), "%s", err);
            }
            continue;
        }
        mkdir(tmp_path, 0777);
        chmod(tmp_path, 0777);
        cleared_local++;
    }

    if (cleared) {
        *cleared = cleared_local;
    }
    if (errors) {
        *errors = errors_local;
    }
    if (last_err && last_err_len > 0) {
        if (last_err_local[0]) {
            strncpy(last_err, last_err_local, last_err_len - 1);
            last_err[last_err_len - 1] = '\0';
        } else {
            last_err[0] = '\0';
        }
    }
    return errors_local > 0 ? -1 : 0;
}

void handle_clear_tmp(int client_sock) {
    int cleared = 0;
    int errors = 0;
    char last_err[256] = {0};
    clear_tmp_all(&cleared, &errors, last_err, sizeof(last_err));
    if (errors > 0) {
        char msg[512];
        snprintf(msg, sizeof(msg), "ERROR: Clear tmp failed: %s\n", last_err[0] ? last_err : "unknown error");
        send(client_sock, msg, strlen(msg), 0);
        return;
    }
    char msg[128];
    snprintf(msg, sizeof(msg), "OK %d\n", cleared);
    send_all(client_sock, msg, strlen(msg));
}

static int parse_quoted_token(const char *src, char *out, size_t out_cap, const char **rest) {
    if (!src || !out || out_cap == 0) return -1;
    while (*src == ' ' || *src == '\t') src++;
    if (*src == '\0') return -1;
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
        while (*src && *src != ' ' && *src != '\t') {
            if (len + 1 >= out_cap) return -1;
            out[len++] = *src++;
        }
    }
    out[len] = '\0';
    if (rest) {
        while (*src == ' ' || *src == '\t') src++;
        *rest = src;
    }
    return 0;
}

void handle_upload_v4_wrapper(int client_sock, const char *args) {
    char dest_path[PATH_MAX];
    char mode[16] = {0};
    int chmod_each_file = 1;
    int chmod_final = 0;
    const char *rest = NULL;
    if (!args) {
        const char *error = "ERROR: Invalid UPLOAD_V4 format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    if (parse_quoted_token(args, dest_path, sizeof(dest_path), &rest) != 0) {
        const char *error = "ERROR: Invalid UPLOAD_V4 format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    if (rest && *rest) {
        char opts[128];
        strncpy(opts, rest, sizeof(opts) - 1);
        opts[sizeof(opts) - 1] = '\0';
        char *save = NULL;
        char *token = strtok_r(opts, " ", &save);
        if (token) {
            strncpy(mode, token, sizeof(mode) - 1);
            mode[sizeof(mode) - 1] = '\0';
        }
        while ((token = strtok_r(NULL, " ", &save)) != NULL) {
            if (strcasecmp(token, "NOCHMOD") == 0) {
                chmod_each_file = 0;
            } else if (strcasecmp(token, "CHMOD_END") == 0 || strcasecmp(token, "CHMOD_FINAL") == 0) {
                chmod_final = 1;
            } else if (strcasecmp(token, "CHMOD_EACH") == 0) {
                chmod_each_file = 1;
            }
        }
    }
    if (!is_path_safe(dest_path)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        printf("[UPLOAD_V4] ERROR: Invalid path: %s\n", dest_path);
        return;
    }

    int use_temp = 1;
    if (mode[0]) {
        if (strcasecmp(mode, "TEMP") == 0) {
            use_temp = 1;
        } else if (strcasecmp(mode, "DIRECT") == 0) {
            use_temp = 0;
        } else {
            const char *error = "ERROR: Invalid UPLOAD_V4 mode\n";
            send(client_sock, error, strlen(error), 0);
            return;
        }
    }

    printf("[UPLOAD_V4] Request: dest=%s mode=%s chmod_each=%d chmod_final=%d\n",
           dest_path, use_temp ? "TEMP" : "DIRECT",
           chmod_each_file ? 1 : 0, chmod_final ? 1 : 0);
    handle_upload_v4(client_sock, dest_path, use_temp, chmod_each_file, chmod_final);
}

void handle_upload_fast_wrapper(int client_sock, const char *args) {
    char dest_root[PATH_MAX];
    char rel_path[PATH_MAX];
    char mode[16] = {0};
    char size_token[64] = {0};
    const char *rest = NULL;
    uint64_t total_size = 0;
    int chmod_final = 0;

    if (!args) {
        const char *error = "ERROR: Invalid UPLOAD_FAST format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    if (parse_quoted_token(args, dest_root, sizeof(dest_root), &rest) != 0 ||
        parse_quoted_token(rest, rel_path, sizeof(rel_path), &rest) != 0 ||
        parse_quoted_token(rest, size_token, sizeof(size_token), &rest) != 0) {
        const char *error = "ERROR: Invalid UPLOAD_FAST format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    if (rest && *rest) {
        if (parse_quoted_token(rest, mode, sizeof(mode), &rest) != 0) {
            const char *error = "ERROR: Invalid UPLOAD_FAST mode\n";
            send(client_sock, error, strlen(error), 0);
            return;
        }
    }
    if (rest && *rest) {
        char opt[16] = {0};
        if (parse_quoted_token(rest, opt, sizeof(opt), &rest) == 0) {
            if (strcasecmp(opt, "CHMOD_END") == 0 || strcmp(opt, "1") == 0) {
                chmod_final = 1;
            }
        }
    }

    if (!is_path_safe(dest_root)) {
        const char *error = "ERROR: Invalid destination path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    if (strstr(rel_path, "..")) {
        const char *error = "ERROR: Invalid relative path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    errno = 0;
    unsigned long long parsed_size = strtoull(size_token, NULL, 10);
    if (errno != 0) {
        const char *error = "ERROR: Invalid upload size\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    total_size = (uint64_t)parsed_size;

    // Mad Max mode is a direct single-file stream path.
    if (mode[0] && strcasecmp(mode, "DIRECT") != 0) {
        const char *error = "ERROR: UPLOAD_FAST supports DIRECT mode only\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    char final_path[PATH_MAX];
    if (snprintf(final_path, sizeof(final_path), "%s/%s", dest_root, rel_path) >= (int)sizeof(final_path)) {
        const char *error = "ERROR: Destination path too long\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    char parent_path[PATH_MAX];
    strncpy(parent_path, final_path, sizeof(parent_path) - 1);
    parent_path[sizeof(parent_path) - 1] = '\0';
    char *slash = strrchr(parent_path, '/');
    if (slash && slash != parent_path) {
        *slash = '\0';
        char mkerr[128] = {0};
        if (mkdir_p(parent_path, 0777, mkerr, sizeof(mkerr)) != 0) {
            char error[256];
            snprintf(error, sizeof(error), "ERROR: %s\n", mkerr[0] ? mkerr : "mkdir failed");
            send(client_sock, error, strlen(error), 0);
            return;
        }
    }

    int fd = open(final_path, O_WRONLY | O_CREAT | O_TRUNC, chmod_final ? 0777 : 0666);
    if (fd < 0) {
        char error[256];
        snprintf(error, sizeof(error), "ERROR: open failed: %s\n", strerror(errno));
        send(client_sock, error, strlen(error), 0);
        return;
    }

    // Pre-allocate large files to reduce fragmentation for large uploads
    if (total_size > 100 * 1024 * 1024) {
        if (ftruncate(fd, (off_t)total_size) != 0) {
            close(fd);
            char error[256];
            snprintf(error, sizeof(error), "ERROR: ftruncate failed: %s\n", strerror(errno));
            send(client_sock, error, strlen(error), 0);
            return;
        }
    }

    const char *ready = "READY\n";
    if (send_all(client_sock, ready, strlen(ready)) != 0) {
        close(fd);
        return;
    }

    int upload_buf = UPLOAD_RCVBUF_SIZE;
    setsockopt(client_sock, SOL_SOCKET, SO_RCVBUF, &upload_buf, sizeof(upload_buf));

    // Keep the userspace staging buffer modest to reduce memory pressure.
    size_t buf_size = (size_t)BUFFER_SIZE;
    uint8_t *buffer = (uint8_t *)malloc(buf_size);
    if (!buffer) {
        close(fd);
        const char *error = "ERROR: Out of memory\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    uint64_t received = 0;
    while (received < total_size) {
        size_t want = (size_t)((total_size - received) > (uint64_t)buf_size ? (uint64_t)buf_size : (total_size - received));
        ssize_t n = recv(client_sock, buffer, want, 0);
        if (n <= 0) {
            free(buffer);
            close(fd);
            const char *error = "ERROR: Upload stream interrupted\n";
            send(client_sock, error, strlen(error), 0);
            return;
        }

        size_t written = 0;
        while (written < (size_t)n) {
            ssize_t w = write(fd, buffer + written, (size_t)n - written);
            if (w <= 0) {
                free(buffer);
                close(fd);
                const char *error = "ERROR: Write failed\n";
                send(client_sock, error, strlen(error), 0);
                return;
            }
            written += (size_t)w;
        }
        received += (uint64_t)n;
        payload_touch_activity();
    }

    free(buffer);
    close(fd);
    if (chmod_final) {
        chmod(final_path, 0777);
    }

    char ok[128];
    snprintf(ok, sizeof(ok), "OK %llu\n", (unsigned long long)received);
    send_all(client_sock, ok, strlen(ok));
}

void handle_upload_fast_offset_wrapper(int client_sock, const char *args) {
    char dest_root[PATH_MAX];
    char rel_path[PATH_MAX];
    char offset_token[64] = {0};
    char total_token[64] = {0};
    char len_token[64] = {0};
    const char *rest = NULL;
    uint64_t file_offset = 0;
    uint64_t total_size = 0;
    uint64_t chunk_len = 0;
    int chmod_final = 0;

    if (!args) {
        const char *error = "ERROR: Invalid UPLOAD_FAST_OFFSET format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    if (parse_quoted_token(args, dest_root, sizeof(dest_root), &rest) != 0 ||
        parse_quoted_token(rest, rel_path, sizeof(rel_path), &rest) != 0 ||
        parse_quoted_token(rest, offset_token, sizeof(offset_token), &rest) != 0 ||
        parse_quoted_token(rest, total_token, sizeof(total_token), &rest) != 0 ||
        parse_quoted_token(rest, len_token, sizeof(len_token), &rest) != 0) {
        const char *error = "ERROR: Invalid UPLOAD_FAST_OFFSET format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    if (rest && *rest) {
        char token[32] = {0};
        if (parse_quoted_token(rest, token, sizeof(token), &rest) == 0) {
            if (strcasecmp(token, "CHMOD_END") == 0 || strcmp(token, "1") == 0) {
                chmod_final = 1;
            }
        }
    }

    if (!is_path_safe(dest_root) || strstr(rel_path, "..")) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    errno = 0;
    file_offset = (uint64_t)strtoull(offset_token, NULL, 10);
    if (errno != 0) {
        const char *error = "ERROR: Invalid offset\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    errno = 0;
    total_size = (uint64_t)strtoull(total_token, NULL, 10);
    if (errno != 0) {
        const char *error = "ERROR: Invalid total size\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    errno = 0;
    chunk_len = (uint64_t)strtoull(len_token, NULL, 10);
    if (errno != 0) {
        const char *error = "ERROR: Invalid chunk length\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    if (chunk_len == 0 || file_offset > total_size || chunk_len > (total_size - file_offset)) {
        const char *error = "ERROR: Invalid chunk range\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    char final_path[PATH_MAX];
    if (snprintf(final_path, sizeof(final_path), "%s/%s", dest_root, rel_path) >= (int)sizeof(final_path)) {
        const char *error = "ERROR: Destination path too long\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    char parent_path[PATH_MAX];
    strncpy(parent_path, final_path, sizeof(parent_path) - 1);
    parent_path[sizeof(parent_path) - 1] = '\0';
    char *slash = strrchr(parent_path, '/');
    if (slash && slash != parent_path) {
        *slash = '\0';
        char mkerr[128] = {0};
        if (mkdir_p(parent_path, 0777, mkerr, sizeof(mkerr)) != 0) {
            char error[256];
            snprintf(error, sizeof(error), "ERROR: %s\n", mkerr[0] ? mkerr : "mkdir failed");
            send(client_sock, error, strlen(error), 0);
            return;
        }
    }

    int fd = open(final_path, O_WRONLY | O_CREAT, chmod_final ? 0777 : 0666);
    if (fd < 0) {
        char error[256];
        snprintf(error, sizeof(error), "ERROR: open failed: %s\n", strerror(errno));
        send(client_sock, error, strlen(error), 0);
        return;
    }
    if (ftruncate(fd, (off_t)total_size) != 0) {
        close(fd);
        char error[256];
        snprintf(error, sizeof(error), "ERROR: ftruncate failed: %s\n", strerror(errno));
        send(client_sock, error, strlen(error), 0);
        return;
    }

    const char *ready = "READY\n";
    if (send_all(client_sock, ready, strlen(ready)) != 0) {
        close(fd);
        return;
    }

    int upload_buf = UPLOAD_RCVBUF_SIZE;
    setsockopt(client_sock, SOL_SOCKET, SO_RCVBUF, &upload_buf, sizeof(upload_buf));

    // Keep the userspace staging buffer modest to reduce memory pressure.
    size_t buf_size = (size_t)BUFFER_SIZE;
    uint8_t *buffer = (uint8_t *)malloc(buf_size);
    if (!buffer) {
        close(fd);
        const char *error = "ERROR: Out of memory\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    uint64_t received = 0;
    while (received < chunk_len) {
        size_t want = (size_t)((chunk_len - received) > (uint64_t)buf_size ? (uint64_t)buf_size : (chunk_len - received));
        ssize_t n = recv(client_sock, buffer, want, 0);
        if (n <= 0) {
            free(buffer);
            close(fd);
            const char *error = "ERROR: Upload stream interrupted\n";
            send(client_sock, error, strlen(error), 0);
            return;
        }

        size_t written = 0;
        while (written < (size_t)n) {
            ssize_t w = pwrite(fd, buffer + written, (size_t)n - written, (off_t)(file_offset + received + written));
            if (w <= 0) {
                free(buffer);
                close(fd);
                const char *error = "ERROR: Write failed\n";
                send(client_sock, error, strlen(error), 0);
                return;
            }
            written += (size_t)w;
        }

        received += (uint64_t)n;
        payload_touch_activity();
    }

    free(buffer);
    close(fd);
    if (chmod_final) {
        chmod(final_path, 0777);
    }

    char ok[128];
    snprintf(ok, sizeof(ok), "OK %llu\n", (unsigned long long)received);
    send_all(client_sock, ok, strlen(ok));
}

void handle_payload_status(int client_sock) {
    char *json = extract_queue_get_status_json();
    if (!json) {
        const char *error = "ERROR: Failed to get status\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    char header[64];
    size_t json_len = strlen(json);
    int hdr_len = snprintf(header, sizeof(header), "STATUS %zu\n", json_len);
    send_all(client_sock, header, (size_t)hdr_len);
    send_all(client_sock, json, json_len);
    send_all(client_sock, "\n", 1);

    free(json);
}

void handle_payload_reset(int client_sock) {
    transfer_request_abort_with_reason("payload_reset");
    transfer_cleanup();
    extract_queue_reset();
    const char *ok = "OK\n";
    send_all(client_sock, ok, strlen(ok));
}

void handle_queue_extract(int client_sock, const char *args) {
    char buffer[PATH_MAX * 2];
    strncpy(buffer, args, sizeof(buffer) - 1);
    buffer[sizeof(buffer) - 1] = '\0';
    trim_newline(buffer);

    char *sep = strchr(buffer, '\t');
    if (!sep) {
        const char *error = "ERROR: Invalid QUEUE_EXTRACT format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    *sep = '\0';
    const char *src = buffer;
    char *dst = sep + 1;
    char *cleanup = NULL;
    char *flags = NULL;

    char *sep2 = strchr(dst, '\t');
    if (sep2) {
        *sep2 = '\0';
        cleanup = sep2 + 1;
        char *sep3 = strchr(cleanup, '\t');
        if (sep3) {
            *sep3 = '\0';
            flags = sep3 + 1;
        }
        if (cleanup && cleanup[0] == '\0') {
            cleanup = NULL;
        }
    }
    if (!*src || !*dst) {
        const char *error = "ERROR: Invalid QUEUE_EXTRACT format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    if (!is_path_safe(src) || !is_path_safe(dst)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    if (cleanup && !is_path_safe(cleanup)) {
        const char *error = "ERROR: Invalid cleanup path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    int delete_source = 0;
    int unrar_mode = EXTRACT_RAR_TURBO;
    if (flags && flags[0]) {
        for (char *p = flags; *p; p++) {
            if (*p >= 'a' && *p <= 'z') {
                *p = (char)(*p - ('a' - 'A'));
            }
        }
        if (strstr(flags, "DEL") || strstr(flags, "DELETE")) {
            delete_source = 1;
        }
    }

    int id = extract_queue_add(src, dst, delete_source, cleanup, unrar_mode);
    if (id == -2) {
        const char *error = "ERROR: Duplicate extraction request\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    if (id < 0) {
        const char *error = "ERROR: Queue full\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    /* Start processing if not already running */
    extract_queue_process();

    char response[64];
    snprintf(response, sizeof(response), "OK %d\n", id);
    send(client_sock, response, strlen(response), 0);
}

void handle_queue_cancel(int client_sock, const char *args) {
    char buffer[64];
    strncpy(buffer, args, sizeof(buffer) - 1);
    buffer[sizeof(buffer) - 1] = '\0';
    trim_newline(buffer);

    int id = atoi(buffer);
    if (id <= 0) {
        const char *error = "ERROR: Invalid item ID\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    if (extract_queue_cancel(id) == 0) {
        const char *success = "OK\n";
        send(client_sock, success, strlen(success), 0);
    } else {
        const char *error = "ERROR: Item not found or cannot be cancelled\n";
        send(client_sock, error, strlen(error), 0);
    }
}

void handle_queue_clear(int client_sock) {
    extract_queue_clear_done();
    const char *success = "OK\n";
    send(client_sock, success, strlen(success), 0);
}

void handle_queue_clear_all(int client_sock) {
    extract_queue_clear_all(1);
    const char *success = "OK\n";
    send(client_sock, success, strlen(success), 0);
}

void handle_queue_clear_failed(int client_sock) {
    extract_queue_clear_failed();
    const char *success = "OK\n";
    send(client_sock, success, strlen(success), 0);
}

void handle_queue_reorder(int client_sock, const char *args) {
    char buffer[256];
    strncpy(buffer, args, sizeof(buffer) - 1);
    buffer[sizeof(buffer) - 1] = '\0';
    trim_newline(buffer);

    if (buffer[0] == '\0') {
        const char *error = "ERROR: Invalid reorder list\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    int ids[EXTRACT_QUEUE_MAX_ITEMS];
    int count = 0;
    char *token = strtok(buffer, " ,\t");
    while (token && count < EXTRACT_QUEUE_MAX_ITEMS) {
        int id = atoi(token);
        if (id > 0) {
            ids[count++] = id;
        }
        token = strtok(NULL, " ,\t");
    }

    if (count == 0) {
        const char *error = "ERROR: Invalid reorder list\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    if (extract_queue_reorder(ids, count) == 0) {
        const char *success = "OK\n";
        send(client_sock, success, strlen(success), 0);
    } else {
        const char *error = "ERROR: Reorder failed\n";
        send(client_sock, error, strlen(error), 0);
    }
}

void handle_queue_process(int client_sock) {
    extract_queue_process();
    const char *success = "OK\n";
    send(client_sock, success, strlen(success), 0);
}

void handle_queue_pause(int client_sock, const char *args) {
    char buffer[64];
    strncpy(buffer, args, sizeof(buffer) - 1);
    buffer[sizeof(buffer) - 1] = '\0';
    trim_newline(buffer);

    int id = atoi(buffer);
    if (id <= 0) {
        const char *error = "ERROR: Invalid item ID\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    if (extract_queue_pause(id) == 0) {
        const char *success = "OK\n";
        send(client_sock, success, strlen(success), 0);
    } else {
        const char *error = "ERROR: Item not running\n";
        send(client_sock, error, strlen(error), 0);
    }
}

void handle_queue_retry(int client_sock, const char *args) {
    char buffer[64];
    strncpy(buffer, args ? args : "", sizeof(buffer) - 1);
    buffer[sizeof(buffer) - 1] = '\0';
    trim_newline(buffer);

    int id = atoi(buffer);
    if (id <= 0) {
        const char *error = "ERROR: Invalid item ID\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    if (extract_queue_retry(id) == 0) {
        const char *success = "OK\n";
        send(client_sock, success, strlen(success), 0);
    } else {
        const char *error = "ERROR: Item not found or cannot be retried\n";
        send(client_sock, error, strlen(error), 0);
    }
}

void handle_queue_remove(int client_sock, const char *args) {
    int id = atoi(args);
    if (id <= 0) {
        const char *error = "ERROR: Invalid QUEUE_REMOVE format\n";
        send_all(client_sock, error, strlen(error));
        return;
    }
    int res = extract_queue_remove(id);
    if (res == 0) {
        const char *ok = "OK\n";
        send_all(client_sock, ok, strlen(ok));
    } else if (res == -2) {
        const char *error = "ERROR: Cannot remove active/pending item\n";
        send_all(client_sock, error, strlen(error));
    } else {
        const char *error = "ERROR: Queue remove failed\n";
        send_all(client_sock, error, strlen(error));
    }
}

void handle_sync_info(int client_sock) {
    long long upload_rev = get_json_rev(UPLOAD_QUEUE_FILE);
    long long history_rev = get_json_rev(HISTORY_FILE);
    time_t upload_ts = get_file_mtime(UPLOAD_QUEUE_FILE);
    time_t history_ts = get_file_mtime(HISTORY_FILE);
    time_t extract_ts = extract_queue_get_updated_at();

    char json[256];
    int len = snprintf(json, sizeof(json),
        "{\"upload_queue_rev\":%lld,\"history_rev\":%lld,\"upload_queue_updated_at\":%ld,\"history_updated_at\":%ld,\"extract_queue_updated_at\":%ld}",
        upload_rev, history_rev, (long)upload_ts, (long)history_ts, (long)extract_ts);
    if (len < 0) {
        const char *err = "ERROR: Sync info failed\n";
        send_all(client_sock, err, strlen(err));
        return;
    }
    char header[64];
    int hdr_len = snprintf(header, sizeof(header), "OK %d\n", len);
    send_all(client_sock, header, (size_t)hdr_len);
    send_all(client_sock, json, (size_t)len);
    send_all(client_sock, "\n", 1);
}

void handle_upload_queue_sync(int client_sock, const char *args) {
    if (!args) {
        const char *err = "ERROR: Invalid payload\n";
        send_all(client_sock, err, strlen(err));
        return;
    }
    long len = strtol(args, NULL, 10);
    if (len <= 0 || len > 50 * 1024 * 1024) {
        const char *err = "ERROR: Invalid payload size\n";
        send_all(client_sock, err, strlen(err));
        return;
    }
    struct timeval tv;
    tv.tv_sec = 5;
    tv.tv_usec = 0;
    setsockopt(client_sock, SOL_SOCKET, SO_RCVTIMEO, (const char *)&tv, sizeof(tv));
    char *buf = malloc((size_t)len + 1);
    if (!buf) {
        const char *err = "ERROR: Out of memory\n";
        send_all(client_sock, err, strlen(err));
        return;
    }
    if (recv_exact(client_sock, buf, (size_t)len) != 0) {
        free(buf);
        const char *err = "ERROR: Payload read failed\n";
        send_all(client_sock, err, strlen(err));
        return;
    }
    buf[len] = '\0';
    pthread_mutex_lock(&g_payload_file_mutex);
    if (write_text_file(UPLOAD_QUEUE_FILE, buf, (size_t)len) != 0) {
        pthread_mutex_unlock(&g_payload_file_mutex);
        free(buf);
        const char *err = "ERROR: Payload write failed\n";
        send_all(client_sock, err, strlen(err));
        return;
    }
    pthread_mutex_unlock(&g_payload_file_mutex);
    free(buf);
    const char *ok = "OK\n";
    send_all(client_sock, ok, strlen(ok));
}

void handle_upload_queue_get(int client_sock) {
    size_t len = 0;
    pthread_mutex_lock(&g_payload_file_mutex);
    char *data = read_text_file(UPLOAD_QUEUE_FILE, &len);
    pthread_mutex_unlock(&g_payload_file_mutex);
    if (!data) {
        const char *ok = "OK 0\n";
        send_all(client_sock, ok, strlen(ok));
        return;
    }
    char header[64];
    int hdr_len = snprintf(header, sizeof(header), "OK %zu\n", len);
    send_all(client_sock, header, (size_t)hdr_len);
    if (len > 0) {
        send_all(client_sock, data, len);
    }
    send_all(client_sock, "\n", 1);
    free(data);
}

void handle_history_sync(int client_sock, const char *args) {
    if (!args) {
        const char *err = "ERROR: Invalid payload\n";
        send_all(client_sock, err, strlen(err));
        return;
    }
    long len = strtol(args, NULL, 10);
    if (len <= 0 || len > 50 * 1024 * 1024) {
        const char *err = "ERROR: Invalid payload size\n";
        send_all(client_sock, err, strlen(err));
        return;
    }
    struct timeval tv;
    tv.tv_sec = 5;
    tv.tv_usec = 0;
    setsockopt(client_sock, SOL_SOCKET, SO_RCVTIMEO, (const char *)&tv, sizeof(tv));
    char *buf = malloc((size_t)len + 1);
    if (!buf) {
        const char *err = "ERROR: Out of memory\n";
        send_all(client_sock, err, strlen(err));
        return;
    }
    if (recv_exact(client_sock, buf, (size_t)len) != 0) {
        free(buf);
        const char *err = "ERROR: Payload read failed\n";
        send_all(client_sock, err, strlen(err));
        return;
    }
    buf[len] = '\0';
    pthread_mutex_lock(&g_payload_file_mutex);
    if (write_text_file(HISTORY_FILE, buf, (size_t)len) != 0) {
        pthread_mutex_unlock(&g_payload_file_mutex);
        free(buf);
        const char *err = "ERROR: Payload write failed\n";
        send_all(client_sock, err, strlen(err));
        return;
    }
    pthread_mutex_unlock(&g_payload_file_mutex);
    free(buf);
    const char *ok = "OK\n";
    send_all(client_sock, ok, strlen(ok));
}

void handle_history_get(int client_sock) {
    size_t len = 0;
    pthread_mutex_lock(&g_payload_file_mutex);
    char *data = read_text_file(HISTORY_FILE, &len);
    pthread_mutex_unlock(&g_payload_file_mutex);
    if (!data) {
        const char *ok = "OK 0\n";
        send_all(client_sock, ok, strlen(ok));
        return;
    }
    char header[64];
    int hdr_len = snprintf(header, sizeof(header), "OK %zu\n", len);
    send_all(client_sock, header, (size_t)hdr_len);
    if (len > 0) {
        send_all(client_sock, data, len);
    }
    send_all(client_sock, "\n", 1);
    free(data);
}
