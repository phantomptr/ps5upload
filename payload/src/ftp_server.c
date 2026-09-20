#include "ftp_server.h"

#include "ftp_format.h"
#include "cross_device.h"

#include <stdio.h>
#include <stdlib.h>
#include <strings.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
#include <pthread.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <sys/stat.h>
#include <sys/mount.h>
#include <dirent.h>
#include <fcntl.h>
#include <stdatomic.h>

#include "proc_identity.h"
/* Max concurrent control sessions. Every accepted client must be tracked so
 * Stop can reliably tear it down; excess clients receive 421 instead of
 * becoming an orphan that survives a restart. */
#define FTP_MAX_SESSIONS 32

/* Forward — full struct is defined below. */
struct ftp_session;

static struct {
    _Atomic int running;
    _Atomic int connections;
    _Atomic int port;
    _Atomic int listen_fd;
    _Atomic unsigned int generation;
    char root[512];
    int readonly;
    char user[64];
    char pass[64];
    pthread_t thread;
    pthread_mutex_t lifecycle_mu;
    /* Live sessions for stop-to-kill. Protected by sessions_mu. */
    pthread_mutex_t sessions_mu;
    struct ftp_session *sessions[FTP_MAX_SESSIONS];
} g_ftp = {
    .running = 0,
    .connections = 0,
    .port = 0,
    .listen_fd = -1,
    .generation = 0,
    .root = "/",
    .readonly = 0,
    .user = {0},
    .pass = {0},
    .lifecycle_mu = PTHREAD_MUTEX_INITIALIZER,
    .sessions_mu = PTHREAD_MUTEX_INITIALIZER,
};

void ftp_server_init(void) {
    atomic_store(&g_ftp.running, 0);
    atomic_store(&g_ftp.connections, 0);
    atomic_store(&g_ftp.port, 0);
    atomic_store(&g_ftp.listen_fd, -1);
}

static int ip_is_safe(const char *ip) {
    if (strncmp(ip, "127.", 4) == 0) return 1;
    if (strncmp(ip, "10.", 3) == 0) return 1;
    if (strncmp(ip, "192.168.", 8) == 0) return 1;
    if (strncmp(ip, "172.", 4) == 0) {
        int second = atoi(ip + 4);
        if (second >= 16 && second <= 31) return 1;
        return 0;
    }
    return 1;
}

/* Bulk transfer buffer for RETR/STOR.
 *
 * Heap, never stack. This was `char buf[256 * 1024]` inside both
 * handlers, on session threads created with default attributes while
 * every other thread in the payload asks for 512 KiB-1 MiB explicitly
 * (see runtime.c). The first real file transfer overflowed the thread
 * stack and wedged the payload hard enough to need a console power
 * cycle — LIST and NLST survived only because their buffers are ~1 KiB. */
#define FTP_XFER_BUF (256 * 1024)

/* Session threads still get an explicit stack, so a future stack-hungry
 * handler cannot reintroduce the same failure silently. */
#define FTP_THREAD_STACK (512u * 1024u)

static void write_all(int fd, const void *data, size_t len) {
    const char *p = (const char *)data;
    while (len > 0) {
        ssize_t w = write(fd, p, len);
        if (w <= 0) {
            if (errno == EINTR) continue;
            break;
        }
        p += w;
        len -= (size_t)w;
    }
}

static void send_resp(int fd, int code, const char *msg) {
    char buf[1024];
    int n = snprintf(buf, sizeof(buf), "%d %s\r\n", code, msg);
    if (n > 0) write_all(fd, buf, (size_t)n);
}

struct ftp_session {
    _Atomic int ctrl_fd;
    _Atomic int data_fd;
    _Atomic int data_listen_fd;
    struct sockaddr_in data_addr;
    int data_offset;
    char cwd[512];
    char root[512];
    int readonly;
    int authenticated;
    char user[64];
    char pass[64];
    int use_pasv;
    char rename_path[512];
    char pending_user[64];
    char transfer_type;
    char line_buf[1024];
    size_t line_len;
    unsigned int generation;
    _Atomic int quit;
    _Atomic int abort_requested;
};

/* Atomically take ownership of a descriptor before closing it. Stop and the
 * session thread can race legitimately; exchange prevents a double-close from
 * hitting an unrelated socket after the descriptor number is reused. */
static void ftp_close_socket(_Atomic int *slot) {
    int fd = atomic_exchange(slot, -1);
    if (fd < 0) return;
    (void)shutdown(fd, SHUT_RDWR);
    close(fd);
}

static int ftp_session_register(struct ftp_session *s) {
    if (!s) return 0;
    int registered = 0;
    pthread_mutex_lock(&g_ftp.sessions_mu);
    /* A client can be accepted immediately before Stop and its thread may not
     * run until after a new server has started. Refuse that stale session so
     * it cannot attach itself to the next generation. */
    if (atomic_load(&g_ftp.running) &&
        atomic_load(&g_ftp.generation) == s->generation) {
        for (int i = 0; i < FTP_MAX_SESSIONS; i++) {
            if (g_ftp.sessions[i] == NULL) {
                g_ftp.sessions[i] = s;
                atomic_fetch_add(&g_ftp.connections, 1);
                registered = 1;
                break;
            }
        }
    }
    pthread_mutex_unlock(&g_ftp.sessions_mu);
    return registered;
}

static void ftp_session_unregister(struct ftp_session *s) {
    if (!s) return;
    pthread_mutex_lock(&g_ftp.sessions_mu);
    for (int i = 0; i < FTP_MAX_SESSIONS; i++) {
        if (g_ftp.sessions[i] == s) {
            g_ftp.sessions[i] = NULL;
            atomic_fetch_sub(&g_ftp.connections, 1);
            break;
        }
    }
    pthread_mutex_unlock(&g_ftp.sessions_mu);
}

/* Close every open session socket so Stop actually unblocks clients stuck in
 * RETR/STOR. The session threads still own their struct lifetime; atomic
 * descriptor exchange makes this safe against their normal cleanup path. */
static void ftp_kick_all_sessions(void) {
    pthread_mutex_lock(&g_ftp.sessions_mu);
    for (int i = 0; i < FTP_MAX_SESSIONS; i++) {
        struct ftp_session *s = g_ftp.sessions[i];
        if (!s) continue;
        atomic_store(&s->quit, 1);
        atomic_store(&s->abort_requested, 1);
        ftp_close_socket(&s->data_fd);
        ftp_close_socket(&s->data_listen_fd);
        ftp_close_socket(&s->ctrl_fd);
    }
    pthread_mutex_unlock(&g_ftp.sessions_mu);
}

