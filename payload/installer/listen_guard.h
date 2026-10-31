#ifndef PS5UPLOAD_INSTALLER_LISTEN_GUARD_H
#define PS5UPLOAD_INSTALLER_LISTEN_GUARD_H

/* Accept-loop policy shared by the :9115 listener and the loopback HTTP
 * server. An accept loop must never spin and never give up: after rest mode
 * or descriptor exhaustion accept() can fail on every call, and an errno the
 * loop did not expect (Sony's 163, once) must not end the server. */
#include <stdint.h>

/* How long a listener may fail continuously before it is rebuilt. */
#define INST_LISTEN_BROKEN_MS  5000u
/* The nap after an accept/poll error, so a failing listener costs no CPU. */
#define INST_LISTEN_BACKOFF_MS 100u
/* Poll slice: how soon a stop request is noticed by an idle listener. */
#define INST_LISTEN_SLICE_MS   250

typedef enum {
    INST_ACCEPT_RETRY = 0,   /* transient (EINTR, ECONNABORTED, EAGAIN): try again now */
    INST_ACCEPT_BUSY,        /* out of fds/buffers: back off; the listener itself is fine */
    INST_ACCEPT_BROKEN,      /* anything else, known or not: back off, count toward a rebuild */
} inst_accept_action_t;

inst_accept_action_t inst_accept_classify(int err);

/* 1 when a listener failing since `err_since_ms` (0 = not failing) has failed
 * for INST_LISTEN_BROKEN_MS by `now_ms`. */
int inst_listen_broken(uint64_t err_since_ms, uint64_t now_ms);

/* CLOCK_MONOTONIC milliseconds; never 0 (0 means "not failing" above). */
uint64_t inst_mono_ms(void);

void inst_nap_ms(unsigned ms);

/* Take a connection slot: 1 when `*count` was below `max` (and is now one
 * higher), 0 when full. inst_slot_release gives one back. */
int  inst_slot_take(volatile int *count, int max);
void inst_slot_release(volatile int *count);

#endif /* PS5UPLOAD_INSTALLER_LISTEN_GUARD_H */
