/* RAR extraction handler implementation */

#define _FILE_OFFSET_BITS 64

#include "unrar_handler.h"
#include "notify.h"
#include "config.h"
#include "protocol.h"
#include "extract_queue.h"
#include "third_party/unrar/unrar_wrapper.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <dirent.h>
#include <time.h>

#define RECV_BUFFER_SIZE (256 * 1024)  /* 256KB receive buffer */

struct ProgressState {
    int sock;
    time_t last_sent;
    char last_filename[PATH_MAX];
    unsigned int min_interval_sec;
};

static char g_last_mkdir_err[256];
static int g_last_mkdir_errno;

/* Helper to reliably send data */
static void send_all(int sock, const char *data) {
    size_t len = strlen(data);
    size_t sent = 0;
    time_t last_ok = time(NULL);
    while (sent < len) {
        ssize_t n = send(sock, data + sent, len - sent, 0);
        if (n < 0) {
            if (errno == EAGAIN || errno == EWOULDBLOCK) {
                if (time(NULL) - last_ok > 10) {
                    break;
                }
                usleep(1000);
                continue;
            }
            break; 
        }
        sent += n;
        last_ok = time(NULL);
        payload_touch_activity();
    }
}

static void get_storage_root(const char *dest_path, char *out, size_t out_len) {
    if (!dest_path || dest_path[0] != '/') {
        snprintf(out, out_len, "/data");
        return;
    }
    if (strncmp(dest_path, "/mnt/", 5) == 0) {
        const char *p = dest_path + 5;
        const char *slash = strchr(p, '/');
        if (slash) {
            size_t len = (size_t)(slash - dest_path);
            if (len >= out_len) len = out_len - 1;
            memcpy(out, dest_path, len);
            out[len] = '\0';
            return;
        }
        snprintf(out, out_len, "%s", dest_path);
        return;
    }
    if (strncmp(dest_path, "/data", 5) == 0) {
        snprintf(out, out_len, "/data");
        return;
    }
    snprintf(out, out_len, "/data");
}

static void build_temp_dir(const char *dest_path, char *out, size_t out_len) {
    char root[PATH_MAX];
    get_storage_root(dest_path, root, sizeof(root));
    snprintf(out, out_len, "%s/ps5upload/tmp", root);
}

static int ends_with(const char *value, const char *suffix) {
    if (!value || !suffix) return 0;
    size_t vlen = strlen(value);
    size_t slen = strlen(suffix);
    if (slen > vlen) return 0;
    return strncmp(value + vlen - slen, suffix, slen) == 0;
}

static void rstrip_space(char *value) {
    if (!value) return;
    size_t len = strlen(value);
    while (len > 0 && (value[len - 1] == ' ' || value[len - 1] == '\t')) {
        value[len - 1] = '\0';
        len--;
    }
}

static void rstrip_slash(char *value) {
    if (!value) return;
    size_t len = strlen(value);
    while (len > 1 && value[len - 1] == '/') {
        value[len - 1] = '\0';
        len--;
    }
}

static void build_temp_dir_override(const char *dest_path, const char *override_root, char *out, size_t out_len) {
    if (override_root && override_root[0] != '\0') {
        char root[PATH_MAX];
        strncpy(root, override_root, sizeof(root) - 1);
        root[sizeof(root) - 1] = '\0';
        rstrip_space(root);
        rstrip_slash(root);
        if (ends_with(root, "/ps5upload/tmp")) {
            snprintf(out, out_len, "%s", root);
            return;
        }
        snprintf(out, out_len, "%s/ps5upload/tmp", root);
        return;
    }
    build_temp_dir(dest_path, out, out_len);
}

static int mkdir_recursive_ex(const char *path, char *err, size_t err_len) {
    char tmp[PATH_MAX];
    char *p = NULL;
    size_t len;

    snprintf(tmp, sizeof(tmp), "%s", path);
    len = strlen(tmp);
    if (len >= sizeof(tmp) - 1) {
        if (err && err_len > 0) {
            snprintf(err, err_len, "path too long");
        }
        errno = ENAMETOOLONG;
        return -1;
    }
    if (len == 0) {
        errno = EINVAL;
        if (err && err_len > 0) {
            snprintf(err, err_len, "invalid path");
        }
        return -1;
    }
    if (tmp[len - 1] == '/') {
        tmp[len - 1] = 0;
    }

    for (p = tmp + 1; *p; p++) {
        if (*p == '/') {
            *p = 0;
            struct stat st;
            if (stat(tmp, &st) == 0) {
                if (!S_ISDIR(st.st_mode)) {
                    if (err && err_len > 0) {
                        snprintf(err, err_len, "path component is not a directory: %s", tmp);
                    }
                    errno = ENOTDIR;
                    return -1;
                }
            } else if (mkdir(tmp, 0777) != 0 && errno != EEXIST) {
                if (err && err_len > 0) {
                    snprintf(err, err_len, "mkdir %s failed: %s", tmp, strerror(errno));
                }
                return -1;
            }
            chmod(tmp, 0777);
            *p = '/';
        }
    }
    {
        struct stat st;
        if (stat(tmp, &st) == 0) {
            if (!S_ISDIR(st.st_mode)) {
                if (err && err_len > 0) {
                    snprintf(err, err_len, "path component is not a directory: %s", tmp);
                }
                errno = ENOTDIR;
                return -1;
            }
        } else if (mkdir(tmp, 0777) != 0 && errno != EEXIST) {
            if (err && err_len > 0) {
                snprintf(err, err_len, "mkdir %s failed: %s", tmp, strerror(errno));
            }
            return -1;
        }
    }
    chmod(tmp, 0777);
    return 0;
}

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

