/* Extract an archive on the console: the helper starts the extractor payload and follows it. See
 * include/archive_extract.h for the operation and payload/extractor/README.md for the extractor. */
#include "archive_extract.h"

#include <arpa/inet.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <time.h>
#include <unistd.h>

#include "archive_name.h"
#include "ava1_app_gen.h"
#include "ava1_op.h"
#include "fs_jobs.h"
#include "path_policy.h"
#include "runtime.h"
#include "sigpipe_guard.h"
#include "webui_codec.h"

/* The extractor ELF, embedded by payload/Makefile (xxd of payload/extractor/ps5upload-extractor.elf). */
extern unsigned char ps5upload_extractor_elf[];
extern unsigned int ps5upload_extractor_elf_len;

#define EXTRACT_REQUEST EXTRACT_DIR "/request.json"
#define EXTRACT_STATUS EXTRACT_DIR "/status.json"
#define EXTRACT_CANCEL EXTRACT_DIR "/cancel"
#define EXTRACT_RECORD EXTRACT_DIR "/job.rec"

#define EXTRACT_LOADER_PORT 9021
/* The extractor's XJ_PASSWORD (its own build; the two must agree): room for 255 bytes and the NUL. */
#define EXTRACT_PASSWORD_MAX 256
/* elfldr is closed for about a second while ps5upload swaps it for its patched build, and for 3 s
 * after a failed accept: a refused connect is retried this long. */
#define EXTRACT_CONNECT_RETRY_MS 8000
/* From the ELF sent to the extractor's first status. */
#define EXTRACT_START_MS 20000
#define EXTRACT_POLL_MS 200
/* After Stop: the extractor stops within one buffer; this long without it ending, it is killed. */
#define EXTRACT_KILL_AFTER_MS 10000

static pthread_mutex_t g_busy = PTHREAD_MUTEX_INITIALIZER;

static uint64_t now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u;
}

static void sleep_ms(unsigned ms) { usleep(ms * 1000u); }

/* The writable roots (runtime.c), as the data layer's may_write hook applies them; usable at
 * startup too, before the data layer is configured. */
static int may_write_here(const char *p) { return is_path_allowed(p) && !path_tree_op_refused(p); }

static int read_small(const char *path, char *buf, size_t cap) {
    int fd = open(path, O_RDONLY);
    size_t at = 0;
    ssize_t n;
    if (fd < 0) return -1;
    while (at + 1 < cap && (n = read(fd, buf + at, cap - 1 - at)) > 0) at += (size_t)n;
    close(fd);
    buf[at] = 0;
    return (int)at;
}

/* `s` as a JSON string with its quotes, appended at *at. -1 when it does not fit. */
static int put_jstr(char *out, size_t cap, size_t *at, const char *s) {
    size_t need;
    if (*at + 2 >= cap) return -1;
    out[(*at)++] = '"';
    need = webui_json_escape(out + *at, cap - *at, s, strlen(s));
    if (need + 2 >= cap - *at) return -1;
    *at += need;
    out[(*at)++] = '"';
    out[*at] = 0;
    return 0;
}

static int write_request(const char *id, const char *src, const char *dest, const char *password, int merge, int replace) {
    char *buf = malloc(8192);
    size_t at = 0;
    int fd, rc = -1;
    if (!buf) return -1;
    at = (size_t)snprintf(buf, 8192, "{\"id\":\"%s\",\"archive\":", id);
    if (put_jstr(buf, 8192, &at, src) != 0) goto out;
    at += (size_t)snprintf(buf + at, 8192 - at, ",\"dest\":");
    if (put_jstr(buf, 8192, &at, dest) != 0) goto out;
    if (password) {
        at += (size_t)snprintf(buf + at, 8192 - at, ",\"password\":");
        if (put_jstr(buf, 8192, &at, password) != 0) goto out;
    }
    at += (size_t)snprintf(buf + at, 8192 - at, ",\"merge\":%s,\"replace\":%s}\n", merge ? "true" : "false",
                           replace ? "true" : "false");
    if (at >= 8192) goto out;
    (void)unlink(EXTRACT_REQUEST);
    fd = open(EXTRACT_REQUEST, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd < 0) goto out;
    rc = write(fd, buf, at) == (ssize_t)at ? 0 : -1;
    close(fd);
out:
    memset(buf, 0, 8192); /* the password */
    free(buf);
    return rc;
}

