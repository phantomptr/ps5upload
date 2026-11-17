/* Convert a game on the console: the helper starts the converter payload and follows it. See
 * include/console_convert.h for the operation; the shape is archive_extract.c's. */
#include "console_convert.h"

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
#include <sys/sysctl.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

#include "archive_extract.h"
#include "ava1_app_gen.h"
#include "ava1_op.h"
#include "fs_jobs.h"
#include "proc_identity.h"
#include "sony_api_lock.h"
#include "webui_codec.h"

/* runtime.c / path_policy.c / webui_pkg.c (their headers pull in the whole helper; the host test
 * supplies these four itself). */
int is_path_allowed(const char *p);
int path_tree_op_refused(const char *p);
int webui_pkg_installing(void);
extern int sceSystemServicePowerTick(void);
/* proc_list.c: the process's name (its thread name, kinfo_proc). 0 read, non-zero gone or unread. */
int proc_name_by_pid(int pid, char *out, size_t cap);

/* The converter ELF, embedded by payload/Makefile (xxd of payload/convert/ps5upload-convert.elf).
 * Length 0 in a helper built without it (the ELF needs Docker to build: `make console-convert`). */
extern unsigned char ps5upload_convert_elf[];
extern unsigned int ps5upload_convert_elf_len;

#define CONVERT_LOADER_PORT 9021
/* elfldr is closed for about a second while ps5upload swaps it for its patched build, and for 3 s
 * after a failed accept: a refused connect is retried this long. */
#define CONVERT_CONNECT_RETRY_MS 8000
/* From the ELF sent to the converter's first status. */
#ifndef CONVERT_START_MS
#define CONVERT_START_MS 20000
#endif
#define CONVERT_POLL_MS 200
/* After Stop: the converter stops at its next check; this long without it ending, it is killed. */
#ifndef CONVERT_KILL_AFTER_MS
#define CONVERT_KILL_AFTER_MS 10000
#endif
/* The console's idle timer is reset this often while a build runs, so it does not enter rest
 * mode on its own in the middle of one (rest mode ends every payload). */
#ifndef CONVERT_POWER_TICK_MS
#define CONVERT_POWER_TICK_MS 30000
#endif

static pthread_mutex_t g_busy = PTHREAD_MUTEX_INITIALIZER;
static int g_building;

#ifdef CONSOLE_CONVERT_SELFTEST
const char *console_convert_test_dir;
int (*console_convert_test_launch)(void);
const char *console_convert_test_runtime_dir;
unsigned long long console_convert_test_boot;
int (*console_convert_test_proc_name)(int pid, char *out, size_t cap);
#endif

typedef enum { CF_REQUEST, CF_STATUS, CF_CANCEL, CF_RECORD, CF_PENDING } cfile_t;

/* When the console started (kern.boottime, wall-clock seconds), or 0 when unknown. */
static unsigned long long boot_time(void) {
    struct timeval bt;
    size_t len = sizeof bt;
    int mib[2] = {CTL_KERN, KERN_BOOTTIME};
#ifdef CONSOLE_CONVERT_SELFTEST
    if (console_convert_test_boot) return console_convert_test_boot;
#endif
    if (sysctl(mib, 2, &bt, &len, NULL, 0) != 0 || len < sizeof bt || bt.tv_sec <= 0) return 0;
    return (unsigned long long)bt.tv_sec;
}

/* A file of the job folder. */
static const char *cpath(cfile_t f, char *out, size_t cap) {
    static const char *const names[] = {"request.json", "status.json", "cancel", "job.rec", "pending.rec"};
    const char *dir = CONVERT_DIR;
#ifdef CONSOLE_CONVERT_SELFTEST
    if (console_convert_test_dir) dir = console_convert_test_dir;
#endif
    snprintf(out, cap, "%s/%s", dir, names[f]);
    return out;
}

static const char *cdir(void) {
#ifdef CONSOLE_CONVERT_SELFTEST
    if (console_convert_test_dir) return console_convert_test_dir;
#endif
    return CONVERT_DIR;
}

static const char *runtime_dir(void) {
#ifdef CONSOLE_CONVERT_SELFTEST
    if (console_convert_test_runtime_dir) return console_convert_test_runtime_dir;
#endif
    return CONVERT_RUNTIME_DIR;
}

/* The desktop app writes each runtime through a temporary name and renames it in place, so a
 * folder whose libSceAmpr.sprx is there holds a whole one. */
int console_convert_find_runtime(cc_runtime_t *rt) {
    char ver[CC_RUNTIME_VERSION_MAX], file[CC_RUNTIME_PATH_MAX];
    const char *dir = runtime_dir();
    struct dirent *e;
    struct stat st;
    int found = 0;
    DIR *d = opendir(dir);
    rt->version[0] = 0;
    rt->path[0] = 0;
    if (!d) return 0;
    while ((e = readdir(d)) != NULL) {
        int w;
        if (!cc_runtime_folder_version(e->d_name, ver, sizeof ver)) continue;
        w = snprintf(file, sizeof file, "%s/%s/%s", dir, e->d_name, CC_RUNTIME_FILE);
        if (w < 0 || (size_t)w >= sizeof file) continue;
        if (stat(file, &st) != 0 || !S_ISREG(st.st_mode) || st.st_size <= 0) continue;
        if (found && !cc_version_older(rt->version, ver)) continue;
        memcpy(rt->version, ver, sizeof rt->version);
        memcpy(rt->path, file, sizeof rt->path);
        found = 1;
    }
    closedir(d);
    return found;
}

