#include "indicator.h"

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#include <ps5/kernel.h>

/* /dev/icc_indicator, the LED's enable flag: 0 = off, 1 = on (the console drives it again).
 * Returned 0 for both values on a CFI-1115A, FW 13.60. */
#define ICC_INDICATOR_DEV      "/dev/icc_indicator"
#define ICC_INDICATOR_SET_FLAG 0x8001950eul

/* libkernel's own setters, each taking one int. They do not resolve through dlsym() in a
 * payload, but do through the SDK's kernel-side lookup on libkernel's handle (1): measured on
 * the same console, where sceKernelIccSetBuzzer(1) returned 0. */
#define LIBKERNEL_HANDLE 1u

typedef int (*icc_int_fn)(int);

/* A resolved libkernel function is a userland address. A failed lookup comes back as a small
 * number or a kernel errno, and calling that would take the helper down with it. */
static icc_int_fn resolve(const char *sym) {
    intptr_t p = kernel_dynlib_dlsym(getpid(), LIBKERNEL_HANDLE, sym);
    if (p <= 0x10000 || (uint64_t)p >= 0x0000800000000000ull) return NULL;
    return (icc_int_fn)p;
}

static int led_flag(uint8_t on) {
    uint8_t arg[8] = { on, 0 };
    int fd = open(ICC_INDICATOR_DEV, O_RDWR);
    if (fd < 0) return -errno;
    int rc = ioctl(fd, ICC_INDICATOR_SET_FLAG, arg) != 0 ? -errno : 0;
    close(fd);
    return rc;
}

int indicator_beep(int pattern) {
    if (pattern < 0 || pattern > 3) return INDICATOR_BAD_VALUE;
    icc_int_fn buzzer = resolve("sceKernelIccSetBuzzer");
    return buzzer ? buzzer(pattern) : INDICATOR_UNAVAILABLE;
}

int indicator_led(int on) {
    return led_flag(on ? 1 : 0);
}

int indicator_led_dim(int level) {
    if (level < 0 || level > 2) return INDICATOR_BAD_VALUE;
    icc_int_fn dim = resolve("sceKernelIccSetDynamicLedDimSetting");
    return dim ? dim(level) : INDICATOR_UNAVAILABLE;
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

    if (strcmp(argv[0], "led") == 0) {
        const char *sub = argc >= 2 ? argv[1] : "";
        if (strcmp(sub, "off") == 0 || strcmp(sub, "on") == 0) {
            int on = strcmp(sub, "on") == 0;
            int rc = indicator_led(on);
            if (rc != 0) say(out_text, out_exit, 1, "led %d: errno %d\n", on, -rc);
            else say(out_text, out_exit, 0, on ? "led on: ok\n" : "led off: ok\n", 0, 0);
            return 1;
        }
        if (strcmp(sub, "dim") == 0) {
            int level = argc >= 3 ? small_int(argv[2], 0, 2) : -1;
            if (level < 0) {
                say(out_text, out_exit, 2, "usage: led dim <0-2>\n", 0, 0);
                return 1;
            }
            int rc = indicator_led_dim(level);
            if (rc == INDICATOR_UNAVAILABLE)
                say(out_text, out_exit, 1, "led: the console's brightness call is not available here\n", 0, 0);
            else if (rc != 0) say(out_text, out_exit, 1, "led dim: the console answered 0x%08x\n", rc, 0);
            else say(out_text, out_exit, 0, "led dim %d: ok\n", level, 0);
            return 1;
        }
        say(out_text, out_exit, 2, "usage: led off | on | dim <0-2>\n", 0, 0);
        return 1;
    }
    return 0;
}
