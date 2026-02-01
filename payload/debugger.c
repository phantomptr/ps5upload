/* PS5 Upload Debugger Payload */

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>
#include <dirent.h>
#include <limits.h>

#include "config.h"
#include "notify.h"

#define DEBUG_ROOT "/data/ps5upload/debug"
#define REQUESTS_DIR "/data/ps5upload/requests"
#define DEBUGGER_PID_FILE "/data/ps5upload/debugger.pid"

#define STATUS_POLL_INTERVAL_MS 1000
#define LOG_TAIL_INTERVAL_MS 1000
#define REQUEST_SYNC_INTERVAL_MS 5000
#define CONNECT_TIMEOUT_SEC 2

static volatile sig_atomic_t g_should_stop = 0;

static void handle_stop(int sig) {
    (void)sig;
    g_should_stop = 1;
}

static void write_pid_file(void) {
    int fd = open(DEBUGGER_PID_FILE, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0) return;
    char buf[32];
    int len = snprintf(buf, sizeof(buf), "%d\n", (int)getpid());
    if (len > 0) {
        write(fd, buf, (size_t)len);
    }
    close(fd);
}

static void kill_previous_debugger(void) {
    FILE *fp = fopen(DEBUGGER_PID_FILE, "r");
    if (!fp) return;
    char buf[32] = {0};
    if (!fgets(buf, sizeof(buf), fp)) {
        fclose(fp);
        return;
    }
    fclose(fp);
    int pid = (int)strtol(buf, NULL, 10);
    if (pid > 1 && pid != (int)getpid()) {
        kill(pid, SIGTERM);
        usleep(200000);
        kill(pid, SIGKILL);
    }
}

static int remove_tree(const char *path) {
    DIR *dir = opendir(path);
    if (!dir) return -1;
    struct dirent *ent;
    while ((ent = readdir(dir)) != NULL) {
        if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0) continue;
        char child[PATH_MAX];
        snprintf(child, sizeof(child), "%s/%s", path, ent->d_name);
        struct stat st;
        if (lstat(child, &st) != 0) continue;
        if (S_ISDIR(st.st_mode)) {
            remove_tree(child);
            rmdir(child);
        } else {
            unlink(child);
        }
    }
    closedir(dir);
    return 0;
}

static void cleanup_debug_root(void) {
    remove_tree(DEBUG_ROOT);
    rmdir(DEBUG_ROOT);
    int fd = open("/data/ps5upload/debug_cleanup.log", O_WRONLY | O_CREAT | O_APPEND, 0666);
    if (fd >= 0) {
        char line[128];
        time_t now = time(NULL);
        int len = snprintf(line, sizeof(line), "cleanup at %ld\n", (long)now);
        if (len > 0) {
            write(fd, line, (size_t)len);
        }
        close(fd);
    }
}

static int mkdir_if_missing(const char *path, mode_t mode) {
    if (mkdir(path, mode) == 0) return 0;
    if (errno == EEXIST) return 0;
    return -1;
}

static int mkdir_p(const char *path, mode_t mode) {
    char tmp[512];
    size_t len = strlen(path);
    if (len >= sizeof(tmp)) return -1;
    memcpy(tmp, path, len + 1);

    if (len == 0) return -1;

    for (size_t i = 1; i < len; i++) {
        if (tmp[i] == '/') {
            tmp[i] = '\0';
            if (tmp[0] && mkdir_if_missing(tmp, mode) != 0) return -1;
            tmp[i] = '/';
        }
    }
    if (mkdir_if_missing(tmp, mode) != 0) return -1;
    return 0;
}

static void timestamp_now(char *out, size_t out_len) {
    time_t now = time(NULL);
    struct tm *tm_info = localtime(&now);
    if (!tm_info) {
        snprintf(out, out_len, "unknown_time");
        return;
    }
    snprintf(out, out_len, "%04d%02d%02d_%02d%02d%02d",
             tm_info->tm_year + 1900, tm_info->tm_mon + 1, tm_info->tm_mday,
             tm_info->tm_hour, tm_info->tm_min, tm_info->tm_sec);
}

static int connect_payload(void) {
    int sock = socket(AF_INET, SOCK_STREAM, 0);
    if (sock < 0) return -1;

    struct timeval tv;
    tv.tv_sec = CONNECT_TIMEOUT_SEC;
    tv.tv_usec = 0;
    setsockopt(sock, SOL_SOCKET, SO_RCVTIMEO, (const char *)&tv, sizeof(tv));
    setsockopt(sock, SOL_SOCKET, SO_SNDTIMEO, (const char *)&tv, sizeof(tv));

    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons(SERVER_PORT);
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);

    if (connect(sock, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        close(sock);
        return -1;
    }
    return sock;
}

static ssize_t recv_line(int sock, char *buf, size_t buf_len) {
    size_t pos = 0;
    while (pos + 1 < buf_len) {
        char c = '\0';
        ssize_t n = recv(sock, &c, 1, 0);
        if (n <= 0) return -1;
        buf[pos++] = c;
        if (c == '\n') break;
    }
    buf[pos] = '\0';
    return (ssize_t)pos;
}

