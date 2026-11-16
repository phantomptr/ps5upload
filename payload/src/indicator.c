#include <pthread.h>

#include "kernel_rw_lock.h"
#include "indicator.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <ps5/kernel.h>

/* libkernel's own setters, each taking one int. They do not resolve through dlsym() in a
 * payload, but do through the SDK's kernel-side lookup on libkernel's handle (1): measured on
 * a CFI-1115A, FW 13.60, where sceKernelIccSetBuzzer(1) returned 0 and beeped.
 *
 * The front light is left alone (#840): /dev/icc_indicator's enable flag (ioctl 0x8001950e) and
 * sceKernelIccSetDynamicLedDimSetting both returned 0 on that console and changed nothing. */
#define LIBKERNEL_HANDLE 1u

typedef int (*icc_int_fn)(int);

/* A resolved libkernel function is a userland address. A failed lookup comes back as a small
 * number or a kernel errno, and calling that would take the helper down with it. */
static icc_int_fn resolve(const char *sym) {
    intptr_t p;
    /* A kernel read: inside the shared kernel-R/W lock (kernel_rw_lock.h). */
    pthread_mutex_lock(&kernel_rw_lock);
    p = kernel_dynlib_dlsym(getpid(), LIBKERNEL_HANDLE, sym);
    pthread_mutex_unlock(&kernel_rw_lock);
    if (p <= 0x10000 || (uint64_t)p >= 0x0000800000000000ull) return NULL;
    return (icc_int_fn)p;
}

int indicator_beep(int pattern) {
    if (pattern < 0 || pattern > 3) return INDICATOR_BAD_VALUE;
    icc_int_fn buzzer = resolve("sceKernelIccSetBuzzer");
    return buzzer ? buzzer(pattern) : INDICATOR_UNAVAILABLE;
}

/* A whole number in [lo, hi], or -1. */
static int small_int(const char *s, int lo, int hi) {
    if (!s || !s[0] || s[1]) return -1;
    int v = s[0] - '0';
    return (v >= lo && v <= hi) ? v : -1;
}

static void say(char **out_text, int *out_exit, int exit_code, const char *fmt, int a, int b) {
    char buf[160];
    snprintf(buf, sizeof(buf), fmt, a, b);
    *out_text = strdup(buf);
    *out_exit = exit_code;
}

int indicator_shell(int argc, char *argv[], char **out_text, int *out_exit) {
    if (argc < 1 || !out_text || !out_exit) return 0;

    if (strcmp(argv[0], "beep") == 0) {
        int pattern = argc >= 2 ? small_int(argv[1], 0, 3) : 1;
        if (pattern < 0) {
            say(out_text, out_exit, 2, "usage: beep [0-3]\n", 0, 0);
            return 1;
        }
        int rc = indicator_beep(pattern);
        if (rc == INDICATOR_UNAVAILABLE)
            say(out_text, out_exit, 1, "beep: the console's beeper call is not available here\n", 0, 0);
        else if (rc != 0) say(out_text, out_exit, 1, "beep: the console answered 0x%08x\n", rc, 0);
        else say(out_text, out_exit, 0, "beep %d: ok\n", pattern, 0);
        return 1;
    }
    return 0;
}
