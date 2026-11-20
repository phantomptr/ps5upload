#ifndef PS5UPLOAD2_START_DEDUPE_H
#define PS5UPLOAD2_START_DEDUPE_H

#include <stddef.h>
#include <stdint.h>

/*
 * Two helpers sent together (12.6.15).
 *
 * The start lock (takeover_flag.h) makes the second of two helpers sent at once wait until the
 * first serves, and then it took over from it: two restarts back to back, each dropping every
 * connection, for one helper's worth of sends. Now the one that waited looks first: when the
 * transfer port is already served by a helper of the same or a newer version that started
 * serving moments ago, and that helper really answers on loopback, the waiter exits quietly
 * without touching anything. In every other case (no record, an older helper, one that started
 * long ago, one that does not answer) it takes over as before, so nothing ever ends with nobody
 * serving (12.6.13's bug).
 *
 * The serving record (<runtime>/serving) is written by a helper once its transfer port serves:
 * "pid=<pid>\nversion=<v>\nmono_ms=<CLOCK_MONOTONIC ms>\n". The monotonic clock is system-wide
 * and the app's clock changes (settimeofday) do not move it. It is written in place (no rename:
 * cross_device.h) and only newline-terminated lines count, so a read that catches it mid-write is
 * incomplete, which reads as "no record".
 */

#define START_DEDUPE_RECENT_MS 120000LL

typedef struct {
    int pid;
    char version[32];
    long long mono_ms;
} serving_rec_t;

/* Formats a record. The length, or -1 when it does not fit or a field is missing. */
int serving_record_format(char *buf, size_t n, const serving_rec_t *r);
/* Parses one. 1 when pid, version and mono_ms are all present (newline-terminated), else 0. */
int serving_record_parse(const char *text, size_t len, serving_rec_t *r);
/* <dir>/serving: written whole in place; read (1 when complete); removed when it is `pid`'s. */
int serving_record_write(const char *dir, const serving_rec_t *r);
int serving_record_read(const char *dir, serving_rec_t *r);
void serving_record_clear(const char *dir, int pid);

/* Orders two helper versions ("12.6.15", "12.7.0-rc.1"): -1, 0 or 1; -2 when either does not
 * read as one. A pre-release is below its release. */
int start_dedupe_version_cmp(const char *a, const char *b);

/* Whether the helper that waited should leave the start to the one in `rec` (still to be
 * confirmed by start_dedupe_probe_serves): a complete record of another process that is alive,
 * of the same or a newer version than `mine`, that began serving within START_DEDUPE_RECENT_MS of
 * `now_mono_ms`. */
int start_dedupe_should_stand_down(const serving_rec_t *rec, int rec_complete, const char *mine,
                                   long long now_mono_ms, int self_pid, int pid_alive);

/* 1 when an AVA1 server on 127.0.0.1:port answers within timeout_ms: an empty Hs1 frame is sent,
 * and the server's reply frame (an Error, which ends the connection) is the round trip. It touches
 * no pairing state: the frame is refused before any handshake. */
int start_dedupe_probe_serves(int port, int timeout_ms);

/* CLOCK_MONOTONIC in ms. */
long long start_dedupe_mono_ms(void);

#endif