/* Sends the extractor to the console's own ELF loader. 0, or -1 (the loader did not answer). */
static int launch_extractor(void) {
    uint64_t until = now_ms() + EXTRACT_CONNECT_RETRY_MS;
    for (;;) {
        struct sockaddr_in sa;
        struct timeval tv = {10, 0};
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        if (fd < 0) return -1;
        (void)setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof tv);
        memset(&sa, 0, sizeof sa);
        sa.sin_family = AF_INET;
        sa.sin_port = htons(EXTRACT_LOADER_PORT);
        sa.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        if (connect(fd, (struct sockaddr *)&sa, sizeof sa) == 0) {
            const unsigned char *p = ps5upload_extractor_elf;
            size_t left = ps5upload_extractor_elf_len;
            while (left) {
                ssize_t w = send(fd, p, left, MSG_NOSIGNAL);
                if (w < 0 && errno == EINTR) continue;
                if (w <= 0) break;
                p += w;
                left -= (size_t)w;
            }
            /* elfldr reads the ELF by its own sizes; closing promptly never leaves it waiting. */
            shutdown(fd, SHUT_WR);
            close(fd);
            return left ? -1 : 0;
        }
        close(fd);
        if (now_ms() >= until) return -1;
        sleep_ms(250);
    }
}

typedef struct {
    char state[16];
    char message[400];
    char current[256];
    uint64_t pid, files_done, files_total, bytes_done, bytes_total;
    uint64_t files, dirs, bytes, skipped, unsafe, links, created;
} xstatus_t;

/* The extractor's status for job `id`: 1 read, 0 none yet (or another job's). */
static int read_status(const char *id, xstatus_t *s) {
    char buf[4096], got[64];
    if (read_small(EXTRACT_STATUS, buf, sizeof buf) <= 0) return 0;
    if (!fsj_json_str(buf, "id", got, sizeof got) || strcmp(got, id) != 0) return 0;
    memset(s, 0, sizeof *s);
    if (!fsj_json_str(buf, "state", s->state, sizeof s->state)) return 0;
    (void)fsj_json_str(buf, "message", s->message, sizeof s->message);
    (void)fsj_json_str(buf, "current", s->current, sizeof s->current);
    (void)fsj_json_u64(buf, "pid", &s->pid);
    (void)fsj_json_u64(buf, "files_done", &s->files_done);
    (void)fsj_json_u64(buf, "files_total", &s->files_total);
    (void)fsj_json_u64(buf, "bytes_done", &s->bytes_done);
    (void)fsj_json_u64(buf, "bytes_total", &s->bytes_total);
    (void)fsj_json_u64(buf, "files", &s->files);
    (void)fsj_json_u64(buf, "dirs", &s->dirs);
    (void)fsj_json_u64(buf, "bytes", &s->bytes);
    (void)fsj_json_u64(buf, "skipped", &s->skipped);
    (void)fsj_json_u64(buf, "unsafe", &s->unsafe);
    (void)fsj_json_u64(buf, "links", &s->links);
    (void)fsj_json_u64(buf, "created_dest", &s->created);
    return 1;
}

static int pid_alive(uint64_t pid) { return pid > 1 && (kill((pid_t)pid, 0) == 0 || errno == EPERM); }