static void normalize_path(const char *src, char *out, size_t cap) {
    char stack[64][256];
    int sp = 0;
    const char *p = src;
    while (*p) {
        while (*p == '/') p++;
        if (!*p) break;
        const char *start = p;
        while (*p && *p != '/') p++;
        size_t len = (size_t)(p - start);
        if (len == 1 && start[0] == '.') continue;
        if (len == 2 && start[0] == '.' && start[1] == '.') {
            if (sp > 0) sp--;
            continue;
        }
        if (sp < 64 && len < 256) {
            memcpy(stack[sp], start, len);
            stack[sp][len] = '\0';
            sp++;
        }
    }
    size_t pos = 0;
    if (cap > 1) { out[0] = '/'; pos = 1; }
    for (int i = 0; i < sp && pos + 1 < cap; i++) {
        size_t slen = strlen(stack[i]);
        if (pos + slen + 2 < cap) {
            if (pos > 1) out[pos++] = '/';
            memcpy(out + pos, stack[i], slen);
            pos += slen;
        }
    }
    if (pos < cap) out[pos] = '\0';
    else out[cap - 1] = '\0';
    if (pos == 1 && cap > 1) out[1] = '\0';
}

static void abs_path(struct ftp_session *s, const char *arg, char *out, size_t cap) {
    char virtual[1024];
    if (!arg || !arg[0]) {
        snprintf(virtual, sizeof(virtual), "%s", s->cwd);
    } else if (arg[0] == '/') {
        snprintf(virtual, sizeof(virtual), "%s", arg);
    } else {
        const char *cwd_rel = s->cwd + strlen(s->root);
        if (*cwd_rel == '\0') cwd_rel = "/";
        snprintf(virtual, sizeof(virtual), "%s/%s", cwd_rel, arg);
    }
    char normalized[1024];
    normalize_path(virtual, normalized, sizeof(normalized));
    size_t root_len = strlen(s->root);
    if (root_len <= 1) {
        snprintf(out, cap, "%s", normalized);
    } else {
        snprintf(out, cap, "%s%s", s->root, normalized);
    }
}

static void handle_user(struct ftp_session *s, const char *arg) {
    if (!arg) {
        send_resp(s->ctrl_fd, 501, "Syntax error");
        return;
    }
    strncpy(s->pending_user, arg, sizeof(s->pending_user) - 1);
    s->pending_user[sizeof(s->pending_user) - 1] = '\0';
    send_resp(s->ctrl_fd, 331, "User name okay, need password");
}

static void handle_pass(struct ftp_session *s, const char *arg) {
    if (s->pending_user[0] == '\0') {
        send_resp(s->ctrl_fd, 503, "Login with USER first");
        return;
    }
    if (s->user[0] != '\0') {
        if (strcmp(s->pending_user, s->user) != 0) {
            send_resp(s->ctrl_fd, 530, "Login incorrect");
            return;
        }
        const char *supplied = arg ? arg : "";
        if (s->pass[0] != '\0' && strcmp(supplied, s->pass) != 0) {
            send_resp(s->ctrl_fd, 530, "Login incorrect");
            return;
        }
    }
    s->authenticated = 1;
    send_resp(s->ctrl_fd, 230, "Login successful");
}

static void handle_syst(struct ftp_session *s) {
    send_resp(s->ctrl_fd, 215, "UNIX Type: L8");
}

static void handle_pwd(struct ftp_session *s) {
    const char *p = s->cwd + strlen(s->root);
    if (*p == '\0') p = "/";
    char msg[600];
    snprintf(msg, sizeof(msg), "\"%s\" is the current directory", p);
    send_resp(s->ctrl_fd, 257, msg);
}

static void handle_cwd(struct ftp_session *s, const char *arg) {
    char path[1024];
    abs_path(s, arg, path, sizeof(path));
    struct stat st;
    if (stat(path, &st) != 0 || !S_ISDIR(st.st_mode)) {
        send_resp(s->ctrl_fd, 550, "Failed to change directory");
        return;
    }
    snprintf(s->cwd, sizeof(s->cwd), "%s", path);
    send_resp(s->ctrl_fd, 250, "Directory successfully changed");
}

static void handle_cdup(struct ftp_session *s) {
    char path[1024];
    abs_path(s, "..", path, sizeof(path));
    struct stat st;
    if (stat(path, &st) != 0 || !S_ISDIR(st.st_mode)) {
        send_resp(s->ctrl_fd, 550, "Failed to change directory");
        return;
    }
    snprintf(s->cwd, sizeof(s->cwd), "%s", path);
    send_resp(s->ctrl_fd, 250, "Directory successfully changed");
}

static void handle_type(struct ftp_session *s, const char *arg) {
    if (!arg || !arg[0]) {
        send_resp(s->ctrl_fd, 501, "Syntax error in parameters");
        return;
    }
    char t = arg[0];
    if (t == 'A' || t == 'a') {
        s->transfer_type = 'A';
        send_resp(s->ctrl_fd, 200, "Type set to A");
    } else if (t == 'I' || t == 'i') {
        s->transfer_type = 'I';
        send_resp(s->ctrl_fd, 200, "Type set to I");
    } else {
        send_resp(s->ctrl_fd, 504, "Type not supported");
    }
}

static void open_data_connection(struct ftp_session *s) {
    if (s->data_fd >= 0) return;
    if (s->data_listen_fd >= 0) {
        struct timeval tv;
        tv.tv_sec = 30;
        tv.tv_usec = 0;
        fd_set fds;
        FD_ZERO(&fds);
        FD_SET(s->data_listen_fd, &fds);
        int rv = select(s->data_listen_fd + 1, &fds, NULL, NULL, &tv);
        if (rv > 0) {
            struct sockaddr_in addr;
            socklen_t addrlen = sizeof(addr);
            s->data_fd = accept(s->data_listen_fd, (struct sockaddr *)&addr, &addrlen);
            if (s->data_fd >= 0) {
                ftp_close_socket(&s->data_listen_fd);
                int opt = 0x100000;
                setsockopt(s->data_fd, SOL_SOCKET, SO_SNDBUF, &opt, sizeof(opt));
                setsockopt(s->data_fd, SOL_SOCKET, SO_RCVBUF, &opt, sizeof(opt));
            }
        } else {
            ftp_close_socket(&s->data_listen_fd);
        }
    } else if (s->data_addr.sin_port) {
        s->data_fd = socket(AF_INET, SOCK_STREAM, 0);
        if (s->data_fd >= 0) {
            struct timeval tv;
            tv.tv_sec = 30;
            tv.tv_usec = 0;
            setsockopt(s->data_fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv));
            if (connect(s->data_fd, (struct sockaddr *)&s->data_addr,
                        sizeof(s->data_addr)) < 0) {
                ftp_close_socket(&s->data_fd);
            } else {
                int opt = 0x100000;
                setsockopt(s->data_fd, SOL_SOCKET, SO_SNDBUF, &opt, sizeof(opt));
                setsockopt(s->data_fd, SOL_SOCKET, SO_RCVBUF, &opt, sizeof(opt));
            }
        }
    }
}

