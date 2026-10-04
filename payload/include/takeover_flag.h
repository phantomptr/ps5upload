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

/* 1 when something accepts a TCP connection on 127.0.0.1:port. */
int takeover_port_responding(int port);

/* Writes the flag, then waits until none of `ports` answers on loopback: `attempts` checks,
 * `interval_us` apart (the loop counts checks, not time). 0 when every port is free (the flag is
 * unlinked), -1 when one still answers after the last check (the flag stays for the old instance). */
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

#endif