/* The extractor's temporary files (".ps5u-x-<pid>-*.part") left in `dir` when it was killed. */
static void sweep_parts(const char *dir, uint64_t pid, int depth) {
    char prefix[48], path[1400];
    DIR *d;
    struct dirent *e;
    struct stat st;
    if (depth > 64 || !(d = opendir(dir))) return;
    snprintf(prefix, sizeof prefix, ".ps5u-x-%llu-", (unsigned long long)pid);
    while ((e = readdir(d)) != NULL) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        if ((size_t)snprintf(path, sizeof path, "%s/%s", dir, e->d_name) >= sizeof path) continue;
        if (lstat(path, &st) != 0) continue;
        if (S_ISDIR(st.st_mode)) sweep_parts(path, pid, depth + 1);
        else if (S_ISREG(st.st_mode) && strncmp(e->d_name, prefix, strlen(prefix)) == 0) (void)unlink(path);
    }
    closedir(d);
}

static void write_record(uint64_t xpid, int created, const char *dest) {
    FILE *f = fopen(EXTRACT_RECORD, "w");
    if (!f) return;
    fprintf(f, "%llu %ld %d\n%s\n", (unsigned long long)xpid, (long)getpid(), created, dest);
    fclose(f);
}

void archive_extract_sweep_request(void) {
    struct stat st;
    if (stat(EXTRACT_REQUEST, &st) != 0 || !archive_extract_request_stale(st.st_mtime, time(NULL))) return;
    fprintf(stderr, "extract: removing a request no extractor read\n");
    (void)unlink(EXTRACT_REQUEST);
}

void archive_extract_sweep(void) {
    char buf[1400], dest[1100];
    unsigned long long xpid = 0;
    long hpid = 0;
    int created = 0;
    archive_extract_sweep_request();
    if (read_small(EXTRACT_RECORD, buf, sizeof buf) <= 0) return;
    dest[0] = 0;
    if (sscanf(buf, "%llu %ld %d\n%1023[^\n]", &xpid, &hpid, &created, dest) < 3) {
        (void)unlink(EXTRACT_RECORD);
        return;
    }
    /* The helper that wrote it, or its extractor, still runs (a takeover lets the old one finish). */
    if ((hpid != (long)getpid() && pid_alive((uint64_t)hpid)) || pid_alive(xpid)) return;
    if (dest[0] == '/' && may_write_here(dest)) {
        if (created && !fsj_path_is_root(dest)) {
            fprintf(stderr, "extract: a helper died mid-extraction; removing the folder it made: %s\n", dest);
            (void)fsj_rm_rf(dest, 0, NULL);
        } else if (xpid) {
            sweep_parts(dest, xpid, 0);
        }
    }
    (void)unlink(EXTRACT_RECORD);
}

/* <dir>/<archive_folder_name(base)> for `src`. 0 or -1. */
static int default_dest(const char *src, char *name, size_t name_cap, char *dest, size_t dest_cap) {
    const char *slash = strrchr(src, '/');
    if (!slash || archive_folder_name(slash + 1, name, name_cap) != 0) return -1;
    if ((size_t)snprintf(dest, dest_cap, "%.*s/%s", (int)(slash - src), src, name) >= dest_cap) return -1;
    return 0;
}

const char *archive_extract_probe(const char *path, archive_probe_t *p) {
    char src[1024], vol[1100];
    struct stat st;
    struct statvfs sv;
    const char *slash;
    memset(p, 0, sizeof *p);
    if (snprintf(src, sizeof src, "%s", path) >= (int)sizeof src) return "fs_extract_path_not_allowed";
    if (fsj_normalize_path(src) != 0 || !may_write_here(src)) return "fs_extract_path_not_allowed";
    slash = strrchr(src, '/');
    p->kind = archive_kind(slash + 1);
    if (!p->kind) return "fs_extract_not_archive";
    if (stat(src, &st) != 0 || !S_ISREG(st.st_mode)) return "fs_extract_open_failed";
    if (default_dest(src, p->name, sizeof p->name, p->dest, sizeof p->dest) != 0) return "fs_extract_bad_dest";
    for (unsigned k = 0; k < 1000; k++) {
        if (archive_volume_name(src, k, vol, sizeof vol) != 0 || stat(vol, &st) != 0 || !S_ISREG(st.st_mode)) break;
        p->size += (uint64_t)st.st_size;
        p->volumes++;
    }
    p->exists = lstat(p->dest, &st) == 0;
    {
        char dir[1024];
        snprintf(dir, sizeof dir, "%.*s", (int)(slash - src), src);
        if (statvfs(dir[0] ? dir : "/", &sv) == 0) {
            p->free = (uint64_t)sv.f_bavail * sv.f_frsize;
            p->have_free = 1;
        }
    }
    return NULL;
}