static int require_auth(struct ftp_session *s) {
    if (s->user[0] == '\0') return 1;
    if (s->authenticated) return 1;
    send_resp(s->ctrl_fd, 530, "Not logged in");
    return 0;
}

/* Shared body of LIST and NLST. `names_only` selects NLST's bare-name
 * output; everything else about the exchange is identical. */
static void send_listing(struct ftp_session *s, int names_only) {
    if (!require_auth(s)) return;
    DIR *d = opendir(s->cwd);
    if (!d) {
        send_resp(s->ctrl_fd, 550, "Failed to open directory");
        return;
    }
    open_data_connection(s);
    if (s->data_fd < 0) {
        closedir(d);
        send_resp(s->ctrl_fd, 425, "Use PASV or PORT first");
        return;
    }
    send_resp(s->ctrl_fd, 150, "Here comes the directory listing");

    struct dirent *ent;
    char linebuf[1024];
    while ((ent = readdir(d)) != NULL) {
        if (names_only) {
            /* NLST names are meant to be usable with RETR, so the dot
             * entries have no place in it. */
            if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0)
                continue;
            int n = snprintf(linebuf, sizeof(linebuf), "%s\r\n", ent->d_name);
            if (n > 0 && (size_t)n < sizeof(linebuf))
                write_all(s->data_fd, linebuf, (size_t)n);
            continue;
        }

        char fullpath[512];
        snprintf(fullpath, sizeof(fullpath), "%s/%s", s->cwd, ent->d_name);
        struct stat st;
        if (stat(fullpath, &st) != 0) continue;
        char timestr[64];
        strftime(timestr, sizeof(timestr), "%b %d %H:%M", localtime(&st.st_mtime));
        int n = ftp_format_list_line(S_ISDIR(st.st_mode) ? 1 : 0,
                                     (unsigned int)st.st_mode,
                                     (long long)st.st_size, timestr,
                                     ent->d_name, linebuf,
                                     sizeof(linebuf) - 2);
        if (n < 0) continue;
        linebuf[n] = '\r';
        linebuf[n + 1] = '\n';
        write_all(s->data_fd, linebuf, (size_t)n + 2);
    }
    closedir(d);
    ftp_close_socket(&s->data_fd);
    send_resp(s->ctrl_fd, 226, "Directory send OK");
}

static void handle_list(struct ftp_session *s) { send_listing(s, 0); }

static void handle_nlst(struct ftp_session *s) { send_listing(s, 1); }

static void handle_retr(struct ftp_session *s, const char *arg) {
    if (!require_auth(s)) return;
    if (!arg) {
        send_resp(s->ctrl_fd, 501, "Filename required");
        return;
    }
    char path[512];
    abs_path(s, arg, path, sizeof(path));
    atomic_store(&s->abort_requested, 0);
    open_data_connection(s);
    if (s->data_fd < 0) {
        s->data_offset = 0;
        send_resp(s->ctrl_fd, 425, "Use PASV or PORT first");
        return;
    }
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        ftp_close_socket(&s->data_fd);
        s->data_offset = 0;
        send_resp(s->ctrl_fd, 550, "Failed to open file");
        return;
    }
    if (s->data_offset > 0) {
        if (lseek(fd, s->data_offset, SEEK_SET) < 0) {
            close(fd);
            ftp_close_socket(&s->data_fd);
            s->data_offset = 0;
            send_resp(s->ctrl_fd, 550, "Failed to seek");
            return;
        }
    }
    char *buf = (char *)malloc(FTP_XFER_BUF);
    if (!buf) {
        close(fd);
        ftp_close_socket(&s->data_fd);
        s->data_offset = 0;
        send_resp(s->ctrl_fd, 451, "Out of memory");
        return;
    }
    send_resp(s->ctrl_fd, 150, "Opening BINARY mode data connection");
    ssize_t n;
    int aborted = 0;
    while ((n = read(fd, buf, FTP_XFER_BUF)) > 0) {
        if (atomic_load(&s->abort_requested) || s->data_fd < 0) { aborted = 1; break; }
        write_all(s->data_fd, buf, (size_t)n);
    }
    free(buf);
    close(fd);
    ftp_close_socket(&s->data_fd);
    s->data_offset = 0;
    atomic_store(&s->abort_requested, 0);
    if (!aborted) send_resp(s->ctrl_fd, 226, "Transfer complete");
}

static void handle_stor(struct ftp_session *s, const char *arg) {
    if (!require_auth(s)) return;
    if (s->readonly) {
        send_resp(s->ctrl_fd, 550, "Read-only server");
        return;
    }
    if (!arg) {
        send_resp(s->ctrl_fd, 501, "Filename required");
        return;
    }
    char path[512];
    abs_path(s, arg, path, sizeof(path));
    atomic_store(&s->abort_requested, 0);
    open_data_connection(s);
    if (s->data_fd < 0) {
        s->data_offset = 0;
        send_resp(s->ctrl_fd, 425, "Use PASV or PORT first");
        return;
    }
    int fd;
    if (s->data_offset > 0) {
        fd = open(path, O_WRONLY | O_CREAT, 0644);
        if (fd >= 0) {
            if (lseek(fd, s->data_offset, SEEK_SET) < 0) {
                close(fd);
                ftp_close_socket(&s->data_fd);
                s->data_offset = 0;
                send_resp(s->ctrl_fd, 550, "Failed to seek");
                return;
            }
        }
    } else {
        fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    }
    if (fd < 0) {
        ftp_close_socket(&s->data_fd);
        s->data_offset = 0;
        send_resp(s->ctrl_fd, 550, "Failed to open file for writing");
        return;
    }
    char *buf = (char *)malloc(FTP_XFER_BUF);
    if (!buf) {
        close(fd);
        ftp_close_socket(&s->data_fd);
        s->data_offset = 0;
        send_resp(s->ctrl_fd, 451, "Out of memory");
        return;
    }
    send_resp(s->ctrl_fd, 150, "Opening BINARY mode data connection");
    ssize_t n;
    off_t total = s->data_offset;
    int aborted = 0;
    while ((n = read(s->data_fd, buf, FTP_XFER_BUF)) > 0) {
        if (atomic_load(&s->abort_requested)) { aborted = 1; break; }
        write_all(fd, buf, (size_t)n);
        total += n;
    }
    free(buf);
    if (!aborted) ftruncate(fd, total);
    close(fd);
    ftp_close_socket(&s->data_fd);
    s->data_offset = 0;
    atomic_store(&s->abort_requested, 0);
    if (!aborted) send_resp(s->ctrl_fd, 226, "Transfer complete");
}

