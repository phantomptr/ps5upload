#include "fakelib_overlay.h"

#include "app_info.h"
#include "fakelib_overlay_paths.h"
#include "proc_list.h"
#include "shell_builtin.h"

#include <dirent.h>
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/event.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/uio.h>
#include <time.h>
#include <unistd.h>

#define OVERLAY_PATH_MAX 1024
#define IOVEC_ENTRY(x) { (void *)(x), (x) ? strlen(x) + 1 : 0 }
#define IOVEC_SIZE(x) (sizeof(x) / sizeof((x)[0]))

typedef struct overlay_state {
    pthread_mutex_t mutex;
    pthread_t thread;
    int started;
    volatile int stop;
    char state[16];
    char title_id[10];
    char error[96];
    char mounted_on[OVERLAY_PATH_MAX];
} overlay_state_t;

static overlay_state_t g_overlay = {
    .mutex = PTHREAD_MUTEX_INITIALIZER,
    .state = "idle",
};

static void set_state(const char *state, const char *title_id, const char *error) {
    pthread_mutex_lock(&g_overlay.mutex);
    snprintf(g_overlay.state, sizeof(g_overlay.state), "%s", state ? state : "idle");
    snprintf(g_overlay.title_id, sizeof(g_overlay.title_id), "%s", title_id ? title_id : "");
    snprintf(g_overlay.error, sizeof(g_overlay.error), "%s", error ? error : "");
    pthread_mutex_unlock(&g_overlay.mutex);
}

static int find_sandbox(const char *title_id, char *sandbox, size_t cap) {
    DIR *dir = opendir("/mnt/sandbox");
    if (!dir) return 0;
    int best = -1;
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        char found[10];
        if (!fakelib_overlay_title_from_sandbox(entry->d_name, found, sizeof(found)) ||
            strcmp(found, title_id) != 0) continue;
        int number = atoi(entry->d_name + 10);
        if (number >= best && strlen(entry->d_name) + 1 <= cap) {
            best = number;
            snprintf(sandbox, cap, "%s", entry->d_name);
        }
    }
    closedir(dir);
    return best >= 0;
}

static int resolve_paths(const char *title_id, char *source, size_t source_cap,
                         char *target, size_t target_cap) {
    char sandbox[32];
    if (!find_sandbox(title_id, sandbox, sizeof(sandbox))) return 0;
    if (snprintf(source, source_cap, "/mnt/sandbox/%s/app0/fakelib", sandbox) >=
        (int)source_cap) return 0;
    struct stat st;
    if (stat(source, &st) != 0 || !S_ISDIR(st.st_mode)) return 0;

    char root[OVERLAY_PATH_MAX];
    if (snprintf(root, sizeof(root), "/mnt/sandbox/%s", sandbox) >= (int)sizeof(root)) return 0;
    DIR *dir = opendir(root);
    if (!dir) return 0;
    int found = 0;
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        if (!fakelib_overlay_run_dir_ok(entry->d_name)) continue;
        int n = snprintf(target, target_cap, "%s/%s/common/lib", root, entry->d_name);
        if (n < 0 || (size_t)n >= target_cap) continue;
        if (stat(target, &st) == 0 && S_ISDIR(st.st_mode)) { found = 1; break; }
    }
    closedir(dir);
    return found;
}

static int target_is_unionfs(const char *target) {
    struct statfs *mounts = NULL;
    int count = mntinfo_snapshot(&mounts);
    int found = 0;
    for (int i = 0; i < count; i++) {
        if (fakelib_overlay_target_matches(mounts[i].f_mntonname,
                                           mounts[i].f_fstypename, target)) {
            found = 1;
            break;
        }
    }
    free(mounts);
    return found;
}

static int mount_overlay(const char *source, const char *target) {
    struct iovec iov[] = {
        IOVEC_ENTRY("fstype"), IOVEC_ENTRY("unionfs"),
        IOVEC_ENTRY("from"), IOVEC_ENTRY(source),
        IOVEC_ENTRY("fspath"), IOVEC_ENTRY(target),
    };
    return nmount(iov, IOVEC_SIZE(iov), 0);
}

static void cleanup_mount(void) {
    char target[OVERLAY_PATH_MAX];
    pthread_mutex_lock(&g_overlay.mutex);
    snprintf(target, sizeof(target), "%s", g_overlay.mounted_on);
    g_overlay.mounted_on[0] = '\0';
    pthread_mutex_unlock(&g_overlay.mutex);
    if (target[0]) (void)unmount(target, MNT_FORCE);
}

/* Is the unionfs already on `target` the one WE mounted?
 *
 * A game execs more than once per launch, so the second pass sees the overlay
 * the first pass created. Reporting that as "an external BackPork is active"
 * sent a user hunting for a payload that was not running: the kernel line
 * `unionfs_domount: The same unionfs mount is prohibited` is OUR second
 * attempt, and it is benign — titles that log it launch fine. */
static int overlay_is_ours(const char *target) {
    int ours;
    pthread_mutex_lock(&g_overlay.mutex);
    ours = g_overlay.mounted_on[0] && strcmp(g_overlay.mounted_on, target) == 0;
    pthread_mutex_unlock(&g_overlay.mutex);
    return ours;
}