/* Follows the started extractor until it ends. 1 done, 0 failed (s says why), -1 it never started
 * or died (message set). */
static int follow(ava1_op_ctx_t *c, const char *id, xstatus_t *s, const char *dest) {
    uint64_t started = now_ms(), cancel_at = 0, files_seen = 0, bytes_seen = 0, tot_files = 0, tot_bytes = 0;
    uint64_t rec_pid = 0;
    int have = 0, cancel_sent = 0, rec_created = -1;
    for (;;) {
        xstatus_t cur;
        if (read_status(id, &cur)) {
            have = 1;
            /* What a helper that dies now leaves behind, for the next start's sweep. */
            if (cur.pid != rec_pid || (int)cur.created != rec_created) {
                rec_pid = cur.pid;
                rec_created = (int)cur.created;
                write_record(rec_pid, rec_created, dest);
            }
            *s = cur;
            if (cur.files_total != tot_files || cur.bytes_total != tot_bytes) {
                tot_files = cur.files_total;
                tot_bytes = cur.bytes_total;
                ava1_op_set_total(c, tot_files, tot_bytes);
            }
            if (cur.files_done > files_seen || cur.bytes_done > bytes_seen) {
                ava1_op_add(c, cur.files_done > files_seen ? cur.files_done - files_seen : 0,
                            cur.bytes_done > bytes_seen ? cur.bytes_done - bytes_seen : 0);
                if (cur.files_done > files_seen) files_seen = cur.files_done;
                if (cur.bytes_done > bytes_seen) bytes_seen = cur.bytes_done;
            }
            if (strcmp(cur.state, "running") != 0) return strcmp(cur.state, "done") == 0 ? 1 : 0;
            if (cur.current[0]) ava1_op_message(c, "extracting %s", cur.current);
        }
        if (!cancel_sent && ava1_op_cancelled(c)) {
            int fd = open(EXTRACT_CANCEL, O_WRONLY | O_CREAT, 0600);
            if (fd >= 0) close(fd);
            cancel_sent = 1;
            cancel_at = now_ms();
        }
        if (!have) {
            if (now_ms() - started > EXTRACT_START_MS) {
                ava1_op_message(c, "fs_extract_not_started");
                return -1;
            }
        } else if (!pid_alive(s->pid)) {
            /* It may have ended between the read and this check: one more read. */
            xstatus_t last;
            if (read_status(id, &last) && strcmp(last.state, "running") != 0) {
                *s = last;
                return strcmp(last.state, "done") == 0 ? 1 : 0;
            }
            ava1_op_message(c, "fs_extract_died");
            sweep_parts(dest, s->pid, 0);
            return -1;
        } else if (cancel_sent && now_ms() - cancel_at > EXTRACT_KILL_AFTER_MS) {
            fprintf(stderr, "extract: the extractor (pid %llu) did not stop; killing it\n", (unsigned long long)s->pid);
            (void)kill((pid_t)s->pid, SIGKILL);
            sleep_ms(500);
            sweep_parts(dest, s->pid, 0);
            ava1_op_message(c, "fs_extract_cancelled");
            return -1;
        }
        sleep_ms(EXTRACT_POLL_MS);
    }
}