static void handle_feat(struct ftp_session *s) {
    char buf[512];
    int n = snprintf(buf, sizeof(buf),
        "211-Features:\r\n"
        " UTF8\r\n"
        " EPSV\r\n"
        " MLSD\r\n"
        " REST STREAM\r\n"
        " SIZE\r\n"
        " MDTM\r\n"
        " ABOR\r\n"
        " SITE MTRW\r\n"
        "211 End\r\n");
    if (n > 0) write_all(s->ctrl_fd, buf, (size_t)n);
}

static void handle_mlsd(struct ftp_session *s) {
    if (!require_auth(s)) return;
    DIR *d = opendir(s->cwd);
    if (!d) {
        send_resp(s->ctrl_fd, 550, "Failed to open directory");
        return;
    }
    open_data_connection(s);
    if (s->data_fd < 0) {
        closedir(d);
        send_resp(s->ctrl_fd, 425, "Use PASV or PORT first");
        return;
    }
    send_resp(s->ctrl_fd, 150, "Here comes the directory listing");

    struct dirent *ent;
    char linebuf[1024];
    while ((ent = readdir(d)) != NULL) {
        char fullpath[600];
        snprintf(fullpath, sizeof(fullpath), "%s/%s", s->cwd, ent->d_name);
        struct stat st;
        if (stat(fullpath, &st) != 0) continue;
        char timestr[32];
        strftime(timestr, sizeof(timestr), "%Y%m%d%H%M%S",
                 localtime(&st.st_mtime));
        int n = snprintf(linebuf, sizeof(linebuf),
            "type=%s;size=%lld;modify=%s; %s\r\n",
            S_ISDIR(st.st_mode) ? "dir" : "file",
            (long long)st.st_size, timestr, ent->d_name);
        if (n > 0) write_all(s->data_fd, linebuf, (size_t)n);
    }
    closedir(d);
    ftp_close_socket(&s->data_fd);
    send_resp(s->ctrl_fd, 226, "Directory send OK");
}

static void handle_rest(struct ftp_session *s, const char *arg) {
    if (!require_auth(s)) return;
    if (!arg || !arg[0]) {
        send_resp(s->ctrl_fd, 501, "REST requires an offset");
        return;
    }
    long offset = atol(arg);
    if (offset < 0) offset = 0;
    s->data_offset = (int)offset;
    send_resp(s->ctrl_fd, 350, "Ready to resume at given offset");
}

static void handle_rnfr(struct ftp_session *s, const char *arg) {
    if (!require_auth(s)) return;
    if (s->readonly) {
        send_resp(s->ctrl_fd, 550, "Read-only server");
        return;
    }
    if (!arg) {
        send_resp(s->ctrl_fd, 501, "Filename required");
        return;
    }
    char path[512];
    abs_path(s, arg, path, sizeof(path));
    struct stat st;
    if (stat(path, &st) != 0) {
        send_resp(s->ctrl_fd, 550, "File not found");
        return;
    }
    snprintf(s->rename_path, sizeof(s->rename_path), "%s", path);
    send_resp(s->ctrl_fd, 350, "Awaiting new name");
}

static void handle_rnto(struct ftp_session *s, const char *arg) {
    if (!require_auth(s)) return;
    if (s->readonly) {
        send_resp(s->ctrl_fd, 550, "Read-only server");
        return;
    }
    if (!s->rename_path[0]) {
        send_resp(s->ctrl_fd, 503, "Use RNFR first");
        return;
    }
    if (!arg) {
        send_resp(s->ctrl_fd, 501, "Filename required");
        return;
    }
    char path[512];
    abs_path(s, arg, path, sizeof(path));
    /* A cross-DEVICE rename() does not return EXDEV on this kernel — it
     * panics the console. An FTP client dragging a file from /mnt/usb0
     * to /data is an ordinary thing to do, so refuse it here rather than
     * let the kernel take the machine down. 553 tells the client the
     * name was disallowed; copy-then-delete is the safe alternative and
     * every client can do it. */
    if (xdev_rename_crosses(s->rename_path, path, xdev_stat_dev)
        == XDEV_CROSSES) {
        s->rename_path[0] = '\0';
        send_resp(s->ctrl_fd, 553,
                  "Cannot rename across devices - copy then delete instead");
        return;
    }
    if (rename(s->rename_path, path) != 0) {
        s->rename_path[0] = '\0';
        send_resp(s->ctrl_fd, 550, "Failed to rename");
        return;
    }
    s->rename_path[0] = '\0';
    send_resp(s->ctrl_fd, 250, "Rename successful");
}

static void handle_port(struct ftp_session *s, const char *arg) {
    if (!require_auth(s)) return;
    if (!arg) {
        send_resp(s->ctrl_fd, 501, "PORT requires arguments");
        return;
    }
    unsigned int h[6];
    if (sscanf(arg, "%u,%u,%u,%u,%u,%u",
               &h[0], &h[1], &h[2], &h[3], &h[4], &h[5]) != 6) {
        send_resp(s->ctrl_fd, 501, "Invalid PORT format");
        return;
    }
    unsigned int ip_addr = (h[3] << 24) | (h[2] << 16) | (h[1] << 8) | h[0];
    unsigned short port = (unsigned short)((h[5] << 8) | h[4]);

    struct sockaddr_in peer;
    socklen_t peerlen = sizeof(peer);
    if (getpeername(s->ctrl_fd, (struct sockaddr *)&peer, &peerlen) == 0) {
        if (peer.sin_addr.s_addr != htonl(ip_addr)) {
            send_resp(s->ctrl_fd, 500, "PORT address does not match client");
            return;
        }
    }

    if (s->data_listen_fd >= 0) {
        ftp_close_socket(&s->data_listen_fd);
    }
    if (s->data_fd >= 0) {
        ftp_close_socket(&s->data_fd);
    }
    memset(&s->data_addr, 0, sizeof(s->data_addr));
    s->data_addr.sin_family = AF_INET;
    s->data_addr.sin_addr.s_addr = htonl(ip_addr);
    s->data_addr.sin_port = htons(port);
    send_resp(s->ctrl_fd, 200, "PORT command successful");
}