static uint64_t now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u;
}

static void sleep_ms(unsigned ms) { usleep(ms * 1000u); }

/* The writable roots (runtime.c), as the data layer's may_write hook applies them. */
static int may_write_here(const char *p) { return is_path_allowed(p) && !path_tree_op_refused(p); }

/* Up to cap - 1 bytes of `path`, NUL-terminated. The length, or -1 when it cannot be opened. A
 * file as large as the buffer is not one of ours: -1. */
static int read_small(const char *path, char *buf, size_t cap) {
    int fd = open(path, O_RDONLY);
    size_t at = 0;
    ssize_t n;
    if (fd < 0) return -1;
    while (at + 1 < cap && (n = read(fd, buf + at, cap - 1 - at)) > 0) at += (size_t)n;
    close(fd);
    buf[at] = 0;
    return at + 1 >= cap ? -1 : (int)at;
}

static int write_request(const char *id, const char *op, const char *path, const cc_build_t *b) {
    char file[160], *buf = malloc(8192);
    int len, fd, rc = -1;
    if (!buf) return -1;
    len = cc_request_json(buf, 8192, id, op, path, b);
    if (len > 0) {
        (void)unlink(cpath(CF_REQUEST, file, sizeof file));
        fd = open(file, O_WRONLY | O_CREAT | O_EXCL, 0600);
        if (fd >= 0) {
            rc = write(fd, buf, (size_t)len) == (ssize_t)len ? 0 : -1;
            close(fd);
        }
    }
    free(buf);
    return rc;
}

