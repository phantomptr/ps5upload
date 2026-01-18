/* RAR extraction handler implementation */

#include "unrar_handler.h"
#include "notify.h"
#include "third_party/unrar/unrar_wrapper.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>

#define RAR_TEMP_DIR "/data/ps5upload/temp"
#define RECV_BUFFER_SIZE (256 * 1024)  /* 256KB receive buffer */

/* Progress callback for extraction */
static int extraction_progress(const char *filename, unsigned long long file_size,
                               int files_done, void *user_data) {
    (void)file_size;
    (void)user_data;
    printf("[RAR] Extracting (%d): %s\n", files_done + 1, filename);
    return 0;  /* Continue extraction */
}

char *receive_rar_to_temp(int sock, size_t file_size) {
    /* Create temp directory if needed */
    mkdir(RAR_TEMP_DIR, 0777);

    /* Generate temp file path */
    static int temp_counter = 0;
    char *temp_path = malloc(PATH_MAX);
    if (!temp_path) {
        return NULL;
    }
    snprintf(temp_path, PATH_MAX, "%s/upload_%d_%d.rar",
             RAR_TEMP_DIR, (int)getpid(), temp_counter++);

    /* Open temp file for writing */
    int fd = open(temp_path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        perror("[RAR] Failed to create temp file");
        free(temp_path);
        return NULL;
    }

    /* Receive data */
    unsigned char *buffer = malloc(RECV_BUFFER_SIZE);
    if (!buffer) {
        close(fd);
        unlink(temp_path);
        free(temp_path);
        return NULL;
    }

    size_t received = 0;
    int last_percent = -1;

    while (received < file_size) {
        size_t to_recv = file_size - received;
        if (to_recv > RECV_BUFFER_SIZE) {
            to_recv = RECV_BUFFER_SIZE;
        }

        ssize_t n = recv(sock, buffer, to_recv, 0);
        if (n <= 0) {
            if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
                usleep(1000);
                continue;
            }
            perror("[RAR] Receive error");
            close(fd);
            unlink(temp_path);
            free(buffer);
            free(temp_path);
            return NULL;
        }

        ssize_t written = write(fd, buffer, (size_t)n);
        if (written != n) {
            perror("[RAR] Write error");
            close(fd);
            unlink(temp_path);
            free(buffer);
            free(temp_path);
            return NULL;
        }

        received += (size_t)n;

        /* Progress logging */
        int percent = (int)((received * 100) / file_size);
        if (percent != last_percent && percent % 10 == 0) {
            printf("[RAR] Receiving: %d%%\n", percent);
            last_percent = percent;
        }
    }

    close(fd);
    free(buffer);

    printf("[RAR] Received %zu bytes to %s\n", received, temp_path);
    return temp_path;
}

int extract_rar_file(const char *rar_path, const char *dest_dir, int strip_root,
                     int *file_count, unsigned long long *total_bytes) {
    if (!rar_path || !dest_dir) {
        return -1;
    }

    /* Create destination directory */
    mkdir(dest_dir, 0777);

    printf("[RAR] Extracting %s to %s (strip_root=%d)\n", rar_path, dest_dir, strip_root);

    /* Scan first to get file count */
    int count = 0;
    unsigned long long size = 0;
    
    /* We just scan to get stats here, common_root check is done in handle_upload_rar or we could trust the caller */
    int scan_result = unrar_scan(rar_path, &count, &size, NULL, 0);
    if (scan_result != UNRAR_OK) {
        printf("[RAR] Scan failed: %s\n", unrar_strerror(scan_result));
        return -1;
    }

    printf("[RAR] Archive contains %d files, %llu bytes uncompressed\n", count, size);

    /* Extract */
    int extract_result = unrar_extract(rar_path, dest_dir, strip_root, extraction_progress, NULL);
    if (extract_result != UNRAR_OK) {
        printf("[RAR] Extraction failed: %s\n", unrar_strerror(extract_result));
        return -1;
    }

    if (file_count) *file_count = count;
    if (total_bytes) *total_bytes = size;

    printf("[RAR] Extraction complete: %d files, %llu bytes\n", count, size);
    return 0;
}

void handle_upload_rar(int sock, const char *args) {
    char dest_path[PATH_MAX];
    unsigned long long file_size = 0;

    /* Parse arguments: dest_path file_size */
    if (sscanf(args, "%s %llu", dest_path, &file_size) != 2) {
        const char *error = "ERROR: Invalid UPLOAD_RAR format (expected: UPLOAD_RAR <dest> <size>)\n";
        send(sock, error, strlen(error), 0);
        return;
    }

    if (file_size == 0 || file_size > (10ULL * 1024 * 1024 * 1024)) {  /* Max 10GB */
        const char *error = "ERROR: Invalid file size\n";
        send(sock, error, strlen(error), 0);
        return;
    }

    printf("[RAR] Upload request: %s (%llu bytes)\n", dest_path, file_size);

    /* Send ready signal */
    const char *ready = "READY\n";
    send(sock, ready, strlen(ready), 0);

    /* Receive RAR file to temp */
    char *temp_path = receive_rar_to_temp(sock, (size_t)file_size);
    if (!temp_path) {
        const char *error = "ERROR: Failed to receive RAR file\n";
        send(sock, error, strlen(error), 0);
        return;
    }

    /* Check for common root folder */
    char common_root[256] = {0};
    int count = 0;
    unsigned long long size = 0;
    int scan_res = unrar_scan(temp_path, &count, &size, common_root, sizeof(common_root));
    
    int strip_root = 0;
    if (scan_res == UNRAR_OK && common_root[0] != '\0') {
        /* Get basename of dest_path */
        const char *base_name = strrchr(dest_path, '/');
        if (base_name) {
            base_name++; /* Skip slash */
        } else {
            base_name = dest_path;
        }
        
        /* Remove potential trailing slash from common_root for comparison */
        size_t len = strlen(common_root);
        if (len > 0 && (common_root[len-1] == '/' || common_root[len-1] == '\\')) {
            common_root[len-1] = '\0';
        }

        if (strcmp(common_root, base_name) == 0) {
            printf("[RAR] Detected common root '%s' matching destination '%s', stripping it.\n", common_root, base_name);
            strip_root = 1;
        }
    }

    /* Extract */
    int file_count = 0;
    unsigned long long total_bytes = 0;
    int result = extract_rar_file(temp_path, dest_path, strip_root, &file_count, &total_bytes);

    /* Cleanup temp file */
    unlink(temp_path);
    free(temp_path);

    if (result != 0) {
        const char *error = "ERROR: RAR extraction failed\n";
        send(sock, error, strlen(error), 0);
        notify_error("PS5 Upload", "RAR extraction failed");
        return;
    }

    /* Success response */
    char response[256];
    snprintf(response, sizeof(response), "SUCCESS %d %llu\n", file_count, total_bytes);
    send(sock, response, strlen(response), 0);

    /* Notification */
    char notify_msg[128];
    snprintf(notify_msg, sizeof(notify_msg), "Extracted %d files (%llu MB)",
             file_count, total_bytes / (1024 * 1024));
    notify_info("PS5 Upload", notify_msg);
}