static void handle_opts(struct ftp_session *s, const char *arg) {
    if (arg && strncasecmp(arg, "UTF8", 4) == 0) {
        send_resp(s->ctrl_fd, 200, "Always in UTF8 mode");
    } else {
        send_resp(s->ctrl_fd, 501, "OPTS not supported");
    }
}

/* Bind an ephemeral passive data socket and start listening.
 * Returns the bound port, or -1. Shared by PASV and EPSV, which differ
 * only in how that port is reported back to the client. */
static int open_passive_socket(struct ftp_session *s) {
    if (s->data_listen_fd >= 0) {
        ftp_close_socket(&s->data_listen_fd);
    }
    if (s->data_fd >= 0) {
        ftp_close_socket(&s->data_fd);
    }
    s->data_addr.sin_port = 0;

    int sock = socket(AF_INET, SOCK_STREAM, 0);
    if (sock < 0) return -1;

    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_ANY);
    addr.sin_port = 0;
    if (bind(sock, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        close(sock);
        return -1;
    }
    listen(sock, 1);
    socklen_t addrlen = sizeof(addr);
    getsockname(sock, (struct sockaddr *)&addr, &addrlen);
    s->data_listen_fd = sock;
    return ntohs(addr.sin_port);
}

static void handle_pasv(struct ftp_session *s) {
    if (!require_auth(s)) return;
    int port = open_passive_socket(s);
    if (port < 0) {
        send_resp(s->ctrl_fd, 425, "Cannot open passive socket");
        return;
    }

    struct sockaddr_in ctrl_addr;
    socklen_t ctrl_addrlen = sizeof(ctrl_addr);
    getsockname(s->ctrl_fd, (struct sockaddr *)&ctrl_addr, &ctrl_addrlen);

    unsigned char *ip = (unsigned char *)&ctrl_addr.sin_addr.s_addr;
    char msg[128];
    if (ftp_format_pasv(ip, port, msg, sizeof(msg)) < 0) {
        send_resp(s->ctrl_fd, 425, "Cannot open passive socket");
        return;
    }
    send_resp(s->ctrl_fd, 227, msg);
}

/* RFC 2428 extended passive mode. Most modern clients try EPSV before
 * PASV; answering 502 made the well-behaved ones fall back and left the
 * rest looking like they had hung. */
static void handle_epsv(struct ftp_session *s, const char *arg) {
    if (!require_auth(s)) return;

    switch (ftp_parse_epsv_arg(arg)) {
    case FTP_EPSV_ALL:
        /* The client is promising to use only EPSV from here on. We have
         * nothing to tear down, so just accept. */
        send_resp(s->ctrl_fd, 200, "EPSV ALL OK");
        return;
    case FTP_EPSV_BAD_PROTO:
        send_resp(s->ctrl_fd, 522, "Network protocol not supported, use (1)");
        return;
    case FTP_EPSV_IPV4:
        break;
    }

    int port = open_passive_socket(s);
    if (port < 0) {
        send_resp(s->ctrl_fd, 425, "Cannot open passive socket");
        return;
    }
    char msg[128];
    if (ftp_format_epsv(port, msg, sizeof(msg)) < 0) {
        send_resp(s->ctrl_fd, 425, "Cannot open passive socket");
        return;
    }
    send_resp(s->ctrl_fd, 229, msg);
}

static void handle_size(struct ftp_session *s, const char *arg) {
    if (!require_auth(s)) return;
    if (!arg) {
        send_resp(s->ctrl_fd, 501, "Filename required");
        return;
    }
    char path[512];
    abs_path(s, arg, path, sizeof(path));
    struct stat st;
    if (stat(path, &st) != 0) {
        send_resp(s->ctrl_fd, 550, "File not found");
        return;
    }
    char msg[64];
    snprintf(msg, sizeof(msg), "%lld", (long long)st.st_size);
    send_resp(s->ctrl_fd, 213, msg);
}

static void handle_mdtm(struct ftp_session *s, const char *arg) {
    if (!require_auth(s)) return;
    if (!arg) {
        send_resp(s->ctrl_fd, 501, "Filename required");
        return;
    }
    char path[512];
    abs_path(s, arg, path, sizeof(path));
    struct stat st;
    if (stat(path, &st) != 0) {
        send_resp(s->ctrl_fd, 550, "File not found");
        return;
    }
    struct tm *tm = gmtime(&st.st_mtime);
    if (!tm) {
        send_resp(s->ctrl_fd, 550, "Failed to get time");
        return;
    }
    char buf[32];
    strftime(buf, sizeof(buf), "%Y%m%d%H%M%S", tm);
    char msg[64];
    snprintf(msg, sizeof(msg), "%s", buf);
    send_resp(s->ctrl_fd, 213, msg);
}

static void handle_mkd(struct ftp_session *s, const char *arg) {
    if (!require_auth(s)) return;
    if (s->readonly) {
        send_resp(s->ctrl_fd, 550, "Read-only server");
        return;
    }
    if (!arg) {
        send_resp(s->ctrl_fd, 501, "Directory name required");
        return;
    }
    char path[512];
    abs_path(s, arg, path, sizeof(path));
    if (mkdir(path, 0755) != 0) {
        send_resp(s->ctrl_fd, 550, "Failed to create directory");
        return;
    }
    send_resp(s->ctrl_fd, 257, "Directory created");
}

static void handle_dele(struct ftp_session *s, const char *arg) {
    if (!require_auth(s)) return;
    if (s->readonly) {
        send_resp(s->ctrl_fd, 550, "Read-only server");
        return;
    }
    if (!arg) {
        send_resp(s->ctrl_fd, 501, "Filename required");
        return;
    }
    char path[512];
    abs_path(s, arg, path, sizeof(path));
    if (unlink(path) != 0) {
        send_resp(s->ctrl_fd, 550, "Failed to delete file");
        return;
    }
    send_resp(s->ctrl_fd, 250, "File deleted");
}

