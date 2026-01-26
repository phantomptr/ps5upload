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



static int remove_recursive(const char *path, char *err, size_t err_len) {
    struct stat st;
    if (lstat(path, &st) != 0) {
        if (errno == ENOENT) {
            return 0;
        }
        snprintf(err, err_len, "lstat %s failed: %s", path, strerror(errno));
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
            if (remove_recursive(child, err, err_len) != 0) {
                closedir(dir);
                return -1;
            }
        }
        closedir(dir);
        if (rmdir(path) != 0) {
            snprintf(err, err_len, "rmdir %s failed: %s", path, strerror(errno));
            return -1;
        }
        return 0;
    }

    if (unlink(path) != 0) {
        snprintf(err, err_len, "unlink %s failed: %s", path, strerror(errno));
        return -1;
    }
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
            ctx->cancelled = 1;
            return;
        }
        ctx->last_send = now;
        ctx->bytes_since_send = 0;
    }
    (void)force;
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

    char buffer[256 * 1024];
    ssize_t n;
    while ((n = read(in_fd, buffer, sizeof(buffer))) > 0) {
        if (ctx && ctx->cancelled) {
            snprintf(err, err_len, "Cancelled");
            close(in_fd);
            close(out_fd);
            return -1;
        }
        ssize_t written = 0;
        while (written < n) {
            ssize_t w = write(out_fd, buffer + written, (size_t)(n - written));
            if (w < 0) {
                snprintf(err, err_len, "write %s failed: %s", dst, strerror(errno));
                close(in_fd);
                close(out_fd);
                return -1;
            }
            written += w;
            copy_progress_add(ctx, (unsigned long long)w);
            if (ctx && ctx->cancelled) {
                snprintf(err, err_len, "Cancelled");
                close(in_fd);
                close(out_fd);
                return -1;
            }
        }
    }
    if (n < 0) {
        snprintf(err, err_len, "read %s failed: %s", src, strerror(errno));
        close(in_fd);
        close(out_fd);
        return -1;
    }

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

struct ExtractProgressCtx {
    int sock;
    time_t last_send;
    unsigned int min_interval_sec;
    char last_filename[PATH_MAX];
    int cancelled;
    time_t last_notify;
    unsigned int notify_interval_sec;
};

