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
#include <dirent.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <limits.h>

#include <ps5/kernel.h>

#include "storage.h"
#include "config.h"

// Check if a directory exists and is non-empty
static int is_dir_non_empty(const char *path) {
    DIR *dir = opendir(path);
    if (!dir) {
        return 0; // Doesn't exist or can't open
    }

    struct dirent *entry;
    int has_files = 0;

    while ((entry = readdir(dir)) != NULL) {
        // Skip . and ..
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
            continue;
        }
        has_files = 1;
        break;
    }

    closedir(dir);
    return has_files;
}

void handle_list_storage(int client_sock) {
    pid_t pid = getpid();
    intptr_t old_root = kernel_get_proc_rootdir(pid);

    // Escape sandbox to see full filesystem
    kernel_set_proc_rootdir(pid, kernel_get_root_vnode());

    printf("[STORAGE] Scanning PS5 mount points...\n");

    // Define specific PS5 mount points to scan
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

    char response[8192];
    int offset = 0;

    offset += sprintf(response, "[\n");

    for (int i = 0; mount_points[i] != NULL; i++) {
        const char *path = mount_points[i];

        printf("[STORAGE] Checking %s...\n", path);

        // Check if path exists and is a directory
        struct stat st;
        if (stat(path, &st) != 0 || !S_ISDIR(st.st_mode)) {
            printf("[STORAGE] %s: Not found or not a directory\n", path);
            continue;
        }

        // Skip access check - we are root/escalated
        /*
        if (access(path, W_OK) != 0) {
            printf("[STORAGE] %s: Not writable\n", path);
            continue;
        }
        */

        // Only check if non-empty for USB drives to avoid listing unmounted ports
        if (strncmp(path, "/mnt/usb", 8) == 0) {
            if (!is_dir_non_empty(path)) {
                printf("[STORAGE] %s: Empty directory (unmounted USB), skipping\n", path);
                continue;
            }
        }

        // Try to write a test file to verify writability
        char test_file[PATH_MAX];
        snprintf(test_file, PATH_MAX, "%s/ps5upload_test_safe_to_delete", path);
        FILE *tf = fopen(test_file, "w");
        if (tf) {
            fprintf(tf, "test");
            fclose(tf);
            unlink(test_file);
        } else {
             printf("[STORAGE] %s: Not writable (fopen failed)\n", path);
             continue;
        }

        // Get filesystem stats
        struct statfs fs;
        double free_gb = 0.0;
        double total_gb = 0.0;
        const char *fs_type = "unknown";

        if (statfs(path, &fs) == 0) {
            unsigned long long free_bytes =
                (unsigned long long)fs.f_bavail * fs.f_bsize;
            free_gb = free_bytes / (1024.0 * 1024.0 * 1024.0);

            unsigned long long total_bytes =
                (unsigned long long)fs.f_blocks * fs.f_bsize;
            total_gb = total_bytes / (1024.0 * 1024.0 * 1024.0);

            fs_type = fs.f_fstypename;
        }

        printf("[STORAGE] %s: Valid mount point (Free: %.2f GB / Total: %.2f GB)\n", path, free_gb, total_gb);

        offset += sprintf(response + offset,
            "  {\"path\":\"%s\",\"type\":\"%s\",\"free_gb\":%.1f,\"total_gb\":%.1f},\n",
            path,
            fs_type,
            free_gb,
            total_gb);
    }

    // Remove trailing comma if we added entries
    if (offset > 2 && response[offset-2] == ',') {
        offset -= 2;
        offset += sprintf(response + offset, "\n");
    }

    offset += sprintf(response + offset, "]\n");

    printf("[STORAGE] Scan complete, found storage locations\n");

    // Restore sandbox
    kernel_set_proc_rootdir(pid, old_root);

    send(client_sock, response, offset, 0);
}

void handle_list_dir(int client_sock, const char *path_arg) {
    char path[PATH_MAX];
    strncpy(path, path_arg, PATH_MAX-1);
    path[PATH_MAX-1] = '\0';

    // Remove trailing newline if present
    size_t len = strlen(path);
    if(len > 0 && path[len-1] == '\n') {
        path[len-1] = '\0';
    }

    pid_t pid = getpid();
    intptr_t old_root = kernel_get_proc_rootdir(pid);
    kernel_set_proc_rootdir(pid, kernel_get_root_vnode());

    DIR *dir = opendir(path);
    if(!dir) {
        const char *error = "ERROR: Cannot open directory\n";
        send(client_sock, error, strlen(error), 0);
        kernel_set_proc_rootdir(pid, old_root);
        return;
    }

    char response[8192];
    int offset = 0;
    offset += sprintf(response, "[\n");

    struct dirent *entry;
    while((entry = readdir(dir)) != NULL) {
        if(strcmp(entry->d_name, ".") == 0 ||
           strcmp(entry->d_name, "..") == 0) {
            continue;
        }

        char fullpath[PATH_MAX];
        snprintf(fullpath, PATH_MAX, "%s/%s", path, entry->d_name);

        struct stat st;
        if(stat(fullpath, &st) == 0) {
            const char *type = S_ISDIR(st.st_mode) ? "dir" : "file";
            offset += sprintf(response + offset,
                "  {\"name\":\"%s\",\"type\":\"%s\",\"size\":%ld},\n",
                entry->d_name, type, (long)st.st_size);
        }
    }

    closedir(dir);

    // Remove trailing comma
    if(offset > 2 && response[offset-2] == ',') {
        offset -= 2;
        offset += sprintf(response + offset, "\n");
    }

    offset += sprintf(response + offset, "]\n");

    kernel_set_proc_rootdir(pid, old_root);

    send(client_sock, response, offset, 0);
}
