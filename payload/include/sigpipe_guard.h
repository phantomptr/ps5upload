#ifndef PS5UPLOAD_SIGPIPE_GUARD_H
#define PS5UPLOAD_SIGPIPE_GUARD_H

/* No thread of the helper may die of SIGPIPE, ever, and one Sony call needs it at its default.
 *
 * The helper ignores SIGPIPE (a write to a closed peer returns EPIPE), but opening a save does not
 * complete that way: sceFsMountSaveData has to run with SIGPIPE at its default (hardware, FW 13.60).
 * A signal's disposition is the whole process's, so for that call every other thread would be
 * one write to a closed peer away from dying with no breadcrumb.
 *
 * So every thread keeps SIGPIPE blocked: main blocks it before any thread exists, and a new thread
 * inherits its creator's mask. FreeBSD directs the signal of a failed write at the writing thread,
 * where it waits, blocked, and the write returns EPIPE as usual. Only the thread making the Sony call
 * unblocks it, for that call. Going back to SIG_IGN drops whatever waited meanwhile. */
#include <pthread.h>
#include <signal.h>
#include <sys/socket.h>

/* Writers that can name it say so too (a host build may not have it). */
#ifndef MSG_NOSIGNAL
#define MSG_NOSIGNAL 0
#endif

static inline void sigpipe_set_of(sigset_t *s) {
    sigemptyset(s);
    sigaddset(s, SIGPIPE);
}

/* In main, before any thread is started. */
static inline void sigpipe_block_this_thread(void) {
    sigset_t s;
    sigpipe_set_of(&s);
    (void)pthread_sigmask(SIG_BLOCK, &s, NULL);
}

typedef struct {
    void (*old)(int);
} sigpipe_window_t;

/* Around the one call that needs SIGPIPE at its default; never nested, never from two threads at once
 * (the caller's lock). A SIGPIPE the call itself raises still ends the process, as it always did. */
static inline void sigpipe_default_open(sigpipe_window_t *w) {
    sigset_t s;
    sigpipe_set_of(&s);
    w->old = signal(SIGPIPE, SIG_DFL);
    (void)pthread_sigmask(SIG_UNBLOCK, &s, NULL);
}

static inline void sigpipe_default_close(const sigpipe_window_t *w) {
    sigset_t s;
    sigpipe_set_of(&s);
    (void)pthread_sigmask(SIG_BLOCK, &s, NULL);
    (void)signal(SIGPIPE, w->old);
}

#endif /* PS5UPLOAD_SIGPIPE_GUARD_H */
