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

#include "formats.h"
#include "config.h"

// Global array of all possible formats
static Format g_formats[] = {
    {"tar",     ".tar",     "tar -xvf - -C \"%s\"", "", false},
    {"tar.gz",  ".tar.gz",  "tar -xvf - -C \"%s\"", "", false},
    {"tgz",     ".tgz",     "tar -xvf - -C \"%s\"", "", false},
    {"tar.bz2", ".tar.bz2", "tar -xvf - -C \"%s\"", "", false},
    {"tar.xz",  ".tar.xz",  "tar -xvf - -C \"%s\"", "", false},
    {"zip",     ".zip",     "unzip -o - -d \"%s\"", "", false},
    {"rar",     ".rar",     "unrar x - \"%s\"", "", false},
    {"7z",      ".7z",      "7z x -si -o\"%s\"", "", false},
};

static int g_formats_count = sizeof(g_formats) / sizeof(g_formats[0]);
static bool g_formats_initialized = false;

// Check if a command exists by trying to access it directly
// Returns the full path in out_path if found
static bool command_exists(const char *cmd, char *out_path, size_t out_len) {
    // Common FreeBSD binary locations
    const char *paths[] = {
        "/bin/%s",
        "/usr/bin/%s",
        "/usr/local/bin/%s",
        "/sbin/%s",
        "/usr/sbin/%s",
        NULL
    };

    char full_path[256];
    for (int i = 0; paths[i] != NULL; i++) {
        snprintf(full_path, sizeof(full_path), paths[i], cmd);
        printf("[FORMATS] Checking: %s\n", full_path);
        if (access(full_path, X_OK) == 0) {
            printf("[FORMATS] ✓ Found executable at: %s\n", full_path);
            if (out_path && out_len > 0) {
                strncpy(out_path, full_path, out_len - 1);
                out_path[out_len - 1] = '\0';
            }
            return true;
        }
    }

    printf("[FORMATS] ✗ Command '%s' not found in standard paths\n", cmd);
    return false;
}

void init_formats(void) {
    if (g_formats_initialized) {
        return;
    }

    printf("[FORMATS] Detecting available decompression tools...\n");

    // Debug: Try to list actual directories to see what's available
    printf("[FORMATS] DEBUG: Attempting to list /bin contents...\n");
    system("ls -la /bin/ 2>&1 | head -20");
    printf("[FORMATS] DEBUG: Attempting to list /usr/bin contents...\n");
    system("ls -la /usr/bin/ 2>&1 | head -20");
    printf("[FORMATS] DEBUG: Checking if we can even run 'ls'...\n");
    system("which ls 2>&1");
    printf("[FORMATS] DEBUG: Trying direct tar execution test...\n");
    system("tar --version 2>&1");
    printf("[FORMATS] ----\n");

    char tar_path[256] = {0};
    // Check for tar (should always exist on FreeBSD)
    if (command_exists("tar", tar_path, sizeof(tar_path))) {
        printf("[FORMATS] ✓ tar found - enabling .tar, .tar.gz, .tgz, .tar.bz2, .tar.xz\n");
        for (int i = 0; i < g_formats_count; i++) {
            if (strncmp(g_formats[i].name, "tar", 3) == 0) {
                g_formats[i].available = true;
                strncpy(g_formats[i].binary_path, tar_path, sizeof(g_formats[i].binary_path) - 1);
                // Update command to use absolute path
                snprintf(g_formats[i].command, sizeof(g_formats[i].command),
                        "%s -xvf - -C \"%%s\"", tar_path);
            }
        }
    } else {
        printf("[FORMATS] ✗ tar not found (unexpected on FreeBSD!)\n");
    }

    char unzip_path[256] = {0};
    // Check for unzip
    if (command_exists("unzip", unzip_path, sizeof(unzip_path))) {
        printf("[FORMATS] ✓ unzip found - enabling .zip\n");
        for (int i = 0; i < g_formats_count; i++) {
            if (strcmp(g_formats[i].name, "zip") == 0) {
                g_formats[i].available = true;
                strncpy(g_formats[i].binary_path, unzip_path, sizeof(g_formats[i].binary_path) - 1);
                snprintf(g_formats[i].command, sizeof(g_formats[i].command),
                        "%s -o - -d \"%%s\"", unzip_path);
            }
        }
    } else {
        printf("[FORMATS] ✗ unzip not found\n");
    }

    char unrar_path[256] = {0};
    // Check for unrar
    if (command_exists("unrar", unrar_path, sizeof(unrar_path))) {
        printf("[FORMATS] ✓ unrar found - enabling .rar\n");
        for (int i = 0; i < g_formats_count; i++) {
            if (strcmp(g_formats[i].name, "rar") == 0) {
                g_formats[i].available = true;
                strncpy(g_formats[i].binary_path, unrar_path, sizeof(g_formats[i].binary_path) - 1);
                snprintf(g_formats[i].command, sizeof(g_formats[i].command),
                        "%s x - \"%%s\"", unrar_path);
            }
        }
    } else {
        printf("[FORMATS] ✗ unrar not found\n");
    }

    char sevenz_path[256] = {0};
    // Check for 7z
    if (command_exists("7z", sevenz_path, sizeof(sevenz_path)) ||
        command_exists("7za", sevenz_path, sizeof(sevenz_path))) {
        printf("[FORMATS] ✓ 7z found - enabling .7z\n");
        for (int i = 0; i < g_formats_count; i++) {
            if (strcmp(g_formats[i].name, "7z") == 0) {
                g_formats[i].available = true;
                strncpy(g_formats[i].binary_path, sevenz_path, sizeof(g_formats[i].binary_path) - 1);
                snprintf(g_formats[i].command, sizeof(g_formats[i].command),
                        "%s x -si -o\"%%s\"", sevenz_path);
            }
        }
    } else {
        printf("[FORMATS] ✗ 7z not found\n");
    }

    g_formats_initialized = true;
    printf("[FORMATS] Format detection complete\n");
}

int get_supported_formats(Format *formats, int max_count) {
    if (!g_formats_initialized) {
        init_formats();
    }

    int count = 0;
    for (int i = 0; i < g_formats_count && count < max_count; i++) {
        if (g_formats[i].available) {
            formats[count] = g_formats[i];
            count++;
        }
    }
    return count;
}

const char* get_extraction_command(const char *format_name, const char *dest_path) {
    if (!g_formats_initialized) {
        init_formats();
    }

    for (int i = 0; i < g_formats_count; i++) {
        if (g_formats[i].available && strcmp(g_formats[i].name, format_name) == 0) {
            static char cmd[512];
            snprintf(cmd, sizeof(cmd), g_formats[i].command, dest_path);
            return cmd;
        }
    }

    return NULL;
}

void handle_list_formats(int client_sock) {
    if (!g_formats_initialized) {
        init_formats();
    }

    printf("[FORMATS] Client requested supported formats list\n");

    // Build JSON response
    char response[4096];
    int offset = 0;

    offset += snprintf(response + offset, sizeof(response) - offset, "[");

    bool first = true;
    for (int i = 0; i < g_formats_count; i++) {
        if (g_formats[i].available) {
            if (!first) {
                offset += snprintf(response + offset, sizeof(response) - offset, ",");
            }
            offset += snprintf(response + offset, sizeof(response) - offset,
                             "{\"name\":\"%s\",\"extension\":\"%s\"}",
                             g_formats[i].name, g_formats[i].extension);
            first = false;
        }
    }

    offset += snprintf(response + offset, sizeof(response) - offset, "]\n");

    printf("[FORMATS] Sending: %s", response);
    send(client_sock, response, strlen(response), 0);
}
