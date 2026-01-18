/* RAR extraction handler implementation */

#define _FILE_OFFSET_BITS 64

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

/* Helper to reliably send data */
static void send_all(int sock, const char *data) {
    size_t len = strlen(data);
    size_t sent = 0;
    while (sent < len) {
        ssize_t n = send(sock, data + sent, len - sent, 0);
        if (n < 0) {
            if (errno == EAGAIN || errno == EWOULDBLOCK) {
                usleep(1000);
                continue;
            }
            break; 
        }
        sent += n;
    }
}

/* Progress callback for extraction */
static int extraction_progress(const char *filename, unsigned long long file_size,
                               int files_done, void *user_data) {
    (void)file_size;
    
    /* Send progress to client if socket is provided */
    int *sock_ptr = (int*)user_data;
    if (sock_ptr) {
        char msg[PATH_MAX + 64];
        /* Ensure filename doesn't break the protocol line (basic sanitization) */
        /* Protocol: EXTRACTING <count> <filename> */
        snprintf(msg, sizeof(msg), "EXTRACTING %d %s\n", files_done + 1, filename);
        send_all(*sock_ptr, msg);
    }

    printf("[RAR] Extracting (%d): %s\n", files_done + 1, filename);
    usleep(1000); /* Yield CPU */
    return 0;  /* Continue extraction */
}

char *receive_rar_to_temp(int sock, size_t file_size) {
    /* Debug off_t size */
    printf("[RAR] sizeof(off_t) = %zu\n", sizeof(off_t));

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

        /* Write loop to handle partial writes */
        size_t to_write = (size_t)n;
        size_t written_total = 0;
        while (written_total < to_write) {
            ssize_t w = write(fd, buffer + written_total, to_write - written_total);
            if (w < 0) {
                if (errno == EINTR) continue;
                perror("[RAR] Write error");
                close(fd);
                unlink(temp_path);
                free(buffer);
                free(temp_path);
                return NULL;
            }
            written_total += (size_t)w;
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
                     int *file_count, unsigned long long *total_bytes, void *user_data) {
    if (!rar_path || !dest_dir) {
        return -1;
    }

    /* Create destination directory */
    mkdir(dest_dir, 0777);

    printf("[RAR] Extracting %s to %s (strip_root=%d)\n", rar_path, dest_dir, strip_root);

    /* Scan first to get file count */
    int count = 0;
    unsigned long long size = 0;
    
    /* We just scan to get stats here */
    int scan_result = unrar_scan(rar_path, &count, &size, NULL, 0);
    if (scan_result != UNRAR_OK) {
        printf("[RAR] Scan failed: %s\n", unrar_strerror(scan_result));
        return -1;
    }

    printf("[RAR] Archive contains %d files, %llu bytes uncompressed\n", count, size);

    /* Extract */
    int extract_result = unrar_extract(rar_path, dest_dir, strip_root, extraction_progress, user_data);
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

    /* Parse arguments: dest_path file_size
       Since dest_path can contain spaces, we parse from the end.
       Format is: <path> <size>
    */
    
    // Create a working copy to manipulate
    char args_copy[PATH_MAX + 32];
    strncpy(args_copy, args, sizeof(args_copy) - 1);
    args_copy[sizeof(args_copy) - 1] = '\0';

    // Remove trailing newlines
    size_t len = strlen(args_copy);
    while(len > 0 && (args_copy[len-1] == '\n' || args_copy[len-1] == '\r')) {
        args_copy[len-1] = '\0';
        len--;
    }

    // Find the last space, which should separate path and size
    char *last_space = strrchr(args_copy, ' ');
    if (!last_space) {
        const char *error = "ERROR: Invalid UPLOAD_RAR format (expected: UPLOAD_RAR <dest> <size>)\n";
        send_all(sock, error);
        return;
    }

    // Parse size
    char *endptr;
    file_size = strtoull(last_space + 1, &endptr, 10);
    if (*endptr != '\0') {
         const char *error = "ERROR: Invalid file size format\n";
         send_all(sock, error);
         return;
    }

    // Parse path
    *last_space = '\0'; // Null-terminate path at the space
    strncpy(dest_path, args_copy, sizeof(dest_path) - 1);
    dest_path[sizeof(dest_path) - 1] = '\0';

    if (file_size == 0 || file_size > (500ULL * 1024 * 1024 * 1024)) {  /* Max 500GB */
        const char *error = "ERROR: Invalid file size\n";
        send_all(sock, error);
        return;
    }

    printf("[RAR] Upload request: %s (%llu bytes)\n", dest_path, file_size);

    /* Send ready signal */
    const char *ready = "READY\n";
    send_all(sock, ready);

    /* Receive RAR file to temp */
    char *temp_path = receive_rar_to_temp(sock, (size_t)file_size);
    if (!temp_path) {
        const char *error = "ERROR: Failed to receive RAR file\n";
        send_all(sock, error);
        return;
    }

    /* Common root detection removed per user request. 
       We always extract exactly what is in the archive to the destination. */
    int strip_root = 0;

    /* Extract */
    int file_count = 0;
    unsigned long long total_bytes = 0;
    /* Pass socket for progress updates */
    int result = extract_rar_file(temp_path, dest_path, strip_root, &file_count, &total_bytes, &sock);

    /* Cleanup temp file */
    unlink(temp_path);
    free(temp_path);

    if (result != 0) {
        const char *error = "ERROR: RAR extraction failed\n";
        send_all(sock, error);
        notify_error("PS5 Upload", "RAR extraction failed");
        return;
    }

    /* Success response */
    char response[256];
    snprintf(response, sizeof(response), "SUCCESS %d %llu\n", file_count, total_bytes);
    send_all(sock, response);

    /* Notification */
    char notify_msg[128];
    snprintf(notify_msg, sizeof(notify_msg), "Extracted %d files (%llu MB)",
             file_count, total_bytes / (1024 * 1024));
    notify_info("PS5 Upload", notify_msg);
}