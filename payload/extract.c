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

#include "extract.h"
#include "notify.h"
#include "config.h"

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
                          long long *out_total_bytes, int *out_file_count) {
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

    char buffer[BUFFER_SIZE];
    char line_buffer[PATH_MAX + 256];
    long long total_bytes = 0;
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
            return -1;
        }

        // Check for completion
        if (strcmp(line_buffer, "DONE") == 0) {
            printf("[EXTRACT] Received DONE signal\n");
            break;
        }

        // Parse FILE command: "FILE <path> <size>"
        char rel_path[PATH_MAX];
        long long file_size;
        if (sscanf(line_buffer, "FILE %s %lld", rel_path, &file_size) != 2) {
            printf("[EXTRACT] ERROR: Invalid file header: %s\n", line_buffer);
            if (err && err_len > 0) {
                snprintf(err, err_len, "invalid file header");
            }
            return -1;
        }

        // Build full path
        char full_path[PATH_MAX];
        snprintf(full_path, sizeof(full_path), "%s/%s", dest_path, rel_path);

        printf("[EXTRACT] Receiving file: %s (%lld bytes)\n", rel_path, file_size);

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
                return -1;
            }
        }

        // Open file for writing
        FILE *fp = fopen(full_path, "wb");
        if (!fp) {
            printf("[EXTRACT] ERROR: Failed to open file %s: %s\n", full_path, strerror(errno));
            if (err && err_len > 0) {
                snprintf(err, err_len, "fopen failed: %s", strerror(errno));
            }
            return -1;
        }

        // Receive file data
        long long remaining = file_size;
        while (remaining > 0) {
            size_t to_recv = (remaining < BUFFER_SIZE) ? remaining : BUFFER_SIZE;
            if (recv_exact(sock, buffer, to_recv) != 0) {
                printf("[EXTRACT] ERROR: Failed to receive file data\n");
                fclose(fp);
                if (err && err_len > 0) {
                    snprintf(err, err_len, "recv failed");
                }
                return -1;
            }

            if (fwrite(buffer, 1, to_recv, fp) != to_recv) {
                printf("[EXTRACT] ERROR: Failed to write file data\n");
                fclose(fp);
                if (err && err_len > 0) {
                    snprintf(err, err_len, "write failed");
                }
                return -1;
            }

            remaining -= to_recv;
            total_bytes += to_recv;

        }

        fclose(fp);
        chmod(full_path, 0777);
        file_count++;

        printf("[EXTRACT] File complete: %s\n", rel_path);
    }

    printf("[EXTRACT] Upload complete: %d files, %.2f MB total\n",
           file_count, total_bytes / (1024.0 * 1024.0));

    if (out_total_bytes) *out_total_bytes = total_bytes;
    if (out_file_count) *out_file_count = file_count;

    return 0;
}
