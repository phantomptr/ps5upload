/* Review 010: exposes the static-inline reap decision to the Rust tests, and a counting start
 * callback for the single-instance gate. */
#include "instance_verdict.h"
#include "takeover_flag.h"

int t10_reap_decision(int ours, unsigned long long rec_started, unsigned long long boot, unsigned long long pstart,
                      int known, unsigned long long now) {
    return (int)instance_reap_decision(ours, rec_started, boot, pstart, known, now);
}

static int g_starts;
static void count_start(void *ctx) {
    (void)ctx;
    g_starts++;
}

/* Runs the gate with a counting start; *starts = how many times start ran. Returns the gate's value. */
int t10_gate(int port, int max_ms, int interval_ms, int *starts) {
    int r;
    g_starts = 0;
    r = takeover_gate_start(port, max_ms, interval_ms, count_start, 0);
    *starts = g_starts;
    return r;
}
