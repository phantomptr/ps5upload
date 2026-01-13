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

#include <ps5/kernel.h>

#include "protocol.h"
#include "extract.h"
#include "notify.h"
#include "config.h"
#include "transfer.h"
#include "protocol_defs.h"
#include "lz4.h"
#include "sha256.h"

#define DOWNLOAD_PACK_BUFFER_SIZE (4 * 1024 * 1024)

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

static int copy_file(const char *src, const char *dst, mode_t mode, char *err, size_t err_len) {
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

    char buffer[64 * 1024];
    ssize_t n;
    while ((n = read(in_fd, buffer, sizeof(buffer))) > 0) {
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

static int copy_recursive(const char *src, const char *dst, char *err, size_t err_len) {
    struct stat st;
    if (lstat(src, &st) != 0) {
        snprintf(err, err_len, "lstat %s failed: %s", src, strerror(errno));
        return -1;
    }

    if (S_ISDIR(st.st_mode)) {
        if (mkdir(dst, st.st_mode & 0777) != 0 && errno != EEXIST) {
            snprintf(err, err_len, "mkdir %s failed: %s", dst, strerror(errno));
            return -1;
        }

        DIR *dir = opendir(src);
        if (!dir) {
            snprintf(err, err_len, "opendir %s failed: %s", src, strerror(errno));
            return -1;
        }
        struct dirent *entry;
        while ((entry = readdir(dir)) != NULL) {
            if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
                continue;
            }
            char child_src[PATH_MAX];
            char child_dst[PATH_MAX];
            snprintf(child_src, sizeof(child_src), "%s/%s", src, entry->d_name);
            snprintf(child_dst, sizeof(child_dst), "%s/%s", dst, entry->d_name);
            if (copy_recursive(child_src, child_dst, err, err_len) != 0) {
                closedir(dir);
                return -1;
            }
        }
        closedir(dir);
        return 0;
    }

    if (S_ISREG(st.st_mode)) {
        return copy_file(src, dst, st.st_mode & 0777, err, err_len);
    }

    snprintf(err, err_len, "Unsupported file type: %s", src);
    return -1;
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

struct DownloadPack {
    uint8_t *buf;
    size_t len;
    uint32_t records;
    int compress;
};

static int send_all(int sock, const void *buf, size_t len) {
    const uint8_t *p = (const uint8_t *)buf;
    size_t sent = 0;
    while (sent < len) {
        ssize_t n = send(sock, p + sent, len - sent, 0);
        if (n > 0) {
            sent += (size_t)n;
            continue;
        }
        if (n < 0 && (errno == EWOULDBLOCK || errno == EAGAIN || errno == EINTR)) {
            usleep(1000);
            continue;
        }
        return -1;
    }
    return 0;
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

static int download_pack_init(struct DownloadPack *pack, int compress) {
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

static int download_pack_flush(int sock, struct DownloadPack *pack) {
    if (pack->records == 0) {
        return 0;
    }
    memcpy(pack->buf, &pack->records, 4);
    if (pack->compress) {
        int max_dst = LZ4_compressBound((int)pack->len);
        uint8_t *tmp = malloc((size_t)max_dst + 4);
        if (!tmp) {
            return -1;
        }
        uint32_t raw_len = (uint32_t)pack->len;
        memcpy(tmp, &raw_len, 4);
        int compressed = LZ4_compress_default((const char *)pack->buf, (char *)tmp + 4, (int)pack->len, max_dst);
        if (compressed <= 0) {
            free(tmp);
            return -1;
        }
        size_t out_len = (size_t)compressed + 4;
        if (send_frame_header(sock, FRAME_PACK_LZ4, out_len) != 0) {
            free(tmp);
            return -1;
        }
        if (send_all(sock, tmp, out_len) != 0) {
            free(tmp);
            return -1;
        }
        free(tmp);
    } else {
        if (send_frame_header(sock, FRAME_PACK, pack->len) != 0) {
            return -1;
        }
        if (send_all(sock, pack->buf, pack->len) != 0) {
            return -1;
        }
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

static int send_file_records(int sock, struct DownloadPack *pack, const char *rel_path, const char *path) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }

    size_t path_len = strlen(rel_path);
    size_t overhead = 2 + path_len + 8;
    size_t max_chunk = DOWNLOAD_PACK_BUFFER_SIZE - 4 - overhead;
    if (max_chunk == 0) {
        close(fd);
        return -1;
    }

    uint8_t *buf = malloc(max_chunk);
    if (!buf) {
        close(fd);
        return -1;
    }

    ssize_t n;
    while ((n = read(fd, buf, max_chunk)) > 0) {
        if (download_pack_add_record(sock, pack, rel_path, buf, (uint64_t)n) != 0) {
            free(buf);
            close(fd);
            return -1;
        }
    }

    free(buf);
    close(fd);
    return (n < 0) ? -1 : 0;
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
            printf("[DOWNLOAD_DIR] Sending %s\n", child_rel);
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

    if (rename(src, dst) == 0) {
        const char *success = "OK\n";
        send(client_sock, success, strlen(success), 0);
        return;
    }

    if (errno == EXDEV) {
        char err[256] = {0};
        if (copy_recursive(src, dst, err, sizeof(err)) != 0) {
            char error_msg[320];
            snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", err);
            send(client_sock, error_msg, strlen(error_msg), 0);
            return;
        }
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

    char err[256] = {0};
    if (copy_recursive(src, dst, err, sizeof(err)) != 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", err);
        send(client_sock, error_msg, strlen(error_msg), 0);
        return;
    }

    const char *success = "OK\n";
    send(client_sock, success, strlen(success), 0);
}

void handle_chmod_777(int client_sock, const char *path_arg) {
    char path[PATH_MAX];
    strncpy(path, path_arg, PATH_MAX-1);
    path[PATH_MAX-1] = '\0';
    trim_newline(path);

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

    struct stat st;
    if (stat(path, &st) != 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: stat failed: %s\n", strerror(errno));
        send(client_sock, error_msg, strlen(error_msg), 0);
        return;
    }
    if (!S_ISREG(st.st_mode)) {
        const char *error = "ERROR: Not a file\n";
        send(client_sock, error, strlen(error), 0);
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

    char buffer[64 * 1024];
    size_t n;
    while ((n = fread(buffer, 1, sizeof(buffer), fp)) > 0) {
        if (send_all(client_sock, buffer, n) != 0) {
            break;
        }
    }

    fclose(fp);
}

void handle_download_dir(int client_sock, const char *path_arg) {
    char path[PATH_MAX];
    strncpy(path, path_arg, PATH_MAX-1);
    path[PATH_MAX-1] = '\0';
    trim_newline(path);

    int use_lz4 = 0;
    char *lz4_tag = strstr(path, " LZ4");
    if (lz4_tag) {
        *lz4_tag = '\0';
        use_lz4 = 1;
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

    char ready[64];
    snprintf(ready, sizeof(ready), "READY %llu\n", (unsigned long long)total_size);
    if (send_all(client_sock, ready, strlen(ready)) != 0) {
        return;
    }

    struct DownloadPack pack;
    if (download_pack_init(&pack, use_lz4) != 0) {
        send_error_frame(client_sock, "Download failed: OOM");
        return;
    }

    printf("[DOWNLOAD_DIR] Streaming %s\n", path);
    if (send_dir_recursive(client_sock, &pack, path, NULL) != 0) {
        printf("[DOWNLOAD_DIR] Failed while streaming %s\n", path);
        send_error_frame(client_sock, "Download failed");
        free(pack.buf);
        return;
    }

    if (download_pack_flush(client_sock, &pack) != 0) {
        printf("[DOWNLOAD_DIR] Failed flush for %s\n", path);
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
    printf("[DOWNLOAD_DIR] Completed %s\n", path);
}

void handle_hash_file(int client_sock, const char *path_arg) {
    char path[PATH_MAX];
    strncpy(path, path_arg, PATH_MAX - 1);
    path[PATH_MAX - 1] = '\0';
    trim_newline(path);

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

void handle_upload_v2_wrapper(int client_sock, const char *args) {
    char dest_path[PATH_MAX];
    // Parse: UPLOAD_V2 <dest_path>
    if(sscanf(args, "%s", dest_path) < 1) {
        const char *error = "ERROR: Invalid UPLOAD_V2 format\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    handle_upload_v2(client_sock, dest_path);
}

void handle_upload(int client_sock, const char *args) {
    char dest_path[PATH_MAX];
    char dummy[64]; // For old "total_size" parameter (now ignored)

    printf("[UPLOAD] Received UPLOAD command with args: %s\n", args);

    // Parse: UPLOAD <dest_path> (total_size is ignored, we'll count as we receive)
    if(sscanf(args, "%s %s", dest_path, dummy) < 1) {
        printf("[UPLOAD] ERROR: Failed to parse command arguments\n");
        const char *error = "ERROR: Invalid UPLOAD command format\n";
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

    // Show notification
    char notify_msg[256];
    snprintf(notify_msg, sizeof(notify_msg), "Starting upload to %s", dest_path);
    notify_info("PS5 Upload", notify_msg);

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

        snprintf(notify_msg, sizeof(notify_msg), "Upload complete: %d files, %.1f MB",
                 file_count, total_bytes / (1024.0 * 1024.0));
        notify_success("PS5 Upload", notify_msg);
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

        notify_error("PS5 Upload", "Upload failed");
    }
}