static void handle_rmd(struct ftp_session *s, const char *arg) {
    if (!require_auth(s)) return;
    if (s->readonly) {
        send_resp(s->ctrl_fd, 550, "Read-only server");
        return;
    }
    if (!arg) {
        send_resp(s->ctrl_fd, 501, "Directory name required");
        return;
    }
    char path[512];
    abs_path(s, arg, path, sizeof(path));
    if (rmdir(path) != 0) {
        send_resp(s->ctrl_fd, 550, "Failed to remove directory");
        return;
    }
    send_resp(s->ctrl_fd, 250, "Directory removed");
}

static void handle_site(struct ftp_session *s, const char *args) {
    if (!require_auth(s)) return;
    if (!args || !args[0]) {
        send_resp(s->ctrl_fd, 501, "SITE requires a subcommand");
        return;
    }

    char subcmd[16] = {0};
    size_t i = 0;
    while (args[i] && args[i] != ' ' && i < sizeof(subcmd) - 1) {
        subcmd[i] = args[i];
        i++;
    }
    subcmd[i] = '\0';

    if (strcasecmp(subcmd, "MTRW") == 0) {
#ifdef PS5UPLOAD_FTP_HOST_SELFTEST
        /* Sony/FreeBSD nmount is unavailable on Linux/macOS. Lifecycle tests
         * exercise the socket server, not privileged console remounting. */
        send_resp(s->ctrl_fd, 550, "MTRW unavailable in host self-test");
#else
        const char *targets[] = {"/preinst", "/system", "/system_ex", NULL};
        struct statfs *mnts = NULL;
        int nm = getmntinfo(&mnts, MNT_NOWAIT);
        int ok_count = 0;
        char detail[256];
        detail[0] = '\0';

        for (int t = 0; targets[t]; t++) {
            const char *fstype = NULL;
            const char *from = NULL;
            for (int j = 0; j < nm; j++) {
                if (strcmp(mnts[j].f_mntonname, targets[t]) == 0) {
                    fstype = mnts[j].f_fstypename;
                    from = mnts[j].f_mntfromname;
                    break;
                }
            }
            if (!fstype || !from) continue;

            struct iovec iov[6];
            iov[0].iov_base = (void *)"fstype";
            iov[0].iov_len = strlen("fstype") + 1;
            iov[1].iov_base = (void *)fstype;
            iov[1].iov_len = strlen(fstype) + 1;
            iov[2].iov_base = (void *)"fspath";
            iov[2].iov_len = strlen("fspath") + 1;
            iov[3].iov_base = (void *)targets[t];
            iov[3].iov_len = strlen(targets[t]) + 1;
            iov[4].iov_base = (void *)"from";
            iov[4].iov_len = strlen("from") + 1;
            iov[5].iov_base = (void *)from;
            iov[5].iov_len = strlen(from) + 1;

            if (nmount(iov, 6, MNT_UPDATE) == 0) {
                ok_count++;
                size_t cur = strlen(detail);
                snprintf(detail + cur, sizeof(detail) - cur, " %s", targets[t]);
            }
        }

        if (ok_count > 0) {
            char msg[300];
            snprintf(msg, sizeof(msg), "Remounted rw:%s", detail);
            send_resp(s->ctrl_fd, 200, msg);
        } else {
            send_resp(s->ctrl_fd, 550, "MTRW failed (need kernel R/W?)");
        }
#endif
        return;
    }

    send_resp(s->ctrl_fd, 504, "SITE subcommand not implemented");
}

static void process_command(struct ftp_session *s, char *line) {
    char *cmd = line;
    char *space = strchr(cmd, ' ');
    char *args = NULL;
    if (space) {
        *space = '\0';
        args = space + 1;
    }

    if (strcasecmp(cmd, "USER") == 0) handle_user(s, args);
    else if (strcasecmp(cmd, "PASS") == 0) handle_pass(s, args);
    else if (strcasecmp(cmd, "QUIT") == 0) { send_resp(s->ctrl_fd, 221, "Goodbye"); atomic_store(&s->quit, 1); }
    else if (strcasecmp(cmd, "SYST") == 0) handle_syst(s);
    else if (strcasecmp(cmd, "FEAT") == 0) handle_feat(s);
    else if (strcasecmp(cmd, "OPTS") == 0) handle_opts(s, args);
    else if (strcasecmp(cmd, "PWD") == 0) { if (require_auth(s)) handle_pwd(s); }
    else if (strcasecmp(cmd, "CWD") == 0) { if (require_auth(s)) handle_cwd(s, args); }
    else if (strcasecmp(cmd, "CDUP") == 0) { if (require_auth(s)) handle_cdup(s); }
    else if (strcasecmp(cmd, "TYPE") == 0) { if (require_auth(s)) handle_type(s, args); }
    else if (strcasecmp(cmd, "LIST") == 0) handle_list(s);
    else if (strcasecmp(cmd, "NLST") == 0) handle_nlst(s);
    else if (strcasecmp(cmd, "MLSD") == 0) handle_mlsd(s);
    else if (strcasecmp(cmd, "RETR") == 0) handle_retr(s, args);
    else if (strcasecmp(cmd, "STOR") == 0) handle_stor(s, args);
    else if (strcasecmp(cmd, "REST") == 0) handle_rest(s, args);
    else if (strcasecmp(cmd, "RNFR") == 0) handle_rnfr(s, args);
    else if (strcasecmp(cmd, "RNTO") == 0) handle_rnto(s, args);
    else if (strcasecmp(cmd, "PORT") == 0) handle_port(s, args);
    else if (strcasecmp(cmd, "PASV") == 0) handle_pasv(s);
    else if (strcasecmp(cmd, "EPSV") == 0) handle_epsv(s, args);
    else if (strcasecmp(cmd, "SIZE") == 0) handle_size(s, args);
    else if (strcasecmp(cmd, "MDTM") == 0) handle_mdtm(s, args);
    else if (strcasecmp(cmd, "MKD") == 0) handle_mkd(s, args);
    else if (strcasecmp(cmd, "DELE") == 0) handle_dele(s, args);
    else if (strcasecmp(cmd, "RMD") == 0) handle_rmd(s, args);
    else if (strcasecmp(cmd, "NOOP") == 0) send_resp(s->ctrl_fd, 200, "OK");
    else if (strcasecmp(cmd, "SITE") == 0) handle_site(s, args);
    else if (strcasecmp(cmd, "ABOR") == 0) {
        if (s->data_fd >= 0) {
            atomic_store(&s->abort_requested, 1);
            ftp_close_socket(&s->data_fd);
            send_resp(s->ctrl_fd, 426, "Transfer aborted");
            send_resp(s->ctrl_fd, 226, "ABOR successful");
        } else {
            send_resp(s->ctrl_fd, 226, "ABOR successful");
        }
    }
    else send_resp(s->ctrl_fd, 502, "Command not implemented");
}

