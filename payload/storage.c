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
#include <errno.h>
#include <unistd.h>
#include <dirent.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <limits.h>
#include <time.h>

#include <ps5/kernel.h>

#include "storage.h"
#include "config.h"

void payload_log(const char *fmt, ...);
void payload_touch_activity(void);
void payload_set_crash_context(const char *op, const char *path, const char *path2);

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
    size_t offset = 0;
    int entries_added = 0;

    int written = snprintf(response + offset, sizeof(response) - offset, "[\n");
    if (written < 0 || (size_t)written >= sizeof(response) - offset) {
        const char *error = "ERROR: Response buffer overflow\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    offset += (size_t)written;

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

        written = snprintf(response + offset, sizeof(response) - offset,
            "  {\"path\":\"%s\",\"type\":\"%s\",\"free_gb\":%.1f,\"total_gb\":%.1f},\n",
            path,
            fs_type,
            free_gb,
            total_gb);
        if (written < 0 || (size_t)written >= sizeof(response) - offset) {
            break;
        }
        offset += (size_t)written;
        entries_added += 1;
    }

    // Remove trailing comma if we added entries
    if (entries_added > 0 && offset > 2 && response[offset - 2] == ',') {
        offset -= 2;
        written = snprintf(response + offset, sizeof(response) - offset, "\n");
        if (written > 0 && (size_t)written < sizeof(response) - offset) {
            offset += (size_t)written;
        }
    }

    written = snprintf(response + offset, sizeof(response) - offset, "]\n");
    if (written < 0 || (size_t)written >= sizeof(response) - offset) {
        const char *error = "ERROR: Response buffer overflow\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    offset += (size_t)written;

    printf("[STORAGE] Scan complete, found storage locations\n");

    send(client_sock, response, offset, 0);
}

void handle_list_dir(int client_sock, const char *path_arg) {
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

    // Remove trailing newline if present
    size_t len = strlen(path);
    if(len > 0 && path[len-1] == '\n') {
        path[len-1] = '\0';
    }
    payload_set_crash_context("LIST_DIR", path, NULL);
    payload_log("[LIST_DIR] Start %s", path);
    payload_touch_activity();

    DIR *dir = opendir(path);
    if(!dir) {
        payload_log("[LIST_DIR] ERROR: open failed %s (%s)", path, strerror(errno));
        const char *error = "ERROR: Cannot open directory\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }

    time_t started_at = time(NULL);
    int listed_count = 0;
    int file_count = 0;
    int dir_count = 0;

    send(client_sock, "[\n", 2, 0);
    int first = 1;
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
            char line[PATH_MAX + 128];
            int written = snprintf(line, sizeof(line),
                "%s  {\"name\":\"%s\",\"type\":\"%s\",\"size\":%ld,\"mtime\":%ld}\n",
                first ? "" : ",",
                entry->d_name, type, (long)st.st_size, (long)st.st_mtime);
            if (written > 0) {
                send(client_sock, line, (size_t)written, 0);
            }
            listed_count++;
            if (S_ISDIR(st.st_mode)) {
                dir_count++;
            } else {
                file_count++;
            }
            if ((listed_count % 128) == 0) {
                payload_touch_activity();
            }
            first = 0;
        }
    }

    closedir(dir);

    send(client_sock, "]\n", 2, 0);
    payload_touch_activity();
    payload_log("[LIST_DIR] Complete %s entries=%d dirs=%d files=%d elapsed=%lds",
        path, listed_count, dir_count, file_count, (long)(time(NULL) - started_at));
}

static int list_dir_recursive_helper(int client_sock, const char *base_path, const char *rel_path,
                                     int *first, int *file_count, int *dir_count, int *error_count) {
    char path[PATH_MAX];
    snprintf(path, sizeof(path), "%s/%s", base_path, rel_path);

    DIR *dir = opendir(path);
    if (!dir) {
        if (error_count) (*error_count)++;
        return -1;
    }

    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
            continue;
        }

        char child_path[PATH_MAX];
        snprintf(child_path, sizeof(child_path), "%s/%s", path, entry->d_name);

        char child_rel_path[PATH_MAX];
        if (strlen(rel_path) > 0) {
            snprintf(child_rel_path, sizeof(child_rel_path), "%s/%s", rel_path, entry->d_name);
        } else {
            snprintf(child_rel_path, sizeof(child_rel_path), "%s", entry->d_name);
        }

        struct stat st;
        if (stat(child_path, &st) == 0) {
            if (S_ISDIR(st.st_mode)) {
                if (dir_count) (*dir_count)++;
                if (list_dir_recursive_helper(client_sock, base_path, child_rel_path, first, file_count, dir_count, error_count) != 0) {
                    // silently ignore directories we can't read
                }
            } else if (S_ISREG(st.st_mode)) {
                char line[PATH_MAX + 128];
                int written = snprintf(line, sizeof(line),
                    "%s  {\"name\":\"%s\",\"type\":\"file\",\"size\":%ld,\"mtime\":%ld}\n",
                    *first ? "" : ",",
                    child_rel_path, (long)st.st_size, (long)st.st_mtime);
                if (written > 0) {
                    send(client_sock, line, (size_t)written, 0);
                }
                if (file_count) {
                    (*file_count)++;
                    if (((*file_count) % 128) == 0) {
                        payload_touch_activity();
                    }
                }
                *first = 0;
            }
        }
    }

    closedir(dir);
    return 0;
}

void handle_list_dir_recursive(int client_sock, const char *path_arg) {
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

    // Remove trailing newline if present
    size_t len = strlen(path);
    if(len > 0 && path[len-1] == '\n') {
        path[len-1] = '\0';
    }
    payload_set_crash_context("LIST_DIR_RECURSIVE", path, NULL);
    payload_log("[LIST_DIR_RECURSIVE] Start %s", path);
    payload_touch_activity();

    DIR *dir = opendir(path);
    if(!dir) {
        payload_log("[LIST_DIR_RECURSIVE] ERROR: open failed %s (%s)", path, strerror(errno));
        const char *error = "ERROR: Cannot open directory\n";
        send(client_sock, error, strlen(error), 0);
        return;
    }
    closedir(dir); // Just used for validation

    send(client_sock, "[\n", 2, 0);
    
    int first = 1;
    int file_count = 0;
    int dir_count = 0;
    int error_count = 0;
    time_t started_at = time(NULL);
    list_dir_recursive_helper(client_sock, path, "", &first, &file_count, &dir_count, &error_count);

    send(client_sock, "]\n", 2, 0);
    payload_touch_activity();
    payload_log("[LIST_DIR_RECURSIVE] Complete %s dirs=%d files=%d errors=%d elapsed=%lds",
        path, dir_count, file_count, error_count, (long)(time(NULL) - started_at));
}
