#ifndef PS5UPLOAD2_AVA1_STOP_H
#define PS5UPLOAD2_AVA1_STOP_H

/* Result bits of ava1_payload_stop(). 0 = everything stopped cleanly. */
#define AVA1_STOP_CONNS_LEFT 1 /* a session was still open after the wait (its threads end with the process) */
#define AVA1_STOP_SONY_BUSY 2  /* a Sony call was still running after the wait */
#define AVA1_STOP_RPC_LEFT 4   /* a detached RPC worker was still running after the wait (final review: console) */

/*
 * The AVA1 half of a payload exit (node.shutdown, the takeover flag, a signal-free main return):
 *   1. stop accepting and tell every session to end (ava1_server_stop);
 *   2. wait up to conn_wait_ms for the sessions to leave, so no handler starts a new call;
 *   3. wait for an in-flight Sony call to finish (sony_api_lock is free). sony_wait_ms only decides
 *      when AVA1_STOP_SONY_BUSY is reported; the wait itself does not end until the lock frees (the
 *      caller's exit watchdog is the bound), so the process never exits inside a Sony call;
 *   4. stop the data layer: every job is stopped, its threads joined and its journal closed, so
 *      a durable job resumes after the next start with nothing lost.
 * The waits use CLOCK_MONOTONIC.
 */
int ava1_payload_stop(int conn_wait_ms, int sony_wait_ms);

/*
 * The exit watchdog's decision (final review: console). It used to _exit() after a flat 8 s, which
 * contradicted ava1_payload_stop's promise never to exit inside a Sony call (a cut call can wedge the
 * console). Now: nothing before `base_ms`; from there the process may go once no Sony call is in
 * flight (sony_busy == 0); while one is, it waits, up to the hard `ceiling_ms`, past which it goes
 * anyway (the caller logs that loudly). Returns AVA1_EXIT_WAIT / AVA1_EXIT_OK / AVA1_EXIT_FORCED.
 */
#define AVA1_EXIT_WAIT 0
#define AVA1_EXIT_OK 1
#define AVA1_EXIT_FORCED 2
int ava1_exit_decide(long long elapsed_ms, int sony_busy, long long base_ms, long long ceiling_ms);

/* Right before a forced _exit: fsync every job's journal and pack segments, bounded by max_ms (the
 * fsync can itself stall on a wedged drive, so it runs on a helper thread and is abandoned at the
 * bound). Journals are fsynced on every append already; this only makes the last ones certain.
 * Returns 0 when it finished, -1 when it was abandoned or no thread could be created (then nothing ran). */
int ava1_exit_flush(int max_ms);
/* Tests only (0 in the payload): makes ava1_exit_flush's thread creation fail. */
extern int ava1_exit_test_fail_create;

/*
 * Runs fire(arg) on a detached thread after delay_ms (CLOCK_MONOTONIC-independent: a plain sleep).
 * node.shutdown uses it so the reply, which leaves only after the handler returns, is on the wire
 * before anything starts to stop. 0 on success; -1 when the thread could not start (the caller
 * then runs fire itself: a late reply is better than no shutdown).
 */
int ava1_shutdown_defer(int delay_ms, void (*fire)(void *), void *arg);

#endif