static char *fetch_payload_status(size_t *out_len) {
    int sock = connect_payload();
    if (sock < 0) return NULL;

    const char *cmd = "PAYLOAD_STATUS\n";
    if (send(sock, cmd, strlen(cmd), 0) < 0) {
        close(sock);
        return NULL;
    }

    char header[128];
    if (recv_line(sock, header, sizeof(header)) <= 0) {
        close(sock);
        return NULL;
    }

    size_t json_len = 0;
    if (sscanf(header, "STATUS %zu", &json_len) != 1 || json_len == 0) {
        close(sock);
        return NULL;
    }

    char *json = (char *)malloc(json_len + 1);
    if (!json) {
        close(sock);
        return NULL;
    }

    size_t received = 0;
    while (received < json_len) {
        ssize_t n = recv(sock, json + received, json_len - received, 0);
        if (n <= 0) {
            free(json);
            close(sock);
            return NULL;
        }
        received += (size_t)n;
    }
    json[json_len] = '\0';

    /* Read trailing newline if present */
    char tail[2];
    recv(sock, tail, sizeof(tail), 0);

    close(sock);
    if (out_len) *out_len = json_len;
    return json;
}

static int append_text_line(const char *path, const char *line) {
    int fd = open(path, O_WRONLY | O_CREAT | O_APPEND, 0666);
    if (fd < 0) return -1;
    size_t len = strlen(line);
    ssize_t written = write(fd, line, len);
    close(fd);
    return (written == (ssize_t)len) ? 0 : -1;
}

static int append_event(const char *event_path, const char *type, const char *message) {
    char line[1024];
    time_t now = time(NULL);
    snprintf(line, sizeof(line), "{\"ts\":%ld,\"type\":\"%s\",\"msg\":\"%s\"}\n",
             (long)now, type, message ? message : "");
    return append_text_line(event_path, line);
}

static int append_status(const char *status_path, const char *json) {
    char line_prefix[128];
    time_t now = time(NULL);
    int prefix_len = snprintf(line_prefix, sizeof(line_prefix), "{\"ts\":%ld,\"type\":\"payload_status\",\"data\":", (long)now);
    if (prefix_len <= 0) return -1;

    int fd = open(status_path, O_WRONLY | O_CREAT | O_APPEND, 0666);
    if (fd < 0) return -1;

    if (write(fd, line_prefix, (size_t)prefix_len) != (ssize_t)prefix_len) {
        close(fd);
        return -1;
    }
    size_t json_len = strlen(json);
    if (write(fd, json, json_len) != (ssize_t)json_len) {
        close(fd);
        return -1;
    }
    if (write(fd, "}\n", 2) != 2) {
        close(fd);
        return -1;
    }
    close(fd);
    return 0;
}

static int tail_file_incremental(const char *src, const char *dst, off_t *offset) {
    int fd = open(src, O_RDONLY);
    if (fd < 0) return -1;

    struct stat st;
    if (fstat(fd, &st) != 0) {
        close(fd);
        return -1;
    }

    if (*offset > st.st_size) {
        *offset = 0;
    }

    if (lseek(fd, *offset, SEEK_SET) < 0) {
        close(fd);
        return -1;
    }

    int out_fd = open(dst, O_WRONLY | O_CREAT | O_APPEND, 0666);
    if (out_fd < 0) {
        close(fd);
        return -1;
    }

    char buf[4096];
    ssize_t n;
    while ((n = read(fd, buf, sizeof(buf))) > 0) {
        if (write(out_fd, buf, (size_t)n) != n) {
            close(fd);
            close(out_fd);
            return -1;
        }
    }

    if (n < 0) {
        close(fd);
        close(out_fd);
        return -1;
    }

    *offset = st.st_size;
    close(fd);
    close(out_fd);
    return 0;
}