static void *ftp_client_thread(void *arg) {
    /* Name this thread: the process listing shows a representative
     * thread that is not reliably main, so an unnamed worker makes the
     * whole process read as "payload.elf". See proc_identity.h. */
    proc_name_set_self(PS5UPLOAD2_PROC_NAME);
    struct ftp_session *s = (struct ftp_session *)arg;
    if (!ftp_session_register(s)) {
        send_resp(s->ctrl_fd, 421, "Too many FTP connections");
        ftp_close_socket(&s->ctrl_fd);
        free(s);
        return NULL;
    }
    struct timeval tv;
    tv.tv_sec = 300;
    tv.tv_usec = 0;
    setsockopt(s->ctrl_fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));

    send_resp(s->ctrl_fd, 220, "PS5 FTP Server ready");

    char buf[1024];
    while (atomic_load(&g_ftp.running) &&
           atomic_load(&g_ftp.generation) == s->generation &&
           !atomic_load(&s->quit)) {
        ssize_t n = read(s->ctrl_fd, buf, sizeof(buf));
        if (n <= 0) break;

        for (ssize_t i = 0; i < n; i++) {
            if (s->line_len >= sizeof(s->line_buf) - 1) {
                s->line_len = 0;
            }
            char c = buf[i];
            if (c == '\r') continue;
            if (c == '\n') {
                s->line_buf[s->line_len] = '\0';
                process_command(s, s->line_buf);
                if (atomic_load(&s->quit) ||
                    !atomic_load(&g_ftp.running) ||
                    atomic_load(&g_ftp.generation) != s->generation) goto done;
                s->line_len = 0;
            } else {
                s->line_buf[s->line_len++] = c;
            }
        }
    }
done:

    /* Keep the session registered until every descriptor is detached. Stop
     * holds the registry lock while walking pointers, so it can never observe
     * a freed struct. */
    ftp_close_socket(&s->data_fd);
    ftp_close_socket(&s->data_listen_fd);
    ftp_close_socket(&s->ctrl_fd);
    ftp_session_unregister(s);
    free(s);
    return NULL;
}

struct ftp_listener_ctx {
    int listen_fd;
    unsigned int generation;
    char root[512];
    int readonly;
    char user[64];
    char pass[64];
};

static void *ftp_listen_thread(void *arg) {
    /* Name this thread: the process listing shows a representative
     * thread that is not reliably main, so an unnamed worker makes the
     * whole process read as "payload.elf". See proc_identity.h. */
    proc_name_set_self(PS5UPLOAD2_PROC_NAME);
    struct ftp_listener_ctx *ctx = (struct ftp_listener_ctx *)arg;
    int listen_fd = ctx->listen_fd;
    unsigned int generation = ctx->generation;
    while (atomic_load(&g_ftp.running) &&
           atomic_load(&g_ftp.generation) == generation) {
        struct sockaddr_in client_addr;
        socklen_t addrlen = sizeof(client_addr);
        int client_fd = accept(listen_fd, (struct sockaddr *)&client_addr, &addrlen);
        if (client_fd < 0) {
            if (!atomic_load(&g_ftp.running) ||
                atomic_load(&g_ftp.generation) != generation) break;
            continue;
        }

        /* A Stop followed immediately by Start may reuse the same numeric
         * descriptor. Generation ownership prevents the old listener from
         * accepting clients for the new server instance. */
        if (!atomic_load(&g_ftp.running) ||
            atomic_load(&g_ftp.generation) != generation) {
            close(client_fd);
            break;
        }

        char ip[INET_ADDRSTRLEN];
        inet_ntop(AF_INET, &client_addr.sin_addr, ip, sizeof(ip));
        if (!ip_is_safe(ip)) {
            close(client_fd);
            continue;
        }

        struct ftp_session *s = calloc(1, sizeof(*s));
        if (!s) {
            close(client_fd);
            continue;
        }
        atomic_store(&s->ctrl_fd, client_fd);
        atomic_store(&s->data_fd, -1);
        atomic_store(&s->data_listen_fd, -1);
        s->data_offset = 0;
        s->transfer_type = 'I';
        s->line_len = 0;
        s->generation = generation;
        atomic_store(&s->quit, 0);
        atomic_store(&s->abort_requested, 0);
        s->rename_path[0] = '\0';
        s->pending_user[0] = '\0';
        snprintf(s->cwd, sizeof(s->cwd), "%s", ctx->root);
        snprintf(s->root, sizeof(s->root), "%s", ctx->root);
        s->readonly = ctx->readonly;
        strncpy(s->user, ctx->user, sizeof(s->user) - 1);
        s->user[sizeof(s->user) - 1] = '\0';
        strncpy(s->pass, ctx->pass, sizeof(s->pass) - 1);
        s->pass[sizeof(s->pass) - 1] = '\0';

        pthread_t tid;
        pthread_attr_t attr;
        pthread_attr_init(&attr);
        (void)pthread_attr_setstacksize(&attr, FTP_THREAD_STACK);
        if (pthread_create(&tid, &attr, ftp_client_thread, s) != 0) {
            close(client_fd);
            free(s);
        } else {
            pthread_detach(tid);
        }
        pthread_attr_destroy(&attr);
    }
    free(ctx);
    return NULL;
}

