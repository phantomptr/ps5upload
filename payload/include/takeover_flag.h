#ifndef PS5UPLOAD2_TAKEOVER_FLAG_H
#define PS5UPLOAD2_TAKEOVER_FLAG_H

#include <stdint.h>

/*
 * Takeover between two AVA1-era instances (and the loopback helpers both takeover paths share).
 *
 * Each instance has a random nonce (not an ordered id: the app changes the console clock with
 * settimeofday, so no time-based ordering is trustworthy). The new instance writes <dir>/takeover
 * holding its own nonce. The old instance polls the file once a second and exits when it holds a
 * nonce other than its own that it did not already find there at its own start (the poll records
 * the file's identity -- nonce, inode, mtime -- when it starts, and unlinks it). Nothing here reads
 * the clock, so a clock that jumps either way changes nothing; the order of events is the
 * observation order of one monotonic poll loop. The flag is unlinked at startup and after a
 * successful takeover. The file is local: nothing on the network can write it.
 */

/* A fresh random nonce (never 0): kern.arandom / getentropy / urandom. 0 on success. */
int takeover_nonce_new(uint64_t *nonce);

/* Writes the flag (a temporary file in <dir>, then an in-directory rename). 0 on success. */
int takeover_flag_write(const char *dir, uint64_t nonce);

/* Reads the nonce the flag holds. 0 on success; -1 when absent or unreadable. */
int takeover_flag_read(const char *dir, uint64_t *nonce);

/* Removes the flag (absent is fine). */
void takeover_flag_unlink(const char *dir);

/* Identity of a flag file as found at one moment. */
typedef struct {
    int present;
    uint64_t nonce;
    uint64_t ino;
    int64_t mtime_ns; /* compared for equality only, never ordered */
} takeover_flag_id_t;

/* Reads the flag's identity (present = 0 when absent or unreadable). */
void takeover_flag_identity(const char *dir, takeover_flag_id_t *out);

/* 1 when a flag is present, holds a nonce other than `my_nonce`, and is not the `stale` one. */
int takeover_flag_asks_us_to_exit(const char *dir, uint64_t my_nonce, const takeover_flag_id_t *stale);

/* 1 when something accepts a TCP connection on 127.0.0.1:port, or when that could not be ruled out
 * (only a refused connect is 0). Bounded: about half a second at most, never a blocking connect. */
int takeover_port_responding(int port);

/* Writes the flag, then waits until none of `ports` answers on loopback, checking every
 * `interval_us` for at most `attempts` x `interval_us` on CLOCK_MONOTONIC (a slow probe of a wedged
 * instance does not stretch the wait; the ports are always checked at least once). 0 when every port
 * is free (the flag is unlinked), -1 when one still answers at the deadline (the flag stays for the
 * old instance). */
int takeover_flag_request(const char *dir, uint64_t my_nonce, const int *ports, int nports,
                          int attempts, int interval_us);

/* Unlinks any flag left from before this instance (recording its identity as stale first), then
 * starts a detached thread that checks the flag every `period_ms` and calls `on_newer` once when a
 * different instance asked this one to exit, then ends. 0 on success. */
/* Waits (CLOCK_MONOTONIC) for `port` to stop answering on loopback: polls every `interval_ms`, up
 * to `max_ms`. 0 = free (checked at least once), -1 = still answered after max_ms. A new instance
 * calls it before it starts its AVA1 side: two helpers with AVA1 running at once is what hung the
 * Pro on 2026-10-03 (final review: console, outage). */
int takeover_wait_port_free(int port, int max_ms, int interval_ms);

/* The single-instance gate in front of the AVA1 side (review 010 §1): waits for `port` to free, then
 * calls start(ctx) and returns 1; when the port is still answered after max_ms it does NOT call
 * start and returns 0 (the caller refuses to co-run). main.c runs its AVA1 start through this. */
int takeover_gate_start(int port, int max_ms, int interval_ms, void (*start)(void *), void *ctx);

int takeover_flag_poll_start(const char *dir, uint64_t my_nonce, int period_ms, void (*on_newer)(void));

/*
 * One helper starts at a time. Two helpers sent together used to run their start-up side by side:
 * each removed the other's takeover flag, the later one read the earlier one's fresh ownership record
 * as "the prior instance" and killed it once it was serving, and the one that lost the port deleted
 * the winner's record on its way out.
 *
 * The lock is flock(LOCK_EX | LOCK_NB) on <dir>/start.lock, held from before the takeover until the
 * transfer port is serving. The kernel drops it when the holder exits or dies, so a file left by a
 * crash or a power cut never keeps a helper from starting. The holder writes "pid monotonic_ms" into
 * the file.
 *
 * takeover_start_lock is one try. Returns a descriptor (>= 0) when this process holds the lock;
 * TAKEOVER_START_BUSY when another helper holds it and has for no longer than `max_age_ms` (or its
 * age cannot be read); TAKEOVER_START_NO_LOCK when no lock could be had (no flock here, the file
 * cannot be opened) or the holder has been starting for longer than max_age_ms and is taken for
 * stuck. `holder_pid` (may be NULL) gets the holder's pid when it is known, else 0.
 */
#define TAKEOVER_START_NO_LOCK (-1)
#define TAKEOVER_START_BUSY (-2)
int takeover_start_lock(const char *dir, int max_age_ms, int *holder_pid);

/* How takeover_start_lock_wait ended (takeover_start_info_t.outcome). */
#define TAKEOVER_START_HELD 1     /* this process holds the lock */
#define TAKEOVER_START_NO_FILE 2  /* the lock file could not be opened (flock_errno has the errno) */
#define TAKEOVER_START_NO_FLOCK 3 /* flock failed other than "held" (flock_errno): no locks here */
#define TAKEOVER_START_STUCK 4    /* the holder has been starting for over max_age_ms */

typedef struct {
    int outcome;
    int flock_errno;
    int waited;              /* another helper held the lock when this one first asked */
    long long waited_ms;     /* how long this one waited (monotonic) */
    int holder_pid;          /* the last holder seen, 0 when unknown */
    long long holder_age_ms; /* its age when last read, -1 when unknown */
} takeover_start_info_t;

/*
 * The start-up gate main() uses. A helper that finds another one starting WAITS for it and never
 * leaves: the 12.6.13 helper left at once, so when the one starting then died or gave up before it
 * served (or hung, for every resend in the next two minutes) nothing served at all. Once the lock is
 * free this helper starts as any helper sent later would, so it takes over from the one that just
 * started: of two sent together the later one runs, as before the lock existed (of three or more, the
 * order the waiters go in is open), and two start-ups never overlap.
 *
 * Polls every `poll_ms`. Returns the lock descriptor, or TAKEOVER_START_NO_LOCK (start without it, as
 * before) when there are no locks here or the holder is stuck: its own stamp says it began more than
 * max_age_ms ago, or, its stamp unreadable, this helper has waited max_age_ms on it. Never returns
 * BUSY. `info` (may be NULL) says what happened, for the log.
 */
int takeover_start_lock_wait(const char *dir, int max_age_ms, int poll_ms, takeover_start_info_t *info);

/* Releases the lock (a descriptor from takeover_start_lock; negative is ignored). */
void takeover_start_unlock(int fd);

#endif
