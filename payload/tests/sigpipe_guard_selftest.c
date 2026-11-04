/* Host test for sigpipe_guard.h: while one thread has SIGPIPE at its default for a Sony call, a write
 * to a closed peer on any other thread returns EPIPE and kills nothing. Each case runs in a child
 * process, so a death is a failure rather than the end of the test. Build: see the root Makefile
 * test-payload. */
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#include "sigpipe_guard.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

static int thread_blocks_sigpipe(void) {
    sigset_t cur;
    (void)pthread_sigmask(SIG_BLOCK, NULL, &cur);
    return sigismember(&cur, SIGPIPE) == 1;
}

/* What a thread started after main blocked SIGPIPE sees. */
static void *inherited_main(void *arg) {
    *(int *)arg = thread_blocks_sigpipe();
    return NULL;
}

/* A write to a closed peer: errno, on a pipe and on a socket. */
static int write_closed_pipe(void) {
    int p[2];
    ssize_t w;
    if (pipe(p) != 0) return -1;
    close(p[0]);
    w = write(p[1], "x", 1);
    int e = w < 0 ? errno : 0;
    close(p[1]);
    return e;
}

static int write_closed_socket(void) {
    int p[2];
    ssize_t w;
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, p) != 0) return -1;
    close(p[0]);
    w = send(p[1], "x", 1, 0);
    int e = w < 0 ? errno : 0;
    close(p[1]);
    return e;
}

static void *writer_main(void *arg) {
    int *out = arg;
    out[0] = write_closed_pipe();
    out[1] = write_closed_socket();
    return NULL;
}

/* Runs fn in a child; 1 when it exited 0, 0 when it failed or a signal killed it. */
static int in_child(const char *what, void (*fn)(void)) {
    pid_t pid = fork();
    int st = 0;
    if (pid == 0) {
        fn();
        _exit(failures ? 1 : 0);
    }
    if (pid < 0 || waitpid(pid, &st, 0) != pid) return 0;
    if (WIFSIGNALED(st)) fprintf(stderr, "%s: killed by signal %d\n", what, WTERMSIG(st));
    return WIFEXITED(st) && WEXITSTATUS(st) == 0;
}

/* main's set-up, then the mask and disposition a window changes, seen from two threads. */
static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t g_cv = PTHREAD_COND_INITIALIZER;
static int g_step, g_other_blocked;

static void *other_main(void *arg) {
    (void)arg;
    pthread_mutex_lock(&g_mu);
    while (g_step == 0) pthread_cond_wait(&g_cv, &g_mu);
    g_other_blocked = thread_blocks_sigpipe();
    g_step = 2;
    pthread_cond_broadcast(&g_cv);
    pthread_mutex_unlock(&g_mu);
    return NULL;
}

static void case_masks(void) {
    pthread_t t;
    int inherited = 0;
    struct sigaction sa;
    sigpipe_window_t w;

    signal(SIGPIPE, SIG_IGN);
    sigpipe_block_this_thread();
    CHECK(thread_blocks_sigpipe());
    CHECK(pthread_create(&t, NULL, inherited_main, &inherited) == 0);
    pthread_join(t, NULL);
    CHECK(inherited);

    CHECK(pthread_create(&t, NULL, other_main, NULL) == 0);
    sigpipe_default_open(&w);
    CHECK(!thread_blocks_sigpipe());
    CHECK(sigaction(SIGPIPE, NULL, &sa) == 0 && sa.sa_handler == SIG_DFL);
    /* The other thread stays blocked while the window is open. */
    pthread_mutex_lock(&g_mu);
    g_step = 1;
    pthread_cond_broadcast(&g_cv);
    while (g_step != 2) pthread_cond_wait(&g_cv, &g_mu);
    pthread_mutex_unlock(&g_mu);
    pthread_join(t, NULL);
    CHECK(g_other_blocked);
    sigpipe_default_close(&w);
    CHECK(thread_blocks_sigpipe());
    CHECK(sigaction(SIGPIPE, NULL, &sa) == 0 && sa.sa_handler == SIG_IGN);
}

/* SIGPIPE at its default with every thread blocking it: the write fails, nobody dies. Back to SIG_IGN,
 * what waited is gone: unblocking afterwards kills nobody either. */
static void case_blocked_writer_lives(void) {
    pthread_t t;
    int errs[2] = {0, 0};
    sigset_t s;
    sigpipe_window_t w;

    signal(SIGPIPE, SIG_IGN);
    sigpipe_block_this_thread();
    w.old = signal(SIGPIPE, SIG_DFL);
    CHECK(pthread_create(&t, NULL, writer_main, errs) == 0);
    pthread_join(t, NULL);
    CHECK(errs[0] == EPIPE);
    CHECK(errs[1] == EPIPE);
    /* A second write on this thread leaves one waiting here too. */
    CHECK(write_closed_pipe() == EPIPE);
    sigpipe_default_close(&w);
    sigemptyset(&s);
    sigaddset(&s, SIGPIPE);
    CHECK(pthread_sigmask(SIG_UNBLOCK, &s, NULL) == 0);
    usleep(1000);
}

/* The real shape: one thread inside the window, another writing. FreeBSD and Linux direct the signal at
 * the writing thread; macOS at the process, which would pick the thread in the window, so the case only
 * means something where the helper runs. */
static void case_writer_during_window(void) {
#if !defined(__APPLE__)
    pthread_t t;
    int errs[2] = {0, 0};
    sigpipe_window_t w;

    signal(SIGPIPE, SIG_IGN);
    sigpipe_block_this_thread();
    sigpipe_default_open(&w);
    CHECK(pthread_create(&t, NULL, writer_main, errs) == 0);
    pthread_join(t, NULL);
    sigpipe_default_close(&w);
    CHECK(errs[0] == EPIPE);
    CHECK(errs[1] == EPIPE);
#endif
}

int main(void) {
    CHECK(in_child("masks", case_masks));
    CHECK(in_child("blocked writer", case_blocked_writer_lives));
    CHECK(in_child("writer during a window", case_writer_during_window));
    if (failures) {
        fprintf(stderr, "sigpipe_guard_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("sigpipe_guard_selftest: ok\n");
    return 0;
}