int archive_extract_op(void *arg, ava1_op_ctx_t *c, const uint8_t *args, size_t n) {
    char src[1024], dest[1024], name[256], id[40], *password = NULL;
    const char *json = (const char *)args;
    const char *slash;
    uint64_t merge = 0, replace = 0;
    struct stat st;
    xstatus_t s;
    int dest_existed, status = AVA1_ERR_IO, r;
    (void)arg;
    (void)n;
    memset(&s, 0, sizeof s);
    if (!fsj_json_str(json, "path", src, sizeof src) || !src[0]) {
        ava1_op_message(c, "fs_extract_missing_path");
        return AVA1_ERR_PROTOCOL;
    }
    if (fsj_normalize_path(src) != 0 || !may_write_here(src) || strncmp(src, "/mnt/shadowmnt/", 15) == 0) {
        ava1_op_message(c, "fs_extract_path_not_allowed");
        return AVA1_ERR_PATH;
    }
    slash = strrchr(src, '/');
    if (!archive_kind(slash + 1)) {
        ava1_op_message(c, "fs_extract_not_archive");
        return AVA1_ERR_PATH;
    }
    if (stat(src, &st) != 0 || !S_ISREG(st.st_mode)) {
        ava1_op_message(c, "fs_extract_open_failed");
        return AVA1_ERR_PATH;
    }
    if (!fsj_json_str(json, "dest", dest, sizeof dest) || !dest[0]) {
        if (default_dest(src, name, sizeof name, dest, sizeof dest) != 0) {
            ava1_op_message(c, "fs_extract_bad_dest");
            return AVA1_ERR_PATH;
        }
    }
    {
        /* The destination: allowed, not a drive's root, inside a folder that exists. */
        char parent[1024];
        const char *ps;
        if (fsj_normalize_path(dest) != 0 || !may_write_here(dest) || fsj_path_is_root(dest) ||
            strncmp(dest, "/mnt/shadowmnt/", 15) == 0) {
            ava1_op_message(c, "fs_extract_path_not_allowed");
            return AVA1_ERR_PATH;
        }
        ps = strrchr(dest, '/');
        snprintf(parent, sizeof parent, "%.*s", (int)(ps - dest), dest);
        if (!parent[0] || stat(parent, &st) != 0 || !S_ISDIR(st.st_mode)) {
            ava1_op_message(c, "fs_extract_bad_dest");
            return AVA1_ERR_PATH;
        }
    }
    (void)fsj_json_u64(json, "merge", &merge);
    (void)fsj_json_u64(json, "replace", &replace);
    dest_existed = lstat(dest, &st) == 0;
    if (dest_existed && !merge) {
        ava1_op_message(c, "fs_extract_destination_exists");
        return AVA1_ERR_PATH;
    }
    if (dest_existed && !S_ISDIR(st.st_mode)) {
        ava1_op_message(c, "fs_extract_bad_dest");
        return AVA1_ERR_PATH;
    }
    if (pthread_mutex_trylock(&g_busy) != 0) {
        ava1_op_message(c, "fs_extract_busy");
        return AVA1_ERR_BUSY;
    }
    password = malloc(1024);
    if (!password) {
        ava1_op_message(c, "fs_extract_no_memory");
        status = AVA1_ERR_IO;
        goto unlock;
    }
    if (!fsj_json_str(json, "password", password, 1024)) {
        free(password);
        password = NULL;
    }
    /* The extractor keeps a password in EXTRACT_PASSWORD_MAX bytes (XJ_PASSWORD,
     * payload/extractor/src/xjob.h) and cannot take a longer one: this accepted up to 1023, wrote
     * it to the request, and the extractor was started only to refuse the request it could not
     * read. A password that long is not one it can open anything with, so say so before anything
     * is written to disk or loaded. */
    if (password && strlen(password) >= EXTRACT_PASSWORD_MAX) {
        ava1_op_message(c, "fs_extract_wrong_password");
        status = AVA1_ERR_IO;
        goto unlock;
    }
    (void)mkdir(EXTRACT_DIR, 0700);
    archive_extract_sweep();
    {
        /* A previous extractor still running (its helper was replaced): one at a time. */
        char buf[4096];
        uint64_t old = 0;
        char state[16];
        if (read_small(EXTRACT_STATUS, buf, sizeof buf) > 0 && fsj_json_str(buf, "state", state, sizeof state) &&
            strcmp(state, "running") == 0 && fsj_json_u64(buf, "pid", &old) && pid_alive(old)) {
            ava1_op_message(c, "fs_extract_busy");
            status = AVA1_ERR_BUSY;
            goto unlock;
        }
    }
    (void)unlink(EXTRACT_STATUS);
    (void)unlink(EXTRACT_CANCEL);
    {
        uint8_t rnd[8];
        arc4random_buf(rnd, sizeof rnd);
        snprintf(id, sizeof id, "%02x%02x%02x%02x%02x%02x%02x%02x", rnd[0], rnd[1], rnd[2], rnd[3], rnd[4], rnd[5],
                 rnd[6], rnd[7]);
    }
    if (write_request(id, src, dest, password, (int)merge, (int)replace) != 0) {
        ava1_op_message(c, "fs_extract_write_failed: request");
        goto unlock;
    }
    ava1_op_message(c, "starting the extractor");
    if (launch_extractor() != 0) {
        (void)unlink(EXTRACT_REQUEST);
        ava1_op_message(c, "fs_extract_loader_unreachable");
        status = AVA1_ERR_IO;
        goto unlock;
    }
    r = follow(c, id, &s, dest);
    (void)unlink(EXTRACT_REQUEST); /* never started: the password is not left on disk */
    if (r == 1) {
        char resp[1600];
        size_t at = 0;
        at = (size_t)snprintf(resp, sizeof resp, "{\"path\":");
        if (put_jstr(resp, sizeof resp, &at, dest) == 0) {
            at += (size_t)snprintf(resp + at, sizeof resp - at,
                                   ",\"files\":%llu,\"dirs\":%llu,\"bytes\":%llu,\"skipped\":%llu,\"unsafe\":%llu,\"links\":%llu}",
                                   (unsigned long long)s.files, (unsigned long long)s.dirs, (unsigned long long)s.bytes,
                                   (unsigned long long)s.skipped, (unsigned long long)s.unsafe, (unsigned long long)s.links);
            if (at < sizeof resp) (void)ava1_op_set_result(c, resp, at);
        }
        ava1_op_message(c, "%s", "");
        status = AVA1_STATUS_OK;
    } else {
        if (r == 0) ava1_op_message(c, "%s", s.message[0] ? s.message : "fs_extract_corrupt");
        status = (r == 0 && strncmp(s.message, "fs_extract_cancelled", 20) == 0) || ava1_op_cancelled(c) ? AVA1_ERR_CANCELLED
                                                                                                         : AVA1_ERR_IO;
        if (status == AVA1_ERR_CANCELLED) ava1_op_message(c, "fs_extract_cancelled");
        /* Nothing kept from a run that made the folder; a merge keeps what it wrote. */
        if (!dest_existed && lstat(dest, &st) == 0 && S_ISDIR(st.st_mode)) {
            fprintf(stderr, "extract: %s failed (%s); removing %s\n", src, s.message[0] ? s.message : "-", dest);
            (void)fsj_rm_rf(dest, 0, NULL);
        }
    }
    (void)unlink(EXTRACT_RECORD);
    (void)unlink(EXTRACT_STATUS);
    (void)unlink(EXTRACT_CANCEL);
unlock:
    if (password) {
        memset(password, 0, 1024);
        free(password);
    }
    pthread_mutex_unlock(&g_busy);
    return status;
}

void archive_extract_register(void) { (void)ava1_op_register(AVA1_JOB_OP_ARCHIVE_EXTRACT, archive_extract_op, NULL); }
