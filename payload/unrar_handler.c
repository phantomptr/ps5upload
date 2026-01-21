/* RAR extraction handler implementation */

#define _FILE_OFFSET_BITS 64

#include "unrar_handler.h"
#include "notify.h"
#include "config.h"
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
#include <dirent.h>
#include <time.h>

#define RAR_TEMP_DIR "/data/ps5upload/temp"
#define RECV_BUFFER_SIZE (256 * 1024)  /* 256KB receive buffer */

struct ProgressState {
    int sock;
    time_t last_sent;
    char last_filename[PATH_MAX];
    unsigned int min_interval_sec;
};

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

static int chmod_recursive(const char *path, mode_t mode, char *err, size_t err_len);

/* Progress callback for extraction */
static int extraction_progress(const char *filename, unsigned long long file_size,
                               int files_done, unsigned long long total_processed, unsigned long long total_size, void *user_data) {
    (void)file_size;
    (void)files_done;
    
    /* Send progress to client if socket is provided */
    struct ProgressState *progress = (struct ProgressState *)user_data;
    if (progress) {
        time_t now = time(NULL);
        int filename_changed = (strncmp(progress->last_filename, filename, sizeof(progress->last_filename)) != 0);
        if (!filename_changed && progress->min_interval_sec > 0 &&
            now - progress->last_sent < (time_t)progress->min_interval_sec) {
            return 0;
        }
        if (filename_changed) {
            strncpy(progress->last_filename, filename, sizeof(progress->last_filename) - 1);
            progress->last_filename[sizeof(progress->last_filename) - 1] = '\0';
        }
        progress->last_sent = now;

        char msg[PATH_MAX + 128];
        int percent = (total_size > 0) ? (int)((total_processed * 100) / total_size) : 0;
        
        /* Protocol: EXTRACT_PROGRESS <percent> <processed> <total> */
        /* Also include EXTRACTING for backward compat / filename display if needed, but client mainly needs progress now */
        snprintf(msg, sizeof(msg), "EXTRACT_PROGRESS %d %llu %llu %s\n", percent, total_processed, total_size, filename);
        send_all(progress->sock, msg);
    }

    /* printf("[RAR] Extracting (%d): %s\n", files_done + 1, filename); */
    /* usleep(1000);  Yield CPU - handled in wrapper now */
    return 0;  /* Continue extraction */
}

char *receive_rar_to_temp(int sock, size_t file_size) {
    /* Debug off_t size */
    printf("[RAR] sizeof(off_t) = %zu\n", sizeof(off_t));

    /* Create temp directory if needed */
    mkdir(RAR_TEMP_DIR, 0777);

    /* Generate temp file path */
    char *temp_path = malloc(PATH_MAX);
    if (!temp_path) {
        return NULL;
    }

    int fd = -1;
    snprintf(temp_path, PATH_MAX, "%s/upload_XXXXXX", RAR_TEMP_DIR);
    fd = mkstemp(temp_path);

    if (fd < 0) {
        /* Fallback for platforms without mkstemp */
        static unsigned int temp_counter = 0;
        for (int attempt = 0; attempt < 100; attempt++) {
            snprintf(temp_path, PATH_MAX, "%s/upload_%d_%u.rar",
                     RAR_TEMP_DIR, (int)getpid(), temp_counter++);
            fd = open(temp_path, O_WRONLY | O_CREAT | O_EXCL | O_TRUNC, 0644);
            if (fd >= 0) {
                break;
            }
            if (errno != EEXIST) {
                break;
            }
        }
    }

    /* Open temp file for writing */
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
                     int *file_count, unsigned long long *total_bytes, void *user_data, UnrarMode mode) {
    if (!rar_path || !dest_dir) {
        return -1;
    }

    /* Create destination directory */
    mkdir(dest_dir, 0777);

    printf("[RAR] Extracting %s to %s (strip_root=%d)\n", rar_path, dest_dir, strip_root);

    /* Scan first to get file count (safe mode only) */
    int count = 0;
    unsigned long long size = 0;
    if (mode == UNRAR_MODE_SAFE) {
        int scan_result = unrar_scan(rar_path, &count, &size, NULL, 0);
        if (scan_result != UNRAR_OK) {
            printf("[RAR] Scan failed: %s\n", unrar_strerror(scan_result));
            return -1;
        }
    }

    if (mode == UNRAR_MODE_SAFE) {
        printf("[RAR] Archive contains %d files, %llu bytes uncompressed\n", count, size);
    }

    /* Extract */
    /* Pass total size to wrapper for progress if known */
    unrar_extract_opts opts;
    if (mode == UNRAR_MODE_SAFE) {
        opts.keepalive_interval_sec = UNRAR_SAFE_KEEPALIVE_SEC;
        opts.sleep_every_bytes = UNRAR_SAFE_SLEEP_EVERY_BYTES;
        opts.sleep_us = UNRAR_SAFE_SLEEP_US;
    } else if (mode == UNRAR_MODE_TURBO) {
        opts.keepalive_interval_sec = UNRAR_TURBO_KEEPALIVE_SEC;
        opts.sleep_every_bytes = UNRAR_TURBO_SLEEP_EVERY_BYTES;
        opts.sleep_us = UNRAR_TURBO_SLEEP_US;
    } else {
        opts.keepalive_interval_sec = UNRAR_FAST_KEEPALIVE_SEC;
        opts.sleep_every_bytes = UNRAR_FAST_SLEEP_EVERY_BYTES;
        opts.sleep_us = UNRAR_FAST_SLEEP_US;
    }
    int extract_result = unrar_extract(rar_path, dest_dir, strip_root, (mode == UNRAR_MODE_SAFE) ? size : 0,
                                       &opts, extraction_progress, user_data, &count, &size);
    if (extract_result != UNRAR_OK) {
        printf("[RAR] Extraction failed: %s\n", unrar_strerror(extract_result));
        return -1;
    }

    if (file_count) *file_count = count;
    if (total_bytes) *total_bytes = size;

    printf("[RAR] Extraction complete: %d files, %llu bytes\n", count, size);
    return 0;
}