#ifndef CONSOLE_CONVERT_SELFTEST
/* Sends the converter to the console's own ELF loader. 0, or -1 (the loader did not answer). */
static int launch_converter(void) {
    uint64_t until = now_ms() + CONVERT_CONNECT_RETRY_MS;
    for (;;) {
        struct sockaddr_in sa;
        struct timeval tv = {10, 0};
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        if (fd < 0) return -1;
        (void)setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof tv);
        memset(&sa, 0, sizeof sa);
        sa.sin_family = AF_INET;
        sa.sin_port = htons(CONVERT_LOADER_PORT);
        sa.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        if (connect(fd, (struct sockaddr *)&sa, sizeof sa) == 0) {
            const unsigned char *p = ps5upload_convert_elf;
            size_t left = ps5upload_convert_elf_len;
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
#endif

static int launch(void) {
#ifdef CONSOLE_CONVERT_SELFTEST
    return console_convert_test_launch ? console_convert_test_launch() : -1;
#else
    return launch_converter();
#endif
}

/* The converter's status for job `id` into `buf` (which s->result points into): 1 read, 0 none
 * yet, another job's, or a file caught half replaced (the next poll reads it again). */
static int read_status(const char *id, char *buf, size_t cap, cc_status_t *s) {
    char file[160];
    int n = read_small(cpath(CF_STATUS, file, sizeof file), buf, cap);
    return n > 0 && cc_status_parse(buf, (size_t)n, id, s);
}

/* A pid read from a file: one that is not cc_pid_ok, or is this helper's own, is never signalled
 * and counts as not running. */
static int pid_signallable(uint64_t pid) { return cc_pid_ok(pid) && (pid_t)pid != getpid(); }

static int pid_alive(uint64_t pid) {
    return pid_signallable(pid) && (kill((pid_t)pid, 0) == 0 || errno == EPERM);
}

/* 1 when live process `pid` is shown to be ours by its name: the converter ("ps5u-convert",
 * payload/convert/entry.c) or, when `helper_too`, a helper ("ps5upload..."). 0 when it is another's
 * or its name cannot be read. */
static int pid_is_ours(uint64_t pid, int helper_too) {
    char name[64];
    int rc;
    if (!pid_signallable(pid)) return 0;
#ifdef CONSOLE_CONVERT_SELFTEST
    rc = console_convert_test_proc_name ? console_convert_test_proc_name((int)pid, name, sizeof name) : -1;
#else
    rc = proc_name_by_pid((int)pid, name, sizeof name);
#endif
    if (rc != 0) return 0;
    return strcmp(name, CONVERT_PROC_NAME) == 0 || (helper_too && proc_name_is_ours(name));
}

/* 1 when `pid` (from a file written at wall-clock `then`, or with boot time `then`) still runs a
 * job of ours. A live process named as ours always does, whatever the boot test says: kern.boottime
 * moves when the clock is set forward, so a job of this boot can read as from an earlier one. The
 * boot test only discounts a live pid it cannot show is ours (after a restart, a pid may be another
 * process's now). */
static int pid_still_ours(uint64_t pid, int helper_too, int earlier_boot) {
    if (!pid_alive(pid)) return 0;
    return !earlier_boot || pid_is_ours(pid, helper_too);
}

/* 1 when `dest` is under a drive's mount point (/mnt/<name>/...) and nothing is mounted there: the
 * folder the console keeps for an unmounted drive is still there, empty, on the device that holds
 * /mnt itself. 0 otherwise (mounted, not under /mnt, or it cannot be told). */
static int mount_point_empty(const char *dest) {
    char mp[256];
    const char *end;
    struct stat mst, pst;
    if (strncmp(dest, "/mnt/", 5) != 0) return 0;
    end = strchr(dest + 5, '/');
    if (!end || end == dest + 5 || (size_t)(end - dest) >= sizeof mp) return 0;
    memcpy(mp, dest, (size_t)(end - dest));
    mp[end - dest] = 0;
    if (stat(mp, &mst) != 0 || stat("/mnt", &pst) != 0) return 0;
    return mst.st_dev == pst.st_dev;
}

/* What an unfinished build of `dest` leaves beside it: `<dest>.partial` (a file, or a folder for
 * a folder output) and the builder's two small files next to it. Only these names are removed,
 * never `dest` itself: it appears by a rename when the build is complete. 1 when settled (removed,
 * or never ours to remove), 0 when the folder it is in is not there or its drive is not mounted
 * (mount_point_empty): nothing can be said about it now. */
static int remove_partial(const char *dest) {
    static const char *const tails[] = {".partial", ".partial.resume", ".partial.stamp"};
    char path[1100];
    const char *slash = strrchr(dest, '/');
    struct stat st;
    size_t i;
    if (dest[0] != '/' || !slash) return 1;
    if (slash > dest && (size_t)(slash - dest) < sizeof path) {
        memcpy(path, dest, (size_t)(slash - dest));
        path[slash - dest] = 0;
        if (stat(path, &st) != 0 || !S_ISDIR(st.st_mode)) return 0;
    }
    /* A drive not mounted yet whose mount point's empty folder is there is just as missing. */
    if (mount_point_empty(dest)) return 0;
    if (!may_write_here(dest)) return 1;
    fprintf(stderr, "convert: removing what an unfinished conversion left beside %s\n", dest);
    for (i = 0; i < sizeof tails / sizeof *tails; i++) {
        if ((size_t)snprintf(path, sizeof path, "%s%s", dest, tails[i]) >= sizeof path) continue;
        if (lstat(path, &st) != 0) continue;
        if (S_ISDIR(st.st_mode)) (void)fsj_rm_rf(path, 0, NULL);
        else if (S_ISREG(st.st_mode)) (void)unlink(path);
    }
    return 1;
}

#define CC_PENDING_MAX 16
#define CC_PENDING_LINE 1100

/* One sweep of pending.rec at a time in this helper; a helper being replaced may run one at the same
 * time, so the file is written through a temporary name of this process's and this sweep's own. */
static pthread_mutex_t g_pending_lock = PTHREAD_MUTEX_INITIALIZER;
static unsigned g_pending_seq;

/* Tries again each unfinished output pending.rec keeps, and keeps `add` (one just found that could
 * not be cleared): what stays is what still can't be (its folder still missing). */
static void sweep_pending_locked(const char *add);
static void sweep_pending(const char *add) {
    pthread_mutex_lock(&g_pending_lock);
    sweep_pending_locked(add);
    pthread_mutex_unlock(&g_pending_lock);
}

static void sweep_pending_locked(const char *add) {
    char file[160], tmp[200];
    size_t cap = CC_PENDING_MAX * (CC_PENDING_LINE + 1) + 1, at = 0, kept = 0;
    char *buf = malloc(cap), *out = malloc(cap), *line, *next;
    struct stat st;
    int len, fd;
    if (!buf || !out) goto out;
    cpath(CF_PENDING, file, sizeof file);
    if (lstat(file, &st) != 0 && !add) goto out;
    len = read_small(file, buf, cap);
    if (len < 0) len = 0;
    buf[len] = 0;
    out[0] = 0;
    for (line = buf; line && *line; line = next) {
        next = strchr(line, '\n');
        if (next) *next++ = 0;
        if (!line[0] || strlen(line) >= CC_PENDING_LINE || (add && strcmp(line, add) == 0)) continue;
        if (remove_partial(line) || kept >= CC_PENDING_MAX) continue;
        at += (size_t)snprintf(out + at, cap - at, "%s\n", line);
        kept++;
    }
    if (add && add[0] == '/' && strlen(add) < CC_PENDING_LINE && kept < CC_PENDING_MAX) {
        fprintf(stderr, "convert: the folder of %s is not there; its unfinished output is removed once it is\n", add);
        at += (size_t)snprintf(out + at, cap - at, "%s\n", add);
        kept++;
    }
    if (!kept) {
        (void)unlink(file);
        goto out;
    }
    snprintf(tmp, sizeof tmp, "%s.%ld.%u.tmp", file, (long)getpid(), ++g_pending_seq);
    fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) goto out;
    if (write(fd, out, at) == (ssize_t)at) {
        close(fd);
        (void)rename(tmp, file);
    } else {
        close(fd);
        (void)unlink(tmp);
    }
out:
    free(buf);
    free(out);
}

static int record_is(const char *id);

/* Rewrites this job's record with the converter's pid, while it is still this job's (taken with
 * O_EXCL; only its owner removes it, but a sweep elsewhere may have judged it stale). */
static void write_record(uint64_t xpid, const char *id, const char *dest) {
    char file[160], *rec = malloc(1200);
    int len = rec ? cc_record_format_boot(rec, 1200, (unsigned long long)xpid, (long)getpid(), id, dest, boot_time()) : -1;
    FILE *f;
    if (len > 0 && record_is(id) && (f = fopen(cpath(CF_RECORD, file, sizeof file), "w")) != NULL) {
        fwrite(rec, 1, (size_t)len, f);
        fclose(f);
    }
    free(rec);
}

/* 1 when job.rec is job `id`'s. */
static int record_is(const char *id) {
    char file[160], rid[CC_JOB_ID_MAX], *buf = malloc(1400), *dest = malloc(1100);
    unsigned long long xpid = 0;
    long hpid = 0;
    int mine = 0;
    if (buf && dest && read_small(cpath(CF_RECORD, file, sizeof file), buf, 1400) > 0 &&
        cc_record_parse(buf, &xpid, &hpid, rid, sizeof rid, dest, 1100))
        mine = hpid == (long)getpid() && strcmp(rid, id) == 0;
    free(buf);
    free(dest);
    return mine;
}

/* 1 when file `f` (request.json or status.json) carries job `id`. */
static int file_is_job(cfile_t f, const char *id) {
    char file[160], got[CC_JOB_ID_MAX], *buf = malloc(CONVERT_STATUS_MAX);
    int mine = 0;
    if (buf && read_small(cpath(f, file, sizeof file), buf, CONVERT_STATUS_MAX) > 0)
        mine = fsj_json_str(buf, "id", got, sizeof got) && strcmp(got, id) == 0;
    free(buf);
    return mine;
}

/* Takes job.rec for job `id` (converter pid 0: not started yet), created only when there is none.
 * 1 taken, 0 another job holds it (a live helper, or its converter, still runs it), -1 it could
 * not be written. */
static int take_record(const char *id, const char *dest) {
    char file[160], *rec = malloc(1200);
    int attempt, rc = -1;
    if (!rec) return -1;
    cpath(CF_RECORD, file, sizeof file);
    for (attempt = 0; attempt < 2; attempt++) {
        int fd = open(file, O_WRONLY | O_CREAT | O_EXCL, 0600), len;
        if (fd < 0) {
            if (errno != EEXIST) break;
            if (attempt == 0) {
                /* One left by a helper that has gone, and its converter with it, is cleared here
                 * (what the sweep does at a start); one whose owner still runs stays. */
                console_convert_sweep();
                continue;
            }
            rc = 0;
            break;
        }
        len = cc_record_format_boot(rec, 1200, 0, (long)getpid(), id, dest, boot_time());
        rc = len > 0 && write(fd, rec, (size_t)len) == (ssize_t)len ? 1 : -1;
        close(fd);
        if (rc < 0) (void)unlink(file);
        break;
    }
    free(rec);
    return rc;
}

static void sweep_request(void) {
    char file[160];
    struct stat st;
    if (stat(cpath(CF_REQUEST, file, sizeof file), &st) != 0 || !console_convert_request_stale(st.st_mtime, time(NULL)))
        return;
    fprintf(stderr, "convert: removing a request no converter read\n");
    (void)unlink(file);
}

void console_convert_sweep(void) {
    char file[160], rid[CC_JOB_ID_MAX];
    /* On the heap: this also runs on a job thread, whose stack is small. */
    char *buf = malloc(1400), *dest = malloc(1100);
    unsigned long long xpid = 0, rboot = 0;
    long hpid = 0;
    int earlier_boot;
    struct stat st;
    sweep_request();
    sweep_pending(NULL);
    if (!buf || !dest) goto out;
    cpath(CF_RECORD, file, sizeof file);
    if (lstat(file, &st) != 0) goto out;
    if (read_small(file, buf, 1400) <= 0 ||
        !cc_record_parse_boot(buf, &xpid, &hpid, rid, sizeof rid, dest, 1100, &rboot)) {
        /* Empty or cut: one may be being written this moment (a helper creates it, then writes
         * it). Only an old one is not. */
        if (console_convert_request_stale(st.st_mtime, time(NULL))) (void)unlink(file);
        goto out;
    }
    /* The helper that wrote it, or its converter, still runs (a takeover lets the old one finish).
     * From an earlier boot (a power cut) both are gone, and a pid it names may be another
     * process's now (one we may not signal reads as alive), which kept Convert "busy" for good: then
     * only a live process shown to be ours by its name keeps it (pid_still_ours: the boot time
     * moves when the clock is set forward, and a live job must never lose its output). */
    earlier_boot = cc_from_earlier_boot(rboot, boot_time());
    if ((hpid != (long)getpid() && pid_still_ours((uint64_t)hpid, 1, earlier_boot)) ||
        pid_still_ours(xpid, 0, earlier_boot))
        goto out;
    if (xpid == 0) {
        /* Its converter was never seen: it may still start late. It is told to stop before it
         * writes anything, and its request goes; nothing of it is removed (there is nothing of
         * a converter that never ran, and a late one's output is its own to clear). */
        char other[160];
        int fd = open(cpath(CF_CANCEL, other, sizeof other), O_WRONLY | O_CREAT, 0600);
        if (fd >= 0) close(fd);
        if (rid[0] && file_is_job(CF_REQUEST, rid)) (void)unlink(cpath(CF_REQUEST, other, sizeof other));
    } else if (dest[0] && !remove_partial(dest)) {
        /* Its folder is not there yet (a drive mounted later after a restart): kept to try again,
         * not forgotten with the record. */
        sweep_pending(dest);
    }
    (void)unlink(file);
out:
    free(buf);
    free(dest);
}

int console_convert_building(void) { return __atomic_load_n(&g_building, __ATOMIC_SEQ_CST); }

static void power_tick(void) {
    pthread_mutex_lock(&sony_api_lock); /* a Sony call: one at a time with the others */
    (void)sceSystemServicePowerTick();
    pthread_mutex_unlock(&sony_api_lock);
}

/* Follows the started converter until it ends. 1 done, 0 failed (s says why), -1 it never started
 * or died, -2 it had to be killed (the job's message is set for both). `buf` holds the last status
 * read. */
static int follow(ava1_op_ctx_t *c, const char *id, const char *dest, const cc_build_t *b, char *buf, size_t cap,
                  cc_status_t *s) {
    uint64_t started = now_ms(), cancel_at = 0, total = 0, shown = 0, rec_pid = 0, tick_at = 0;
    /* The converter counts each stage (pack, plan, write, verify) from 0 against that stage's own
     * total, while a job has one bar: its total is the largest stage total seen (a first stage can
     * count something small, a .pkg's check, a plan's files: the write's total is about the game's
     * size), and each stage moves it through that stage's fixed share (cc_job_progress). The share
     * only grows, and the bar is moved only forward, so it never falls back as a stage starts or
     * the total grows, nor passes its total. */
    int build = b != NULL, have = 0, cancel_sent = 0;
    char *cur_buf = malloc(cap);
    if (!cur_buf) {
        ava1_op_message(c, "convert_no_memory");
        return -1;
    }
    for (;;) {
        cc_status_t cur;
        if (read_status(id, cur_buf, cap, &cur)) {
            have = 1;
            /* What a helper that dies now leaves behind, for the next start's sweep. */
            if (cur.pid != rec_pid) {
                rec_pid = cur.pid;
                write_record(rec_pid, id, dest);
            }
            memcpy(buf, cur_buf, cap);
            *s = cur;
            if (cur.result) s->result = buf + (cur.result - cur_buf);
            /* A final status may carry no counts: the last total stands. */
            if (cur.bytes_total > total) {
                total = cur.bytes_total;
                ava1_op_set_total(c, 0, total);
            }
            if (total) {
                uint64_t at = cc_job_progress(b ? b->format : NULL, b && b->zstd, cur.stage, cur.bytes_done,
                                              cur.bytes_total, total);
                if (at != UINT64_MAX && at > shown) {
                    ava1_op_add(c, 0, at - shown);
                    shown = at;
                }
            }
            if (strcmp(cur.state, "running") != 0) {
                free(cur_buf);
                return strcmp(cur.state, "done") == 0 ? 1 : 0;
            }
            if (cur.stage[0]) ava1_op_message(c, "convert_stage_%s", cur.stage);
        }
        if (!cancel_sent && ava1_op_cancelled(c)) {
            char file[160];
            int fd = open(cpath(CF_CANCEL, file, sizeof file), O_WRONLY | O_CREAT, 0600);
            if (fd >= 0) close(fd);
            cancel_sent = 1;
            cancel_at = now_ms();
        }
        if (build && now_ms() - tick_at >= CONVERT_POWER_TICK_MS) {
            tick_at = now_ms();
            power_tick();
        }
        if (!have) {
            if (now_ms() - started > CONVERT_START_MS) {
                ava1_op_message(c, "convert_not_started");
                break;
            }
        } else if (!pid_alive(s->pid)) {
            /* It may have ended between the read and this check: one more read. */
            cc_status_t last;
            if (read_status(id, cur_buf, cap, &last) && strcmp(last.state, "running") != 0) {
                memcpy(buf, cur_buf, cap);
                *s = last;
                if (last.result) s->result = buf + (last.result - cur_buf);
                free(cur_buf);
                return strcmp(last.state, "done") == 0 ? 1 : 0;
            }
            ava1_op_message(c, "convert_died");
            break;
        } else if (cancel_sent && now_ms() - cancel_at > CONVERT_KILL_AFTER_MS) {
            fprintf(stderr, "convert: the converter (pid %llu) did not stop; killing it\n", (unsigned long long)s->pid);
            if (pid_signallable(s->pid)) (void)kill((pid_t)s->pid, SIGKILL);
            sleep_ms(500);
            ava1_op_message(c, "convert_cancelled");
            free(cur_buf);
            return -2;
        }
        sleep_ms(CONVERT_POLL_MS);
    }
    free(cur_buf);
    return -1;
}

/* Checks a build's destination and fills `b`. NULL, or the cause token it is refused with. */
static const char *check_build(const char *json, const char *src, int src_is_dir, char *dest, size_t dest_cap,
                               char *format, size_t format_cap, cc_build_t *b) {
    char parent[1024], verify[16];
    const char *slash, *by_name;
    struct stat st;
    uint64_t zstd = 0, level = 0;
    if (!fsj_json_str(json, "dest", dest, dest_cap) || !dest[0]) return "convert_missing_dest";
    if (!fsj_json_str(json, "format", format, format_cap) || !cc_format_ok(format)) return "convert_bad_format";
    /* Allowed, not a drive's root, not inside ShadowMount+'s mounts, inside a folder that exists. */
    if (fsj_normalize_path(dest) != 0 || !may_write_here(dest) || fsj_path_is_root(dest) ||
        strncmp(dest, "/mnt/shadowmnt/", 15) == 0)
        return "convert_path_not_allowed";
    slash = strrchr(dest, '/');
    snprintf(parent, sizeof parent, "%.*s", (int)(slash - dest), dest);
    if (!parent[0] || stat(parent, &st) != 0 || !S_ISDIR(st.st_mode)) return "convert_bad_dest";
    /* An image is named for what it is: the console mounts by extension. */
    by_name = cc_image_format(dest);
    if (strcmp(format, "pkg") == 0) {
        if (!cc_pkg_name_ok(dest)) return "convert_bad_dest";
    } else if (strcmp(format, "folder") == 0 ? by_name != NULL : (!by_name || strcmp(by_name, format) != 0)) {
        return "convert_bad_dest";
    }
    /* Never over the source, and never a folder written inside the folder being read. */
    if (strcmp(dest, src) == 0 || (src_is_dir && cc_path_within(src, dest))) return "convert_bad_dest";
    /* Nor a source the build's own leftovers are named for: `<dest>.partial` is cleared before the
     * build writes, and removed after one that died. */
    if (cc_build_reads_own_leftovers(src, dest)) return "convert_bad_dest";
    if (lstat(dest, &st) == 0) return "convert_destination_exists";
    verify[0] = 0;
    if (fsj_json_str(json, "verify", verify, sizeof verify) && strcmp(verify, "fast") != 0 && strcmp(verify, "full") != 0)
        return "convert_bad_verify";
    (void)fsj_json_u64(json, "zstd", &zstd);
    if (fsj_json_u64(json, "zstd_level", &level) && !cc_zstd_level_ok(level)) return "convert_bad_zstd_level";
    b->dest = dest;
    b->format = format;
    b->zstd = zstd != 0;
    b->zstd_level = b->zstd ? (int)level : 0;
    b->full_verify = strcmp(verify, "full") == 0;
    b->zstd_runtime = NULL; /* found by the op, once the build is otherwise acceptable */
    b->inner[0] = 0;
    b->ffpfsc_level[0] = 0;
    b->pkg_level[0] = 0;
    if (strcmp(format, "pkg") == 0) {
        /* zstd packs go into an image or a folder; the level is the converter's default when absent. */
        if (b->zstd) return "convert_bad_format";
        if (fsj_json_present(json, "pkg_level") &&
            (!fsj_json_str(json, "pkg_level", b->pkg_level, sizeof b->pkg_level) || !cc_pkg_level_ok(b->pkg_level)))
            return "convert_bad_pkg_level";
    }
    if (strcmp(format, "ffpfsc") == 0) {
        /* The image inside it and how hard it is compressed: the converter's defaults when absent. */
        if (fsj_json_present(json, "inner") &&
            (!fsj_json_str(json, "inner", b->inner, sizeof b->inner) || !cc_inner_ok(b->inner)))
            return "convert_bad_inner";
        if (fsj_json_present(json, "ffpfsc_level") &&
            (!fsj_json_str(json, "ffpfsc_level", b->ffpfsc_level, sizeof b->ffpfsc_level) ||
             !cc_ffpfsc_level_ok(b->ffpfsc_level)))
            return "convert_bad_ffpfsc_level";
    }
    return NULL;
}

int console_convert_op(void *arg, ava1_op_ctx_t *c, const uint8_t *args, size_t n) {
    char src[1024], dest[1024], action[16], format[16], id[CC_JOB_ID_MAX], file[160];
    const char *json = (const char *)args, *bad;
    cc_build_t build, *b = NULL;
    cc_status_t s;
    struct stat st;
    cc_runtime_t *rt = NULL; /* on the heap: a job thread's stack is small */
    char *buf = NULL;
    int status = AVA1_ERR_IO, r, is_dir, never_seen = 0, archive = 0;
    (void)arg;
    (void)n;
    memset(&s, 0, sizeof s);
    dest[0] = 0;
    if (!fsj_json_str(json, "path", src, sizeof src) || !src[0]) {
        ava1_op_message(c, "convert_missing_path");
        return AVA1_ERR_PROTOCOL;
    }
    if (!fsj_json_str(json, "action", action, sizeof action)) snprintf(action, sizeof action, "inspect");
    if (strcmp(action, "inspect") != 0 && strcmp(action, "build") != 0) {
        ava1_op_message(c, "convert_bad_action");
        return AVA1_ERR_PROTOCOL;
    }
    /* The source is only read, but it is held to the roots the helper may write: those are the
     * console's game storage, and nothing else has a game to convert. */
    if (fsj_normalize_path(src) != 0 || !may_write_here(src) || strncmp(src, "/mnt/shadowmnt/", 15) == 0) {
        ava1_op_message(c, "convert_path_not_allowed");
        return AVA1_ERR_PATH;
    }
    if (stat(src, &st) != 0 || (!S_ISDIR(st.st_mode) && !S_ISREG(st.st_mode))) {
        ava1_op_message(c, "convert_open_failed");
        return AVA1_ERR_PATH;
    }
    is_dir = S_ISDIR(st.st_mode);
    if (!is_dir && !cc_image_format(src)) {
        /* An archive: the game folder in a .zip or .7z is read in place; a .rar is not converted
         * here (cc_archive_kind), refused before a converter is started. */
        const char *kind = cc_archive_kind(src);
        if (kind && strcmp(kind, "rar") == 0) {
            ava1_op_message(c, "convert_rar_unsupported");
            return AVA1_ERR_PATH;
        }
        if (!kind) {
            ava1_op_message(c, "convert_unsupported_source");
            return AVA1_ERR_PATH;
        }
        archive = 1;
    }
    if (strcmp(action, "build") == 0) {
        bad = check_build(json, src, is_dir, dest, sizeof dest, format, sizeof format, &build);
        if (bad) {
            ava1_op_message(c, "%s", bad);
            return strcmp(bad, "convert_destination_exists") == 0 ? AVA1_ERR_EXISTS
                   : strncmp(bad, "convert_missing", 15) == 0 || strncmp(bad, "convert_bad_format", 18) == 0 ||
                             strcmp(bad, "convert_bad_verify") == 0 || strcmp(bad, "convert_bad_zstd_level") == 0 ||
                             strcmp(bad, "convert_bad_inner") == 0 || strcmp(bad, "convert_bad_ffpfsc_level") == 0 ||
                             strcmp(bad, "convert_bad_pkg_level") == 0
                         ? AVA1_ERR_PROTOCOL
                         : AVA1_ERR_PATH;
        }
        b = &build;
    }
    if (!ps5upload_convert_elf_len) {
        ava1_op_message(c, "convert_not_bundled");
        return AVA1_ERR_INTERNAL;
    }
    /* zstd packs carry the runtime, which the desktop app puts on the console (nothing is
     * downloaded here): without one the converter is not started at all. */
    if (b && b->zstd) {
        rt = malloc(sizeof *rt);
        if (!rt) {
            ava1_op_message(c, "convert_no_memory");
            return AVA1_ERR_INTERNAL;
        }
        if (!console_convert_find_runtime(rt)) {
            free(rt);
            ava1_op_message(c, "convert_no_zstd_runtime");
            return AVA1_ERR_PATH;
        }
        b->zstd_runtime = rt->path;
    }
    if (pthread_mutex_trylock(&g_busy) != 0) {
        free(rt);
        ava1_op_message(c, "convert_busy");
        return AVA1_ERR_BUSY;
    }
    /* A build beside an install or an extraction: all three write a drive hard and hold memory.
     * An inspection of a folder or an image reads a few KiB and may run beside either; one of a
     * .zip or .7z is held like a build (a .7z's param file may be behind a large solid block,
     * decoded with its dictionary and cache). Our flag first, then theirs (all seq_cst; the
     * install and the extraction do the same the other way round): of two that start at once at
     * least one sees the other and backs out, never neither. */
    if (b || archive) {
        __atomic_store_n(&g_building, 1, __ATOMIC_SEQ_CST);
        if (webui_pkg_installing() || archive_extract_running()) {
            ava1_op_message(c, "convert_busy");
            status = AVA1_ERR_BUSY;
            goto unlock; /* clears the flag */
        }
    }
    buf = malloc(CONVERT_STATUS_MAX);
    if (!buf) {
        ava1_op_message(c, "convert_no_memory");
        goto unlock;
    }
    (void)mkdir(cdir(), 0700);
    console_convert_sweep();
    {
        uint8_t rnd[8];
        arc4random_buf(rnd, sizeof rnd);
        snprintf(id, sizeof id, "%02x%02x%02x%02x%02x%02x%02x%02x", rnd[0], rnd[1], rnd[2], rnd[3], rnd[4], rnd[5],
                 rnd[6], rnd[7]);
    }
    /* The job's record, before anything is started: g_busy keeps one job per helper, this keeps
     * one across helpers. A helper that took over while an old one's converter has not reported
     * yet finds the old one's record and waits its turn, instead of taking its request and
     * starting a second converter. */
    r = take_record(id, dest);
    if (r <= 0) {
        if (r == 0) {
            ava1_op_message(c, "convert_busy");
            status = AVA1_ERR_BUSY;
        } else {
            ava1_op_message(c, "convert_write_failed: job.rec");
        }
        goto unlock;
    }
    {
        /* A previous converter still running (its helper was replaced): one at a time. */
        uint64_t old = 0;
        char state[16];
        int len = read_small(cpath(CF_STATUS, file, sizeof file), buf, CONVERT_STATUS_MAX);
        struct stat sst;
        /* A "running" written before this boot is a converter a power cut ended: its pid may be
         * another process's now. */
        int stale = stat(file, &sst) == 0 && cc_from_earlier_boot((unsigned long long)sst.st_mtime, boot_time());
        if (len > 0 && fsj_json_str(buf, "state", state, sizeof state) && strcmp(state, "running") == 0 &&
            fsj_json_u64(buf, "pid", &old) && pid_still_ours(old, 0, stale)) {
            ava1_op_message(c, "convert_busy");
            status = AVA1_ERR_BUSY;
            (void)unlink(cpath(CF_RECORD, file, sizeof file)); /* ours: taken just above */
            goto unlock;
        }
    }
    (void)unlink(cpath(CF_STATUS, file, sizeof file));
    (void)unlink(cpath(CF_CANCEL, file, sizeof file));
    if (write_request(id, action, src, b) != 0) {
        ava1_op_message(c, "convert_write_failed: request");
        goto cleanup;
    }
    ava1_op_message(c, "convert_starting");
    if (launch() != 0) {
        if (file_is_job(CF_REQUEST, id)) (void)unlink(cpath(CF_REQUEST, file, sizeof file));
        ava1_op_message(c, "convert_loader_unreachable");
        goto cleanup;
    }
    r = follow(c, id, dest, b, buf, CONVERT_STATUS_MAX, &s);
    /* No status ever seen: the converter may still start late, and may already have read the
     * request. The cancel file goes first and stays (the next job's start removes it), so a late
     * one stops before it writes anything; then the request, so none starts it later. */
    never_seen = !s.state[0];
    if (never_seen) {
        int fd = open(cpath(CF_CANCEL, file, sizeof file), O_WRONLY | O_CREAT, 0600);
        if (fd >= 0) close(fd);
    }
    /* Only ours: never another job's. */
    if (file_is_job(CF_REQUEST, id)) (void)unlink(cpath(CF_REQUEST, file, sizeof file));
    if (r == 1) {
        if (b) {
            /* The page gets the path as the helper normalised it. In `buf` (the last status is
             * not needed any more): 4 KiB is too much for a job thread's stack. */
            char *resp = buf;
            size_t cap = CONVERT_RESULT_MAX, at = (size_t)snprintf(resp, cap, "{\"path\":\""), need;
            need = webui_json_escape(resp + at, cap - at, dest, strlen(dest));
            if (need + 3 < cap - at) {
                at += need;
                at += (size_t)snprintf(resp + at, cap - at, "\"}");
                (void)ava1_op_set_result(c, resp, at);
            }
        } else if (s.result && s.result_len <= CONVERT_RESULT_MAX) {
            /* The answer says whether a zstd build could run here. Into `out` (heap, as `resp`
             * above); the converter's answer as it is when that does not fit. */
            char *out = malloc(CONVERT_RESULT_MAX + 1);
            int len = -1;
            if (!rt) rt = malloc(sizeof *rt);
            if (out && rt)
                len = cc_result_with_runtime(out, CONVERT_RESULT_MAX + 1, s.result, s.result_len,
                                             console_convert_find_runtime(rt) ? rt->version : NULL);
            if (len > 0 && (size_t)len <= CONVERT_RESULT_MAX) (void)ava1_op_set_result(c, out, (size_t)len);
            else (void)ava1_op_set_result(c, s.result, s.result_len);
            free(out);
        } else {
            ava1_op_message(c, "convert_bad_status");
            goto cleanup;
        }
        ava1_op_message(c, "%s", "");
        status = AVA1_STATUS_OK;
    } else {
        int cancelled = ava1_op_cancelled(c) || (r == 0 && strcmp(s.cause, "convert_cancelled") == 0);
        if (cancelled) ava1_op_message(c, "convert_cancelled");
        else if (r == 0) ava1_op_message(c, "%s", s.cause[0] ? s.cause : "convert_failed");
        if (r == 0 && s.detail[0]) fprintf(stderr, "convert: %s %s: %s: %s\n", action, src, s.cause, s.detail);
        status = cancelled ? AVA1_ERR_CANCELLED : AVA1_ERR_IO;
        /* Nothing kept from a build that did not finish. The converter removes its own output
         * when it fails or is stopped; this is for one that died or was killed. */
        if (b && s.pid && (r == -2 || !pid_alive(s.pid))) remove_partial(dest);
    }
cleanup:
    /* The job's files, only while they are this job's: a record that is not ours any more means
     * another job owns the folder now, and its status and cancel are its own. The record goes
     * last: it is the lock. */
    if (record_is(id)) {
        if (file_is_job(CF_STATUS, id)) (void)unlink(cpath(CF_STATUS, file, sizeof file));
        if (!never_seen) (void)unlink(cpath(CF_CANCEL, file, sizeof file));
        (void)unlink(cpath(CF_RECORD, file, sizeof file));
    }
unlock:
    free(buf);
    free(rt);
    __atomic_store_n(&g_building, 0, __ATOMIC_SEQ_CST);
    pthread_mutex_unlock(&g_busy);
    return status;
}

void console_convert_register(void) { (void)ava1_op_register(AVA1_JOB_OP_CONSOLE_CONVERT, console_convert_op, NULL); }
