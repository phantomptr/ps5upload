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

#include <ps5/kernel.h>

#include "protocol.h"
#include "extract.h"
#include "notify.h"
#include "config.h"
#include "transfer.h"

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

void handle_check_dir(int client_sock, const char *path_arg) {
    char path[PATH_MAX];
    strncpy(path, path_arg, PATH_MAX-1);
    path[PATH_MAX-1] = '\0';
    
    // Remove trailing newline
    size_t len = strlen(path);
    if(len > 0 && path[len-1] == '\n') path[len-1] = '\0';

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
    size_t len = strlen(path);
    if(len > 0 && path[len-1] == '\n') {
        path[len-1] = '\0';
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
    size_t len = strlen(path);
    if(len > 0 && path[len-1] == '\n') {
        path[len-1] = '\0';
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