void handle_upload_rar(int sock, const char *args, UnrarMode mode) {
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
        unrar_cleanup_temp();
        return;
    }

    /* Common root detection removed per user request. 
       We always extract exactly what is in the archive to the destination. */
    int strip_root = 0;

    /* Extract */
    int file_count = 0;
    unsigned long long total_bytes = 0;
    struct ProgressState progress;
    memset(&progress, 0, sizeof(progress));
    progress.sock = sock;
    if (mode == UNRAR_MODE_SAFE) {
        progress.min_interval_sec = UNRAR_SAFE_PROGRESS_INTERVAL_SEC;
    } else if (mode == UNRAR_MODE_TURBO) {
        progress.min_interval_sec = UNRAR_TURBO_PROGRESS_INTERVAL_SEC;
    } else {
        progress.min_interval_sec = UNRAR_FAST_PROGRESS_INTERVAL_SEC;
    }
    /* Pass progress state for updates */
    int result = extract_rar_file(temp_path, dest_path, strip_root, &file_count, &total_bytes, &progress, mode);

    /* Cleanup temp file */
    unlink(temp_path);
    free(temp_path);
    unrar_cleanup_temp();

    if (result != 0) {
        const char *error = "ERROR: RAR extraction failed\n";
        send_all(sock, error);
        return;
    }

    {
        char chmod_err[256];
        if (chmod_recursive(dest_path, 0777, chmod_err, sizeof(chmod_err)) != 0) {
            printf("[RAR] Chmod failed: %s\n", chmod_err);
            send_all(sock, "ERROR: Failed to chmod extracted files\n");
            return;
        }
    }

    /* Success response */
    char response[256];
    snprintf(response, sizeof(response), "SUCCESS %d %llu\n", file_count, total_bytes);
    send_all(sock, response);

    /* Notification */
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

static int rm_recursive(const char *path) {
    DIR *d = opendir(path);
    size_t path_len = strlen(path);
    int r = -1;

    if (d) {
        struct dirent *p;
        r = 0;
        while (!r && (p = readdir(d))) {
            int r2 = -1;
            char *buf;
            size_t len;

            if (!strcmp(p->d_name, ".") || !strcmp(p->d_name, ".."))
                continue;

            len = path_len + strlen(p->d_name) + 2; 
            buf = (char *)malloc(len);

            if (buf) {
                struct stat statbuf;
                snprintf(buf, len, "%s/%s", path, p->d_name);
                if (!stat(buf, &statbuf)) {
                    if (S_ISDIR(statbuf.st_mode))
                        r2 = rm_recursive(buf);
                    else
                        r2 = unlink(buf);
                }
                free(buf);
            }
            r = r2;
        }
        closedir(d);
    }

    if (!r)
        r = rmdir(path);

    return r;
}

void unrar_cleanup_temp(void) {
    printf("[RAR] Cleaning up temp directory: %s\n", RAR_TEMP_DIR);
    rm_recursive(RAR_TEMP_DIR);
    /* Recreate it empty */
    mkdir(RAR_TEMP_DIR, 0777);
}