static int copy_file(const char *src, const char *dst) {
    int in_fd = open(src, O_RDONLY);
    if (in_fd < 0) return -1;
    int out_fd = open(dst, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (out_fd < 0) {
        close(in_fd);
        return -1;
    }

    char buf[4096];
    ssize_t n;
    while ((n = read(in_fd, buf, sizeof(buf))) > 0) {
        if (write(out_fd, buf, (size_t)n) != n) {
            close(in_fd);
            close(out_fd);
            return -1;
        }
    }

    close(in_fd);
    close(out_fd);
    return (n < 0) ? -1 : 0;
}

static void sync_requests(const char *dest_dir, const char *event_path) {
    DIR *dir = opendir(REQUESTS_DIR);
    if (!dir) return;

    struct dirent *ent;
    while ((ent = readdir(dir)) != NULL) {
        if (ent->d_name[0] == '.') continue;
        char src_path[512];
        char dst_path[512];
        snprintf(src_path, sizeof(src_path), "%s/%s", REQUESTS_DIR, ent->d_name);
        snprintf(dst_path, sizeof(dst_path), "%s/%s", dest_dir, ent->d_name);

        if (access(dst_path, F_OK) == 0) continue;
        if (copy_file(src_path, dst_path) == 0) {
            char msg[256];
            snprintf(msg, sizeof(msg), "request_copied:%s", ent->d_name);
            append_event(event_path, "request_copy", msg);
        }
    }
    closedir(dir);
}

int main(void) {
    signal(SIGINT, handle_stop);
    signal(SIGTERM, handle_stop);

    kill_previous_debugger();
    write_pid_file();

    notify_info("PS5 Upload Debugger", "Ready - capturing logs.");

    if (mkdir_p("/data/ps5upload", 0777) != 0) {
        notify_error("PS5 Upload Debugger", "Failed to create /data/ps5upload.");
        return 1;
    }
    mkdir_p(REQUESTS_DIR, 0777);
    cleanup_debug_root();
    if (mkdir_p(DEBUG_ROOT, 0777) != 0) {
        notify_error("PS5 Upload Debugger", "Failed to create debug directory.");
        return 1;
    }

    char stamp[64];
    timestamp_now(stamp, sizeof(stamp));

    char session_dir[512];
    snprintf(session_dir, sizeof(session_dir), "%s/session_%s_pid%d", DEBUG_ROOT, stamp, (int)getpid());
    if (mkdir_p(session_dir, 0777) != 0) {
        notify_error("PS5 Upload Debugger", "Failed to create session directory.");
        return 1;
    }

    char requests_dir[512];
    snprintf(requests_dir, sizeof(requests_dir), "%s/requests", session_dir);
    mkdir_p(requests_dir, 0777);

    char events_path[512];
    char status_path[512];
    char payload_log_path[512];
    char crash_log_path[512];
    char exit_log_path[512];

    snprintf(events_path, sizeof(events_path), "%s/events.ndjson", session_dir);
    snprintf(status_path, sizeof(status_path), "%s/status.ndjson", session_dir);
    snprintf(payload_log_path, sizeof(payload_log_path), "%s/payload.log", session_dir);
    snprintf(crash_log_path, sizeof(crash_log_path), "%s/payload_crash.log", session_dir);
    snprintf(exit_log_path, sizeof(exit_log_path), "%s/payload_exit.log", session_dir);

    char meta_path[512];
    snprintf(meta_path, sizeof(meta_path), "%s/session.json", session_dir);

    char meta[512];
    time_t now = time(NULL);
    snprintf(meta, sizeof(meta),
             "{\"version\":\"%s\",\"pid\":%d,\"started_at\":%ld,\"session\":\"%s\"}\n",
             PS5_UPLOAD_VERSION, (int)getpid(), (long)now, session_dir);
    append_text_line(meta_path, meta);

    append_event(events_path, "start", "debugger_started");

    off_t payload_offset = 0;
    off_t crash_offset = 0;
    off_t exit_offset = 0;

    int payload_up = 0;
    int failure_count = 0;

    long long last_status_ms = 0;
    long long last_tail_ms = 0;
    long long last_requests_ms = 0;

    while (!g_should_stop) {
        struct timespec ts;
        clock_gettime(CLOCK_MONOTONIC, &ts);
        long long now_ms = (long long)ts.tv_sec * 1000LL + (long long)(ts.tv_nsec / 1000000LL);

        if (now_ms - last_status_ms >= STATUS_POLL_INTERVAL_MS) {
            last_status_ms = now_ms;
            size_t json_len = 0;
            char *json = fetch_payload_status(&json_len);
            if (json) {
                append_status(status_path, json);
                if (!payload_up) {
                    append_event(events_path, "payload_recovered", "payload_status_ok");
                    notify_success("PS5 Upload Debugger", "Payload detected.");
                    payload_up = 1;
                }
                failure_count = 0;
                free(json);
            } else {
                failure_count++;
                if (payload_up && failure_count >= 2) {
                    append_event(events_path, "payload_unreachable", "payload_status_failed");
                    payload_up = 0;
                }
            }
        }

        if (now_ms - last_tail_ms >= LOG_TAIL_INTERVAL_MS) {
            last_tail_ms = now_ms;
            tail_file_incremental("/data/ps5upload/payload.log", payload_log_path, &payload_offset);
            tail_file_incremental("/data/ps5upload/payload_crash.log", crash_log_path, &crash_offset);
            tail_file_incremental("/data/ps5upload/payload_exit.log", exit_log_path, &exit_offset);
        }

        if (now_ms - last_requests_ms >= REQUEST_SYNC_INTERVAL_MS) {
            last_requests_ms = now_ms;
            sync_requests(requests_dir, events_path);
        }

        usleep(200000);
    }

    append_event(events_path, "stop", "debugger_stopped");
    notify_info("PS5 Upload Debugger", "Debug capture stopped.");
    unlink(DEBUGGER_PID_FILE);
    return 0;
}