/* Ask to be told when this process exits.
 *
 * Registered on EVERY path that identifies a game, not just the one that
 * mounts. `blocked` and `error` used to return before this, and since the exit
 * event is the only thing that puts the state back to `watching`, a single
 * blocked launch left the UI reporting "overlay unavailable" until the payload
 * was reloaded — long after the cause was gone. */
static void watch_for_exit(int kq, pid_t pid) {
    struct kevent exit_event;
    EV_SET(&exit_event, (uintptr_t)pid, EVFILT_PROC, EV_ADD | EV_ENABLE | EV_CLEAR,
           NOTE_EXIT, 0, NULL);
    (void)kevent(kq, &exit_event, 1, NULL, 0, NULL);
}

static void handle_exec(int kq, pid_t pid) {
    app_info_t info;
    char title_id[10];
    memset(&info, 0, sizeof(info));
    if (sceKernelGetAppInfo(pid, &info) != 0 ||
        !app_info_title_id(&info, title_id, sizeof(title_id)) ||
        (strncmp(title_id, "PPSA", 4) != 0 && strncmp(title_id, "CUSA", 4) != 0)) return;

    char source[OVERLAY_PATH_MAX] = {0};
    char target[OVERLAY_PATH_MAX] = {0};
    for (int attempt = 0; attempt < 30 && !g_overlay.stop; attempt++) {
        if (resolve_paths(title_id, source, sizeof(source), target, sizeof(target))) break;
        usleep(100000);
    }
    if (!source[0] || !target[0]) return;
    /* Our own overlay, seen again on a later exec of the same launch. The
     * libraries ARE mounted, so say so rather than crying foul. */
    if (overlay_is_ours(target)) {
        set_state("mounted", title_id, NULL);
        watch_for_exit(kq, pid);
        return;
    }
    /* Only these two are genuinely somebody else's. Naming which one is the
     * difference between "stop the other payload" and "reboot to clear a
     * stale mount", and the user cannot tell them apart from one message. */
    if (proc_find_pid_by_name("backpork.elf") > 0) {
        set_state("blocked", title_id, "external_backpork_running");
        watch_for_exit(kq, pid);
        return;
    }
    if (target_is_unionfs(target)) {
        set_state("blocked", title_id, "foreign_unionfs_on_target");
        watch_for_exit(kq, pid);
        return;
    }
    if (mount_overlay(source, target) != 0) {
        set_state("error", title_id, strerror(errno));
        watch_for_exit(kq, pid);
        return;
    }
    pthread_mutex_lock(&g_overlay.mutex);
    snprintf(g_overlay.mounted_on, sizeof(g_overlay.mounted_on), "%s", target);
    pthread_mutex_unlock(&g_overlay.mutex);
    set_state("mounted", title_id, NULL);
    watch_for_exit(kq, pid);
}

static void *overlay_main(void *unused) {
    (void)unused;
    int syscore = proc_find_pid_by_name("SceSysCore.elf");
    if (syscore <= 0) { set_state("error", NULL, "syscore_not_found"); return NULL; }
    int kq = kqueue();
    if (kq < 0) { set_state("error", NULL, "kqueue_failed"); return NULL; }
    struct kevent watch;
    EV_SET(&watch, (uintptr_t)syscore, EVFILT_PROC, EV_ADD | EV_ENABLE | EV_CLEAR,
           NOTE_FORK | NOTE_EXEC | NOTE_TRACK, 0, NULL);
    if (kevent(kq, &watch, 1, NULL, 0, NULL) != 0) {
        close(kq); set_state("error", NULL, "syscore_watch_failed"); return NULL;
    }
    set_state("watching", NULL, NULL);
    pid_t child = -1;
    while (!g_overlay.stop) {
        struct kevent event;
        struct timespec timeout = { .tv_sec = 1, .tv_nsec = 0 };
        int n = kevent(kq, NULL, 0, &event, 1, &timeout);
        if (n < 0) { if (errno == EINTR) continue; set_state("error", NULL, "kevent_failed"); break; }
        if (n == 0) continue;
        if (event.fflags & NOTE_CHILD) child = (pid_t)event.ident;
        if ((event.fflags & NOTE_EXEC) && child > 0 && event.ident == (uintptr_t)child)
            handle_exec(kq, child);
        if ((event.fflags & NOTE_EXIT) && event.ident != (uintptr_t)syscore) {
            cleanup_mount();
            set_state("watching", NULL, NULL);
            child = -1;
        }
    }
    cleanup_mount();
    close(kq);
    return NULL;
}

int fakelib_overlay_start(void) {
    if (g_overlay.started) return 0;
    g_overlay.stop = 0;
    if (pthread_create(&g_overlay.thread, NULL, overlay_main, NULL) != 0) {
        set_state("error", NULL, "thread_create_failed");
        return -1;
    }
    g_overlay.started = 1;
    return 0;
}

void fakelib_overlay_stop(void) {
    if (!g_overlay.started) return;
    g_overlay.stop = 1;
    pthread_join(g_overlay.thread, NULL);
    g_overlay.started = 0;
    set_state("idle", NULL, NULL);
}

int fakelib_overlay_status_json(char *buf, size_t cap) {
    pthread_mutex_lock(&g_overlay.mutex);
    int n = snprintf(buf, cap,
        "{\"state\":\"%s\",\"title_id\":\"%s\",\"error\":\"%s\"}",
        g_overlay.state, g_overlay.title_id, g_overlay.error);
    pthread_mutex_unlock(&g_overlay.mutex);
    return n;
}