static void extract_check_cancel(struct ExtractProgressCtx *ctx) {
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

static int extract_progress(const char *filename, unsigned long long file_size,
                            int files_done, unsigned long long total_processed,
                            unsigned long long total_size, void *user_data) {
    (void)file_size;
    (void)files_done;
    struct ExtractProgressCtx *ctx = (struct ExtractProgressCtx *)user_data;
    if (!ctx) {
        return 0;
    }
    extract_check_cancel(ctx);
    if (ctx->cancelled) {
        return 1;
    }
    time_t now = time(NULL);
    int filename_changed = (strncmp(ctx->last_filename, filename, sizeof(ctx->last_filename)) != 0);
    if (!filename_changed && ctx->min_interval_sec > 0 &&
        now - ctx->last_send < (time_t)ctx->min_interval_sec) {
        return 0;
    }
    if (filename_changed) {
        strncpy(ctx->last_filename, filename, sizeof(ctx->last_filename) - 1);
        ctx->last_filename[sizeof(ctx->last_filename) - 1] = '\0';
    }
    ctx->last_send = now;

    int percent = (total_size > 0) ? (int)((total_processed * 100) / total_size) : 0;
    char msg[PATH_MAX + 128];
    int len = snprintf(msg, sizeof(msg), "EXTRACT_PROGRESS %d %llu %llu %s\n",
                       percent, total_processed, total_size, filename);
    if (len > 0 && send_all(ctx->sock, msg, (size_t)len) != 0) {
        ctx->cancelled = 1;
        return 1;
    }
    (void)percent;
    return 0;
}

static int has_extension(const char *path, const char *ext) {
    const char *dot = strrchr(path, '.');
    if (!dot) {
        return 0;
    }
    return strcasecmp(dot, ext) == 0;
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
    if (send_frame_header(sock, FRAME_PACK, pack->len) != 0) {
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
        if (send_frame_header(sock, FRAME_PACK_LZ4, out_len) != 0) {
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
        if (send_frame_header(sock, FRAME_PACK_ZSTD, out_len) != 0) {
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
        if (send_frame_header(sock, FRAME_PACK_LZMA, out_len) != 0) {
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
    size_t overhead = 2 + path_len + 8;
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
    size_t overhead = 2 + path_len + 8;
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


// Thread arguments for raw file download
struct DownloadRawArgs {
    int sock;
    char path[PATH_MAX];
};

static void *download_raw_thread(void *arg) {
    struct DownloadRawArgs *args = (struct DownloadRawArgs *)arg;
    if (!args) {
        return NULL;
    }

    int sock = args->sock;
    char *path = args->path;

    payload_log("[DOWNLOAD_RAW] Thread started for %s", path);

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
        free(args);
        return NULL;
    }

    struct stat st;
    if (fstat(fd, &st) != 0 || !S_ISREG(st.st_mode)) {
        const char *error = "ERROR: Not a file\n";
        send(sock, error, strlen(error), 0);
        close(fd);
        close(sock);
        free(args);
        return NULL;
    }

    char ready[96];
    snprintf(ready, sizeof(ready), "READY %llu\n", (unsigned long long)st.st_size);
    if (send_all(sock, ready, strlen(ready)) != 0) {
        close(fd);
        close(sock);
        free(args);
        return NULL;
    }
    payload_touch_activity();

    uint8_t buffer[64 * 1024];
    ssize_t n;
    while ((n = read(fd, buffer, sizeof(buffer))) > 0) {
        if (send_all(sock, buffer, (size_t)n) != 0) {
            payload_log("[DOWNLOAD_RAW] Send failed for %s", path);
            close(fd);
            close(sock);
            free(args);
            return NULL;
        }
        payload_touch_activity();
    }

    close(fd);
    payload_log("[DOWNLOAD_RAW] Complete %s", path);
    close(sock);
    free(args);
    return NULL;
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

    // Allocate args for thread
    struct DownloadRawArgs *args = malloc(sizeof(struct DownloadRawArgs));
    if (!args) {
        const char *error = "ERROR: Out of memory\n";
        send(client_sock, error, strlen(error), 0);
        close(client_sock);
        return;
    }

    args->sock = client_sock;
    strncpy(args->path, path, sizeof(args->path) - 1);
    args->path[sizeof(args->path) - 1] = '\0';

    // Create detached thread for download
    pthread_t tid;
    if (pthread_create(&tid, NULL, download_raw_thread, args) != 0) {
        const char *error = "ERROR: Failed to start download\n";
        send(client_sock, error, strlen(error), 0);
        close(client_sock);
        free(args);
        return;
    }
    pthread_detach(tid);

    // Return immediately - thread owns the socket now
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
        return -1;
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
            closedir(dir);
            return -1;
        }
        if (S_ISLNK(st.st_mode)) {
            continue;
        }
        if (S_ISDIR(st.st_mode)) {
            if (send_dir_recursive(sock, pack, base_path, child_rel) != 0) {
                closedir(dir);
                return -1;
            }
        } else if (S_ISREG(st.st_mode)) {
            payload_log("[DOWNLOAD_DIR] Sending %s", child_rel);
            if (send_file_records(sock, pack, child_rel, child_path) != 0) {
                closedir(dir);
                return -1;
            }
        }
    }

    closedir(dir);
    return 0;
}

static int calc_dir_size(const char *path, uint64_t *total) {
    DIR *dir = opendir(path);
    if (!dir) {
        return -1;
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
            closedir(dir);
            return -1;
        }
        if (S_ISLNK(st.st_mode)) {
            continue;
        }
        if (S_ISDIR(st.st_mode)) {
            if (calc_dir_size(child, total) != 0) {
                closedir(dir);
                return -1;
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
    strncpy(path, path_arg, PATH_MAX-1);
    path[PATH_MAX-1] = '\0';
    
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
    strncpy(path, path_arg, PATH_MAX-1);
    path[PATH_MAX-1] = '\0';

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

    pthread_t tid;
    if (pthread_create(&tid, NULL, delete_worker, task) != 0) {
        free(task);
        const char *error = "ERROR: Delete thread failed\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    pthread_detach(tid);

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

    if (rename(src, dst) == 0) {
        const char *success = "OK\n";
        send(client_sock, success, strlen(success), 0);
        return;
    }

    if (errno == EXDEV) {
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
            return;
        }
        progress.total = total_size;
        copy_progress_send(&progress, 1);
        if (copy_recursive(src, dst, err, sizeof(err), &progress) != 0) {
            char error_msg[320];
            snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", err);
            send(client_sock, error_msg, strlen(error_msg), 0);
            return;
        }
        copy_progress_send(&progress, 1);
        if (remove_recursive(src, err, sizeof(err)) != 0) {
            char error_msg[320];
            snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", err);
            send(client_sock, error_msg, strlen(error_msg), 0);
            return;
        }
        const char *success = "OK\n";
        send(client_sock, success, strlen(success), 0);
        return;
    }

    {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: rename failed: %s\n", strerror(errno));
        send(client_sock, error_msg, strlen(error_msg), 0);
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
        return;
    }
    progress.total = total_size;
    copy_progress_send(&progress, 1);
    if (copy_recursive(src, dst, err, sizeof(err), &progress) != 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", err);
        send(client_sock, error_msg, strlen(error_msg), 0);
        return;
    }
    copy_progress_send(&progress, 1);

    const char *success = "OK\n";
    send(client_sock, success, strlen(success), 0);
}

void handle_extract_archive(int client_sock, const char *args) {
    char buffer[PATH_MAX * 2];
    strncpy(buffer, args, sizeof(buffer) - 1);
    buffer[sizeof(buffer) - 1] = '\0';
    trim_newline(buffer);

    char *sep = strchr(buffer, '\t');
    if (!sep) {
        const char *error = "ERROR: Invalid EXTRACT_ARCHIVE format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    *sep = '\0';
    const char *src = buffer;
    const char *dst = sep + 1;
    if (!*src || !*dst) {
        const char *error = "ERROR: Invalid EXTRACT_ARCHIVE format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    if (!is_path_safe(src) || !is_path_safe(dst)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    if (mkdir(dst, 0777) != 0 && errno != EEXIST) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: mkdir %s failed: %s\n", dst, strerror(errno));
        send(client_sock, error_msg, strlen(error_msg), 0);
        return;
    }

    struct ExtractProgressCtx progress;
    memset(&progress, 0, sizeof(progress));
    progress.sock = client_sock;
    progress.min_interval_sec = UNRAR_FAST_PROGRESS_INTERVAL_SEC;
    progress.last_send = time(NULL);
    progress.notify_interval_sec = 10;
    progress.last_notify = 0;

    if (!has_extension(src, ".rar")) {
        const char *error = "ERROR: Unsupported archive type\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    int count = 0;
    unsigned long long size = 0;
    int scan_result = unrar_scan(src, &count, &size, NULL, 0);
    if (scan_result != UNRAR_OK) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", unrar_strerror(scan_result));
        send(client_sock, error_msg, strlen(error_msg), 0);
        return;
    }

    unrar_extract_opts opts;
    opts.keepalive_interval_sec = UNRAR_FAST_KEEPALIVE_SEC;
    opts.sleep_every_bytes = UNRAR_FAST_SLEEP_EVERY_BYTES;
    opts.sleep_us = UNRAR_FAST_SLEEP_US;

    int extracted_count = 0;
    unsigned long long total_bytes = 0;
    int extract_result = unrar_extract(src, dst, 0, size, &opts, extract_progress, &progress,
                                       &extracted_count, &total_bytes);
    if (extract_result != UNRAR_OK) {
        const char *err = progress.cancelled ? "ERROR: Cancelled\n" : unrar_strerror(extract_result);
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", err);
        send(client_sock, error_msg, strlen(error_msg), 0);
        return;
    }

    char chmod_err[256] = {0};
    if (chmod_recursive(dst, 0777, chmod_err, sizeof(chmod_err)) != 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", chmod_err);
        send(client_sock, error_msg, strlen(error_msg), 0);
        return;
    }

    const char *success = "OK\n";
    send(client_sock, success, strlen(success), 0);
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

    FILE *fp = fopen(path, "rb");
    if (!fp) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: open failed: %s\n", strerror(errno));
        send(client_sock, error_msg, strlen(error_msg), 0);
        return;
    }

    char header[128];
    snprintf(header, sizeof(header), "OK %lld\n", (long long)st.st_size);
    if (send_all(client_sock, header, strlen(header)) != 0) {
        fclose(fp);
        return;
    }
    printf("[DOWNLOAD] OK size=%lld\n", (long long)st.st_size);
    payload_touch_activity();

    char buffer[64 * 1024];
    size_t n;
    while ((n = fread(buffer, 1, sizeof(buffer), fp)) > 0) {
        if (send_all(client_sock, buffer, n) != 0) {
            printf("[DOWNLOAD] send failed (%s)\n", strerror(errno));
            break;
        }
        payload_touch_activity();
    }

    fclose(fp);
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

    payload_log("[DOWNLOAD_DIR] Streaming %s", path);
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
    uint8_t buffer[64 * 1024];
    size_t n = 0;
    while ((n = fread(buffer, 1, sizeof(buffer), fp)) > 0) {
        sha256_update(&ctx, buffer, n);
    }
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

void handle_clear_tmp(int client_sock) {
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

    int cleared = 0;
    int errors = 0;
    char last_err[256] = {0};

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
            errors++;
            if (last_err[0] == '\0') {
                snprintf(last_err, sizeof(last_err), "%s", err);
            }
            continue;
        }
        mkdir(tmp_path, 0777);
        chmod(tmp_path, 0777);
        cleared++;
    }

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

void handle_upload_v2_wrapper(int client_sock, const char *args) {
    char dest_path[PATH_MAX];
    char mode[16] = {0};
    // Parse: UPLOAD_V2 <dest_path> [TEMP|DIRECT]
    const char *rest = NULL;
    if (!args) {
        const char *error = "ERROR: Invalid UPLOAD_V2 format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    // Manual token parse to avoid overflow
    while (*args == ' ') args++;
    const char *end = args;
    while (*end != '\0' && *end != ' ') end++;
    size_t len = (size_t)(end - args);
    if (len == 0 || len >= sizeof(dest_path)) {
        const char *error = "ERROR: Invalid UPLOAD_V2 format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    memcpy(dest_path, args, len);
    dest_path[len] = '\0';
    while (*end == ' ') end++;
    rest = end;
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
        send(client_sock, error, strlen(error), 0);
        return;
    }

    int use_temp = 1;
    if (mode[0]) {
        if (strcasecmp(mode, "TEMP") == 0) {
            use_temp = 1;
        } else if (strcasecmp(mode, "DIRECT") == 0) {
            use_temp = 0;
        } else {
            const char *error = "ERROR: Invalid UPLOAD_V2 mode\n";
            send(client_sock, error, strlen(error), 0);
            return;
        }
    }

    handle_upload_v2(client_sock, dest_path, use_temp);
}

void handle_upload(int client_sock, const char *args) {
    char dest_path[PATH_MAX];

    printf("[UPLOAD] Received UPLOAD command with args: %s\n", args);

    // Parse: UPLOAD <dest_path> (total_size is ignored, we'll count as we receive)
    if (!args) {
        printf("[UPLOAD] ERROR: Failed to parse command arguments\n");
        const char *error = "ERROR: Invalid UPLOAD command format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    // Safe parse of first token
    while (*args == ' ') args++;
    const char *end = args;
    while (*end != '\0' && *end != ' ') end++;
    size_t len = (size_t)(end - args);
    if (len == 0 || len >= sizeof(dest_path)) {
        printf("[UPLOAD] ERROR: Failed to parse command arguments\n");
        const char *error = "ERROR: Invalid UPLOAD command format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    memcpy(dest_path, args, len);
    dest_path[len] = '\0';

    if (!is_path_safe(dest_path)) {
        const char *error = "ERROR: Invalid path\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    printf("[UPLOAD] Destination: %s\n", dest_path);

    // Log request to file
    time_t now = time(NULL);
    struct tm *tm_info = localtime(&now);
    char log_filename[512];
    snprintf(log_filename, sizeof(log_filename),
             "/data/ps5upload/requests/ps5upload_request_%04d%02d%02d_%02d%02d%02d.txt",
             tm_info->tm_year + 1900, tm_info->tm_mon + 1, tm_info->tm_mday,
             tm_info->tm_hour, tm_info->tm_min, tm_info->tm_sec);

    FILE *log_fp = fopen(log_filename, "w");
    if(log_fp) {
        fprintf(log_fp, "=== PS5 Upload Request ===\n");
        fprintf(log_fp, "Timestamp: %s", asctime(tm_info));
        fprintf(log_fp, "Destination: %s\n", dest_path);
        fclose(log_fp);
    }

    printf("[UPLOAD] Using root vnode for filesystem access\n");

    // Check if writable
    printf("[UPLOAD] Checking write access to destination parent\n");
    // We don't create the directory yet - receive_folder_stream will do that

    // Send ready signal
    printf("[UPLOAD] Sending READY signal to client\n");
    const char *ready = "READY\n";
    send(client_sock, ready, strlen(ready), 0);

    // Receive folder stream directly (no tar/compression)
    printf("[UPLOAD] Starting direct file stream reception...\n");
    time_t start_time = time(NULL);
    char extract_err[256] = {0};
    long long total_bytes = 0;
    int file_count = 0;

    int result = receive_folder_stream(client_sock, dest_path, extract_err, sizeof(extract_err),
                                       &total_bytes, &file_count);

    time_t end_time = time(NULL);
    double elapsed_secs = difftime(end_time, start_time);


    // Update request log with results
    log_fp = fopen(log_filename, "a");
    if(log_fp) {
        fprintf(log_fp, "\n=== Upload Results ===\n");
        fprintf(log_fp, "Status: %s\n", (result == 0) ? "SUCCESS" : "FAILED");
        fprintf(log_fp, "Files transferred: %d\n", file_count);
        fprintf(log_fp, "Total bytes: %lld (%.2f MB)\n", total_bytes, total_bytes / (1024.0 * 1024.0));
        fprintf(log_fp, "Duration: %.1f seconds\n", elapsed_secs);
        if(elapsed_secs > 0) {
            double speed_mbps = (total_bytes / (1024.0 * 1024.0)) / elapsed_secs;
            fprintf(log_fp, "Average speed: %.2f MB/s\n", speed_mbps);
        }
        if(result != 0 && extract_err[0] != '\0') {
            fprintf(log_fp, "Error: %s\n", extract_err);
        }
        fclose(log_fp);
    }

    if(result == 0) {
        printf("[UPLOAD] SUCCESS: %d files, %.2f MB, %.1f seconds\n",
               file_count, total_bytes / (1024.0 * 1024.0), elapsed_secs);

        // Send success with metrics
        char success_msg[256];
        snprintf(success_msg, sizeof(success_msg), "SUCCESS %d %lld\n", file_count, total_bytes);
        send(client_sock, success_msg, strlen(success_msg), 0);

    } else {
        printf("[UPLOAD] ERROR: Upload failed\n");
        char error_msg[320];
        if(extract_err[0] != '\0') {
            printf("[UPLOAD] Error details: %s\n", extract_err);
            snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", extract_err);
        } else {
            snprintf(error_msg, sizeof(error_msg), "ERROR: Upload failed\n");
        }
        send(client_sock, error_msg, strlen(error_msg), 0);

    }
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
    transfer_request_abort();
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
    const char *dst = sep + 1;
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

    int id = extract_queue_add(src, dst, 0, NULL);
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
