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
#include <errno.h>
#include <time.h>
#include <limits.h>
#include <pthread.h>
#include <strings.h>
#include <dirent.h> // Added for DIR, opendir, readdir, closedir
#include <sys/types.h> // Added for types used by dirent.h

#include "extract.h"
#include "notify.h"
#include "config.h"
#include "protocol_defs.h"
#include "third_party/unrar/unrar_wrapper.h"

// Helper to send exact number of bytes
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

// Helper to check for file extension
static int has_extension(const char *path, const char *ext) {
    const char *dot = strrchr(path, '.');
    if (!dot) {
        return 0;
    }
    return strcasecmp(dot, ext) == 0;
}

// Helper for recursive chmod
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


// --- Threaded Extraction Logic ---

struct ExtractProgressCtx {
    int sock;
    time_t last_send;
    unsigned int min_interval_sec;
    char last_filename[PATH_MAX];
    int cancelled;
    time_t last_notify;
    unsigned int notify_interval_sec;
};

struct extract_thread_args {
    char src[PATH_MAX];
    char dst[PATH_MAX];
    int client_sock;
};

static void extract_check_cancel(struct ExtractProgressCtx *ctx) {
    if (!ctx || ctx->sock < 0 || ctx->cancelled) {
        return;
    }
    char buf[64];
    ssize_t n = recv(ctx->sock, buf, sizeof(buf) - 1, MSG_DONTWAIT);
    if (n == 0) { // Connection closed by peer
        ctx->cancelled = 1;
        return;
    }
    if (n < 0) {
        if (errno == EWOULDBLOCK || errno == EAGAIN || errno == EINTR) {
            return; // No data, not an error
        }
        ctx->cancelled = 1; // Any other error, assume cancelled
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


static void* extract_thread_main(void *arg) {
    struct extract_thread_args* args = (struct extract_thread_args*)arg;
    if (!args) {
        return NULL;
    }
    
    // The socket is now owned by this thread.
    int sock = args->client_sock;

    if (mkdir(args->dst, 0777) != 0 && errno != EEXIST) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: mkdir %s failed: %s\n", args->dst, strerror(errno));
        send_all(sock, error_msg, strlen(error_msg));
        goto cleanup;
    }

    struct ExtractProgressCtx progress;
    memset(&progress, 0, sizeof(progress));
    progress.sock = sock;
    progress.min_interval_sec = UNRAR_TURBO_PROGRESS_INTERVAL_SEC;
    progress.last_send = time(NULL);
    progress.notify_interval_sec = 10;
    progress.last_notify = 0;

    if (!has_extension(args->src, ".rar")) {
        const char *error = "ERROR: Unsupported archive type\n";
        send_all(sock, error, strlen(error));
        goto cleanup;
    }

    unrar_extract_opts opts;
    opts.keepalive_interval_sec = UNRAR_TURBO_KEEPALIVE_SEC;
    opts.sleep_every_bytes = UNRAR_TURBO_SLEEP_EVERY_BYTES;
    opts.sleep_us = UNRAR_TURBO_SLEEP_US;
    opts.trust_paths = UNRAR_TURBO_TRUST_PATHS;
    opts.progress_file_start = UNRAR_TURBO_PROGRESS_FILE_START;

    int extracted_count = 0;
    unsigned long long total_bytes = 0;
    int extract_result = unrar_extract(args->src, args->dst, 0, 0, &opts, extract_progress, &progress,
                                       &extracted_count, &total_bytes);

    if (extract_result != UNRAR_OK) {
        const char *err_str = progress.cancelled ? "Cancelled" : unrar_strerror(extract_result);
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", err_str);
        send_all(sock, error_msg, strlen(error_msg));
        goto cleanup;
    }

    char chmod_err[256] = {0};
    if (chmod_recursive(args->dst, 0777, chmod_err, sizeof(chmod_err)) != 0) {
        char error_msg[320];
        snprintf(error_msg, sizeof(error_msg), "ERROR: %s\n", chmod_err);
        send_all(sock, error_msg, strlen(error_msg));
        goto cleanup;
    }

    const char *success = "OK\n";
    send_all(sock, success, strlen(success));

cleanup:
    close(sock);
    free(args);
    return NULL;
}

int start_threaded_extraction(int client_sock, const char *src, const char *dst) {
    struct extract_thread_args *args = malloc(sizeof(struct extract_thread_args));
    if (!args) {
        const char *error = "ERROR: Out of memory\n";
        send_all(client_sock, error, strlen(error));
        return -1;
    }

    strncpy(args->src, src, sizeof(args->src) - 1);
    args->src[sizeof(args->src) - 1] = '\0';
    strncpy(args->dst, dst, sizeof(args->dst) - 1);
    args->dst[sizeof(args->dst) - 1] = '\0';
    args->client_sock = client_sock;
    
    pthread_t tid;
    if (pthread_create(&tid, NULL, extract_thread_main, args) != 0) {
        free(args);
        const char *error = "ERROR: Failed to create extraction thread\n";
        send_all(client_sock, error, strlen(error));
        return -1;
    }

    pthread_detach(tid);
    return 0; // Success, socket ownership transferred
}


// --- Original code from extract.c ---

// Create directories recursively
static int mkdir_recursive(const char *path) {
    char tmp[PATH_MAX];
    char *p = NULL;
    size_t len;

    snprintf(tmp, sizeof(tmp), "%s", path);
    len = strlen(tmp);
    if (tmp[len - 1] == '/')
        tmp[len - 1] = 0;

    for (p = tmp + 1; *p; p++) {
        if (*p == '/') {
            *p = 0;
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

// Receive exact number of bytes from socket
static int recv_exact(int sock, void *buffer, size_t size) {
    size_t received = 0;
    while (received < size) {
        ssize_t bytes = recv(sock, (char*)buffer + received, size - received, 0);
        if (bytes <= 0) {
            return -1;
        }
        received += bytes;
    }
    return 0;
}

// Receive a line from socket (until \n)
static int recv_line(int sock, char *buffer, size_t max_size) {
    size_t pos = 0;
    while (pos < max_size - 1) {
        char c;
        ssize_t bytes = recv(sock, &c, 1, 0);
        if (bytes <= 0) {
            return -1;
        }
        if (c == '\n') {
            buffer[pos] = '\0';
            return pos;
        }
        buffer[pos++] = c;
    }
    buffer[pos] = '\0';
    return pos;
}

int receive_folder_stream(int sock, const char *dest_path, char *err, size_t err_len,
                          unsigned long long *out_total_bytes, int *out_file_count) {
    if (err && err_len > 0) {
        err[0] = '\0';
    }

    printf("[EXTRACT] Starting direct file stream to: %s\n", dest_path);

    // Create base destination directory
    if (mkdir_recursive(dest_path) != 0) {
        printf("[EXTRACT] ERROR: Failed to create base directory: %s\n", strerror(errno));
        if (err && err_len > 0) {
            snprintf(err, err_len, "mkdir failed: %s", strerror(errno));
        }
        return -1;
    }
    chmod(dest_path, 0777);

    char *buffer = malloc(BUFFER_SIZE);
    if (!buffer) {
        if (err && err_len > 0) {
            snprintf(err, err_len, "oom");
        }
        return -1;
    }
    char line_buffer[PATH_MAX + 256];
    unsigned long long total_bytes = 0;
    int file_count = 0;

    printf("[EXTRACT] Waiting for file stream...\n");

    // Protocol: FILE <relative_path> <size>\n followed by raw data
    //           DONE\n when complete
    while (1) {
        // Read command line
        if (recv_line(sock, line_buffer, sizeof(line_buffer)) < 0) {
            printf("[EXTRACT] ERROR: Connection lost\n");
            if (err && err_len > 0) {
                snprintf(err, err_len, "connection lost");
            }
            goto cleanup_error;
        }

        // Check for completion
        if (strcmp(line_buffer, "DONE") == 0) {
            printf("[EXTRACT] Received DONE signal\n");
            break;
        }

        // Parse FILE command: "FILE <path> <size>"
        char rel_path[PATH_MAX];
        unsigned long long file_size;
        if (sscanf(line_buffer, "FILE %1023s %llu", rel_path, &file_size) != 2) {
            printf("[EXTRACT] ERROR: Invalid file header: %s\n", line_buffer);
            if (err && err_len > 0) {
                snprintf(err, err_len, "invalid file header");
            }
            goto cleanup_error;
        }

        // Build full path
        char full_path[PATH_MAX];
        snprintf(full_path, sizeof(full_path), "%s/%s", dest_path, rel_path);

        printf("[EXTRACT] Receiving file: %s (%llu bytes)\n", rel_path, file_size);

        // Create parent directories
        char dir_path[PATH_MAX];
        snprintf(dir_path, sizeof(dir_path), "%s", full_path);
        char *last_slash = strrchr(dir_path, '/');
        if (last_slash) {
            *last_slash = '\0';
            if (mkdir_recursive(dir_path) != 0) {
                printf("[EXTRACT] ERROR: Failed to create directory: %s\n", strerror(errno));
                if (err && err_len > 0) {
                    snprintf(err, err_len, "mkdir failed: %s", strerror(errno));
                }
                goto cleanup_error;
            }
        }

        // Open file for writing
        FILE *fp = fopen(full_path, "wb");
        if (!fp) {
            printf("[EXTRACT] ERROR: Failed to open file %s: %s\n", full_path, strerror(errno));
            if (err && err_len > 0) {
                snprintf(err, err_len, "fopen failed: %s", strerror(errno));
            }
            goto cleanup_error;
        }
        char *io_buf = malloc(BUFFER_SIZE);
        if (io_buf) {
            setvbuf(fp, io_buf, _IOFBF, BUFFER_SIZE);
        }

        // Receive file data
        unsigned long long remaining = file_size;
        while (remaining > 0) {
            size_t to_recv = (remaining < BUFFER_SIZE) ? (size_t)remaining : BUFFER_SIZE;
            if (recv_exact(sock, buffer, to_recv) != 0) {
                printf("[EXTRACT] ERROR: Failed to receive file data\n");
                fclose(fp);
                free(io_buf);
                if (err && err_len > 0) {
                    snprintf(err, err_len, "recv failed");
                }
                goto cleanup_error;
            }

            if (fwrite(buffer, 1, to_recv, fp) != to_recv) {
                printf("[EXTRACT] ERROR: Failed to write file data\n");
                fclose(fp);
                free(io_buf);
                if (err && err_len > 0) {
                    snprintf(err, err_len, "write failed");
                }
                goto cleanup_error;
            }

            remaining -= (unsigned long long)to_recv;
            total_bytes += (unsigned long long)to_recv;

        }

        fclose(fp);
        free(io_buf);
        chmod(full_path, 0777);
        file_count++;

        printf("[EXTRACT] File complete: %s\n", rel_path);
    }

    printf("[EXTRACT] Upload complete: %d files, %.2f MB total\n",
           file_count, total_bytes / (1024.0 * 1024.0));

    free(buffer);

    if (out_total_bytes) *out_total_bytes = total_bytes;
    if (out_file_count) *out_file_count = file_count;

    return 0;

cleanup_error:
    free(buffer);
    return -1;
}
