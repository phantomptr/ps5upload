#include "listen_guard.h"

#include <errno.h>
#include <time.h>

inst_accept_action_t inst_accept_classify(int err) {
    switch (err) {
    case EINTR:
    case ECONNABORTED:
    case EAGAIN:
#if defined(EWOULDBLOCK) && EWOULDBLOCK != EAGAIN
    case EWOULDBLOCK:
#endif
        return INST_ACCEPT_RETRY;
    case EMFILE:
    case ENFILE:
    case ENOBUFS:
    case ENOMEM:
        return INST_ACCEPT_BUSY;
    default:
        return INST_ACCEPT_BROKEN;
    }
}

int inst_listen_broken(uint64_t err_since_ms, uint64_t now_ms) {
    return err_since_ms != 0 && now_ms >= err_since_ms &&
           now_ms - err_since_ms >= INST_LISTEN_BROKEN_MS;
}

uint64_t inst_mono_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    uint64_t ms = (uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u;
    return ms ? ms : 1;
}

void inst_nap_ms(unsigned ms) {
    struct timespec ts = { (time_t)(ms / 1000u), (long)(ms % 1000u) * 1000000L };
    while (nanosleep(&ts, &ts) != 0 && errno == EINTR) {}
}

int inst_slot_take(volatile int *count, int max) {
    if (__sync_add_and_fetch(count, 1) > max) {
        __sync_sub_and_fetch(count, 1);
        return 0;
    }
    return 1;
}

void inst_slot_release(volatile int *count) {
    __sync_sub_and_fetch(count, 1);
}