int ftp_server_start(int port, const char *root, int readonly,
                     const char *user, const char *pass,
                     char *resp, size_t cap, size_t *written) {
    if (port == 0) {
        return ftp_server_stop(resp, cap, written);
    }
    if (port < 1 || port > 65535) {
        int n = snprintf(resp, cap,
            "{\"ok\":false,\"error\":\"invalid_port\",\"port\":%d}", port);
        if (written) *written = (size_t)(n > 0 ? n : 0);
        return 0;
    }

    pthread_mutex_lock(&g_ftp.lifecycle_mu);
    if (atomic_load(&g_ftp.running)) {
        int n = snprintf(resp, cap,
            "{\"ok\":false,\"error\":\"already_running\",\"port\":%d}",
            atomic_load(&g_ftp.port));
        if (written) *written = (size_t)(n > 0 ? n : 0);
        pthread_mutex_unlock(&g_ftp.lifecycle_mu);
        return 0;
    }

    /* Do not reuse numeric descriptors while a stopped session can still
     * have an in-flight read/write using the old value. Session registration
     * and this check share sessions_mu, and stale-generation clients cannot
     * register after the new generation starts. */
    pthread_mutex_lock(&g_ftp.sessions_mu);
    int draining = atomic_load(&g_ftp.connections) > 0;
    pthread_mutex_unlock(&g_ftp.sessions_mu);
    if (draining) {
        int n = snprintf(resp, cap,
            "{\"ok\":false,\"error\":\"stopping\",\"connections\":%d}",
            atomic_load(&g_ftp.connections));
        if (written) *written = (size_t)(n > 0 ? n : 0);
        pthread_mutex_unlock(&g_ftp.lifecycle_mu);
        return 0;
    }

    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        int n = snprintf(resp, cap, "{\"ok\":false,\"error\":\"socket_failed\"}");
        if (written) *written = (size_t)(n > 0 ? n : 0);
        pthread_mutex_unlock(&g_ftp.lifecycle_mu);
        return 0;
    }
    int opt = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));

    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_ANY);
    addr.sin_port = htons((uint16_t)port);
    if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        close(fd);
        int n = snprintf(resp, cap, "{\"ok\":false,\"error\":\"bind_failed\",\"port\":%d}", port);
        if (written) *written = (size_t)(n > 0 ? n : 0);
        pthread_mutex_unlock(&g_ftp.lifecycle_mu);
        return 0;
    }
    if (listen(fd, 5) < 0) {
        close(fd);
        int n = snprintf(resp, cap, "{\"ok\":false,\"error\":\"listen_failed\"}");
        if (written) *written = (size_t)(n > 0 ? n : 0);
        pthread_mutex_unlock(&g_ftp.lifecycle_mu);
        return 0;
    }

    g_ftp.readonly = readonly;
    if (root && root[0]) {
        strncpy(g_ftp.root, root, sizeof(g_ftp.root) - 1);
        g_ftp.root[sizeof(g_ftp.root) - 1] = '\0';
    } else {
        strcpy(g_ftp.root, "/");
    }
    if (user && user[0]) {
        strncpy(g_ftp.user, user, sizeof(g_ftp.user) - 1);
        g_ftp.user[sizeof(g_ftp.user) - 1] = '\0';
    } else {
        g_ftp.user[0] = '\0';
    }
    if (pass && pass[0]) {
        strncpy(g_ftp.pass, pass, sizeof(g_ftp.pass) - 1);
        g_ftp.pass[sizeof(g_ftp.pass) - 1] = '\0';
    } else {
        g_ftp.pass[0] = '\0';
    }

    struct ftp_listener_ctx *ctx = calloc(1, sizeof(*ctx));
    if (!ctx) {
        close(fd);
        int n = snprintf(resp, cap, "{\"ok\":false,\"error\":\"out_of_memory\"}");
        if (written) *written = (size_t)(n > 0 ? n : 0);
        pthread_mutex_unlock(&g_ftp.lifecycle_mu);
        return 0;
    }
    ctx->listen_fd = fd;
    ctx->generation = atomic_fetch_add(&g_ftp.generation, 1) + 1;
    snprintf(ctx->root, sizeof(ctx->root), "%s", g_ftp.root);
    ctx->readonly = g_ftp.readonly;
    snprintf(ctx->user, sizeof(ctx->user), "%s", g_ftp.user);
    snprintf(ctx->pass, sizeof(ctx->pass), "%s", g_ftp.pass);

    atomic_store(&g_ftp.listen_fd, fd);
    atomic_store(&g_ftp.port, port);
    atomic_store(&g_ftp.running, 1);

    if (pthread_create(&g_ftp.thread, NULL, ftp_listen_thread, ctx) != 0) {
        atomic_store(&g_ftp.running, 0);
        atomic_fetch_add(&g_ftp.generation, 1);
        ftp_close_socket(&g_ftp.listen_fd);
        atomic_store(&g_ftp.port, 0);
        free(ctx);
        int n = snprintf(resp, cap, "{\"ok\":false,\"error\":\"thread_failed\"}");
        if (written) *written = (size_t)(n > 0 ? n : 0);
        pthread_mutex_unlock(&g_ftp.lifecycle_mu);
        return 0;
    }
    pthread_detach(g_ftp.thread);

    int n = snprintf(resp, cap, "{\"ok\":true,\"port\":%d,\"root\":\"%s\"}",
                     port, g_ftp.root);
    if (written) *written = (size_t)(n > 0 ? n : 0);
    pthread_mutex_unlock(&g_ftp.lifecycle_mu);
    return 0;
}

int ftp_server_stop(char *resp, size_t cap, size_t *written) {
    pthread_mutex_lock(&g_ftp.lifecycle_mu);
    int was = atomic_load(&g_ftp.running) ? 1 : 0;
    /* Always tear down sockets even if the flag is already false — a
     * wedged accept or orphaned session used to leave the port open while
    * status reported stopped, so Stop looked broken. */
    atomic_store(&g_ftp.running, 0);
    atomic_fetch_add(&g_ftp.generation, 1);
    ftp_close_socket(&g_ftp.listen_fd);
    ftp_kick_all_sessions();
    atomic_store(&g_ftp.port, 0);
    int n = snprintf(resp, cap,
        "{\"ok\":true,\"port\":0,\"was_running\":%s}",
        was ? "true" : "false");
    if (written) *written = (size_t)(n > 0 ? n : 0);
    pthread_mutex_unlock(&g_ftp.lifecycle_mu);
    return 0;
}

int ftp_server_status(char *resp, size_t cap, size_t *written) {
    pthread_mutex_lock(&g_ftp.lifecycle_mu);
    int n = snprintf(resp, cap,
        "{\"running\":%s,\"port\":%d,\"connections\":%d,\"root\":\"%s\"}",
        atomic_load(&g_ftp.running) ? "true" : "false",
        atomic_load(&g_ftp.port),
        atomic_load(&g_ftp.connections),
        g_ftp.root);
    if (written) *written = (size_t)(n > 0 ? n : 0);
    pthread_mutex_unlock(&g_ftp.lifecycle_mu);
    return 0;
}