char *receive_rar_to_temp(int sock, size_t file_size, const char *temp_dir) {
    /* Debug off_t size */
    printf("[RAR] sizeof(off_t) = %zu\n", sizeof(off_t));

    /* Create temp directory if needed */
    mkdir_recursive_ex(temp_dir, NULL, 0);

    /* Generate temp file path */
    char *temp_path = malloc(PATH_MAX);
    if (!temp_path) {
        return NULL;
    }

    int fd = -1;
    snprintf(temp_path, PATH_MAX, "%s/upload_XXXXXX", temp_dir);
    fd = mkstemp(temp_path);

    if (fd < 0) {
        /* Fallback for platforms without mkstemp */
        static unsigned int temp_counter = 0;
        for (int attempt = 0; attempt < 100; attempt++) {
            snprintf(temp_path, PATH_MAX, "%s/upload_%d_%u.rar",
                     temp_dir, (int)getpid(), temp_counter++);
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
    char mkdir_err[256];
    g_last_mkdir_err[0] = '\0';
    g_last_mkdir_errno = 0;
    struct stat st;
    if (stat(dest_dir, &st) == 0 && !S_ISDIR(st.st_mode)) {
        snprintf(g_last_mkdir_err, sizeof(g_last_mkdir_err), "destination exists and is not a directory");
        printf("[RAR] ERROR: Destination exists and is not a directory: %s\n", dest_dir);
        return -2;
    }
    if (mkdir_recursive_ex(dest_dir, mkdir_err, sizeof(mkdir_err)) != 0) {
        if (mkdir_err[0] == '\0') {
            snprintf(mkdir_err, sizeof(mkdir_err), "mkdir failed: errno=%d %s", errno, strerror(errno));
        }
        snprintf(g_last_mkdir_err, sizeof(g_last_mkdir_err), "%s", mkdir_err);
        g_last_mkdir_errno = errno;
        printf("[RAR] Warning: failed to create destination path (will try extract anyway): %s (%s)\n",
               dest_dir, mkdir_err);
        g_last_mkdir_err[0] = '\0';
        g_last_mkdir_errno = 0;
    }

    printf("[RAR] Extracting %s to %s (strip_root=%d)\n", rar_path, dest_dir, strip_root);

    /* Scan first to get file count (safe mode only) */
    int count = 0;
    unsigned long long size = 0;
    if (mode == UNRAR_MODE_SAFE) {
        int scan_result = unrar_scan(rar_path, &count, &size, NULL, 0);
        if (scan_result != UNRAR_OK) {
            printf("[RAR] Scan failed: %s\n", unrar_strerror(scan_result));
            return scan_result;
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
        return extract_result;
    }

    if (file_count) *file_count = count;
    if (total_bytes) *total_bytes = size;

    printf("[RAR] Extraction complete: %d files, %llu bytes\n", count, size);
    return UNRAR_OK;
}

void handle_upload_rar(int sock, const char *args, UnrarMode mode) {
    char dest_path[PATH_MAX];
    unsigned long long file_size = 0;
    int allow_overwrite = 1;
    char temp_root_override[PATH_MAX];
    temp_root_override[0] = '\0';

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

    rstrip_space(args_copy);

    char *size_token = NULL;
    while (1) {
        char *last_space = strrchr(args_copy, ' ');
        if (!last_space) {
            break;
        }
        char *tail_token = last_space + 1;
        if (*tail_token == '\0') {
            *last_space = '\0';
            rstrip_space(args_copy);
            continue;
        }
        if (strcasecmp(tail_token, "OVERWRITE") == 0 || strcasecmp(tail_token, "NOOVERWRITE") == 0) {
            allow_overwrite = (strcasecmp(tail_token, "NOOVERWRITE") != 0);
            *last_space = '\0';
            rstrip_space(args_copy);
            continue;
        }
        if (strncasecmp(tail_token, "TMP=", 4) == 0) {
            const char *value = tail_token + 4;
            if (!*value) {
                const char *error = "ERROR: Invalid TMP path\n";
                send_all(sock, error);
                return;
            }
            strncpy(temp_root_override, value, sizeof(temp_root_override) - 1);
            temp_root_override[sizeof(temp_root_override) - 1] = '\0';
            *last_space = '\0';
            rstrip_space(args_copy);
            continue;
        }
        size_token = tail_token;
        *last_space = '\0';
        break;
    }

    if (!size_token) {
        const char *error = "ERROR: Invalid UPLOAD_RAR format (expected: UPLOAD_RAR <dest> <size>)\n";
        send_all(sock, error);
        return;
    }

    // Parse size
    char *endptr;
    file_size = strtoull(size_token, &endptr, 10);
    if (*endptr != '\0') {
        const char *error = "ERROR: Invalid file size format\n";
        send_all(sock, error);
        return;
    }

    // Parse path
    rstrip_space(args_copy);
    strncpy(dest_path, args_copy, sizeof(dest_path) - 1);
    dest_path[sizeof(dest_path) - 1] = '\0';
    if (dest_path[0] == '\0') {
        const char *error = "ERROR: Invalid destination path\n";
        send_all(sock, error);
        return;
    }

    if (file_size == 0 || file_size > (500ULL * 1024 * 1024 * 1024)) {  /* Max 500GB */
        const char *error = "ERROR: Invalid file size\n";
        send_all(sock, error);
        return;
    }

    if (!is_path_safe(dest_path)) {
        const char *error = "ERROR: Invalid path\n";
        send_all(sock, error);
        return;
    }
    if (temp_root_override[0] != '\0' && !is_path_safe(temp_root_override)) {
        const char *error = "ERROR: Invalid temp storage path\n";
        send_all(sock, error);
        return;
    }

    if (!allow_overwrite) {
        struct stat st;
        if (stat(dest_path, &st) == 0) {
            const char *error = "ERROR: Destination already exists\n";
            send_all(sock, error);
            return;
        }
        if (errno != ENOENT) {
            char error[256];
            snprintf(error, sizeof(error), "ERROR: Unable to access destination: %s\n", strerror(errno));
            send_all(sock, error);
            return;
        }
    }

    printf("[RAR] Upload request: %s (%llu bytes)\n", dest_path, file_size);

    /* Send ready signal */
    const char *ready = "READY\n";
    send_all(sock, ready);

    char mkdir_err[256];
    char temp_root[PATH_MAX];
    char temp_dir[PATH_MAX];
    static unsigned int temp_counter = 0;
    build_temp_dir_override(dest_path, temp_root_override, temp_root, sizeof(temp_root));
    if (mkdir_recursive_ex(temp_root, mkdir_err, sizeof(mkdir_err)) != 0) {
        char error[256];
        if (mkdir_err[0] == '\0') {
            snprintf(mkdir_err, sizeof(mkdir_err), "mkdir failed: errno=%d %s", errno, strerror(errno));
        }
        snprintf(error, sizeof(error), "ERROR: Failed to create temp root: %s\n", mkdir_err);
        send_all(sock, error);
        return;
    }
    snprintf(temp_dir, sizeof(temp_dir), "%s/rar_%d_%u", temp_root, (int)getpid(), temp_counter++);
    if (mkdir_recursive_ex(temp_dir, mkdir_err, sizeof(mkdir_err)) != 0) {
        char error[256];
        if (mkdir_err[0] == '\0') {
            snprintf(mkdir_err, sizeof(mkdir_err), "mkdir failed: errno=%d %s", errno, strerror(errno));
        }
        snprintf(error, sizeof(error), "ERROR: Failed to create temp directory: %s\n", mkdir_err);
        send_all(sock, error);
        return;
    }

    /* Receive RAR file to temp */
    char *temp_path = receive_rar_to_temp(sock, (size_t)file_size, temp_dir);
    if (!temp_path) {
        const char *error = "ERROR: Failed to receive RAR file\n";
        send_all(sock, error);
        return;
    }

    int queue_id = extract_queue_add(temp_path, dest_path, 1, temp_dir);
    if (queue_id == -2) {
        const char *error = "ERROR: Duplicate extraction request\n";
        send_all(sock, error);
        free(temp_path);
        return;
    }
    if (queue_id < 0) {
        const char *error = "ERROR: Failed to queue extraction\n";
        send_all(sock, error);
        free(temp_path);
        return;
    }
    free(temp_path);
    extract_queue_process();

    /* Response: queued */
    char response[64];
    snprintf(response, sizeof(response), "QUEUED %d\n", queue_id);
    send_all(sock, response);
}
