/* Host test for mp1_clock.c with a fake /dev/mp1: good tables parse, out-of-range values read as
 * 0, the 2 s cache holds, the first refused open/ioctl latches "unsupported" for good (no further
 * driver calls), "not now" (a busy SMU during a game) does not and keeps the last values, a driver
 * writing past the 108-byte table is refused, and only the two read requests ever reach the driver. */
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include "../src/mp1_clock.c"

static int fails;
#define CHECK(cond)                                                    \
    do {                                                               \
        if (!(cond)) {                                                 \
            printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond);    \
            fails++;                                                   \
        }                                                              \
    } while (0)

typedef struct {
    int64_t now;
    int opens, closes, gets, gfx_gets, core_gets, other_reqs;
    int open_fails, gfx_fails;
    int err;          /* the errno of a failing call; 0 = each call's own (a refusal) */
    int core_fail_at; /* core index whose read fails; -1 never */
    int spill;        /* write a word past the 27-word table */
    int fd_open;      /* currently open fds */
    uint32_t gfx[MP1_GFX_WORDS];
    uint32_t core_mhz[MP1_CORES];
    uint32_t core_pstate[MP1_CORES];
} fake_t;

static int fake_open(void *ctx) {
    fake_t *f = ctx;
    f->opens++;
    if (f->open_fails) {
        errno = f->err ? f->err : ENOENT;
        return -1;
    }
    f->fd_open++;
    return 42;
}

static int fake_get(void *ctx, int fd, unsigned long req, void *buf) {
    fake_t *f = ctx;
    f->gets++;
    if (fd != 42) return -1;
    if (req == MP1_GFX_IOCTL_GET) {
        f->gfx_gets++;
        if (((uintptr_t)buf & 7u) != 0) return -1; /* alignment promised by the reader */
        if (f->gfx_fails) {
            errno = f->err ? f->err : ENOTTY;
            return -1;
        }
        memcpy(buf, f->gfx, sizeof f->gfx); /* exactly IOCPARM_LEN = 108 bytes */
        if (f->spill) ((uint32_t *)buf)[40] = 0xdeadbeef;
        return 0;
    }
    if (req == MP1_CORE_IOCTL_GET) {
        f->core_gets++;
        uint32_t *q = buf;
        uint32_t core = q[0];
        if (q[1] != 0 || q[2] != 0 || core >= MP1_CORES) return -1; /* zeroed, core set */
        if (f->core_fail_at == (int)core) {
            errno = f->err ? f->err : EINVAL;
            return -1;
        }
        q[1] = f->core_pstate[core];
        q[2] = f->core_mhz[core];
        return 0;
    }
    f->other_reqs++;
    return 0;
}

static void fake_close(void *ctx, int fd) {
    fake_t *f = ctx;
    if (fd == 42) f->fd_open--;
    f->closes++;
}

static int64_t fake_now(void *ctx) { return ((fake_t *)ctx)->now; }

static void fake_init(fake_t *f, mp1_ops_t *ops) {
    memset(f, 0, sizeof *f);
    f->now = 100000;
    f->core_fail_at = -1;
    for (int i = 0; i < MP1_GFX_WORDS; i++) f->gfx[i] = 0x1000u + (uint32_t)i;
    f->gfx[MP1_GFX_MHZ_WORD] = 2230;
    for (int c = 0; c < MP1_CORES; c++) {
        f->core_mhz[c] = c == 0 ? 3500 : 2844;
        f->core_pstate[c] = c == 0 ? 0 : 2;
    }
    ops->open_ro = fake_open;
    ops->get = fake_get;
    ops->close_fd = fake_close;
    ops->now_ms = fake_now;
    ops->ctx = f;
    mp1_clocks_reset();
}

static void test_good_read(void) {
    fake_t f;
    mp1_ops_t ops;
    fake_init(&f, &ops);
    mp1_clocks_t c;
    CHECK(mp1_clocks_read(&ops, &c) == MP1_OK);
    CHECK(c.state == MP1_OK);
    CHECK(c.gpu_mhz == 2230);
    CHECK(c.core_mhz[0] == 3500 && c.core_mhz[7] == 2844);
    CHECK(c.core_pstate[0] == 0 && c.core_pstate[3] == 2);
    CHECK(c.gfx_raw[0] == 0x1000u && c.gfx_raw[MP1_GFX_MHZ_WORD] == 2230);
    CHECK(f.opens == 1 && f.closes == 1 && f.fd_open == 0);
    CHECK(f.gfx_gets == 1 && f.core_gets == MP1_CORES);
    CHECK(f.other_reqs == 0);

    char buf[1024];
    int n = mp1_clocks_format(&c, 0, buf, sizeof buf);
    CHECK(n > 0 && (size_t)n < sizeof buf);
    CHECK(strstr(buf, "gpu_clock_src=mp1\n") != NULL);
    CHECK(strstr(buf, "cpu_core_mhz=3500,2844,2844,2844,2844,2844,2844,2844\n") != NULL);
    CHECK(strstr(buf, "cpu_core_mhz_min=2844\n") != NULL);
    CHECK(strstr(buf, "cpu_core_mhz_max=3500\n") != NULL);
    CHECK(strstr(buf, "mp1_clk_raw=") == NULL);

    n = mp1_clocks_format(&c, 1, buf, sizeof buf);
    CHECK(n > 0 && (size_t)n < sizeof buf);
    CHECK(strstr(buf, "mp1_clk_raw=00001000,00001001,") != NULL);
    CHECK(strstr(buf, ",000008b6,") != NULL); /* 2230 at word 21 */
    CHECK(strstr(buf, "mp1_core_raw=0:3500,2:2844,") != NULL);

    /* Too small a buffer reports the length it needed, never overruns. */
    char tiny[16];
    memset(tiny, 'x', sizeof tiny);
    n = mp1_clocks_format(&c, 1, tiny, 10);
    CHECK(n >= 10 && tiny[9] == '\0' && tiny[10] == 'x');
}

static void test_cache(void) {
    fake_t f;
    mp1_ops_t ops;
    fake_init(&f, &ops);
    mp1_clocks_t c;
    CHECK(mp1_clocks_read(&ops, &c) == MP1_OK);
    f.gfx[MP1_GFX_MHZ_WORD] = 1800;
    f.now += 1999;
    CHECK(mp1_clocks_read(&ops, &c) == MP1_OK);
    CHECK(c.gpu_mhz == 2230);              /* served from the cache */
    CHECK(f.opens == 1 && f.gets == 1 + MP1_CORES);
    f.now += 1; /* 2000 ms: stale */
    CHECK(mp1_clocks_read(&ops, &c) == MP1_OK);
    CHECK(c.gpu_mhz == 1800);
    CHECK(f.opens == 2);
    f.now -= 5000; /* clock went backwards: not a cache hit */
    CHECK(mp1_clocks_read(&ops, &c) == MP1_OK);
    CHECK(f.opens == 3);
}

static void test_out_of_range(void) {
    fake_t f;
    mp1_ops_t ops;
    fake_init(&f, &ops);
    f.gfx[MP1_GFX_MHZ_WORD] = 99;
    f.core_mhz[1] = 399;
    f.core_mhz[2] = 4001;
    f.core_mhz[3] = 0xFFFFFFFFu;
    f.core_pstate[4] = 64;
    mp1_clocks_t c;
    CHECK(mp1_clocks_read(&ops, &c) == MP1_OK);
    CHECK(c.gpu_mhz == 0);
    CHECK(c.core_mhz[0] == 3500 && c.core_mhz[1] == 0 && c.core_mhz[2] == 0 && c.core_mhz[3] == 0);
    CHECK(c.core_raw_mhz[2] == 4001);
    CHECK(c.core_pstate[4] == 0xFFFFFFFFu);
    char buf[1024];
    CHECK(mp1_clocks_format(&c, 1, buf, sizeof buf) > 0);
    CHECK(strstr(buf, "cpu_core_mhz=3500,0,0,0,2844,") != NULL);
    CHECK(strstr(buf, "cpu_core_mhz_min=2844\n") != NULL);
    CHECK(strstr(buf, ",-:2844,") != NULL); /* core 4: pstate not reported */

    mp1_clocks_reset();
    f.gfx[MP1_GFX_MHZ_WORD] = 2501;
    for (int i = 0; i < MP1_CORES; i++) f.core_mhz[i] = 0;
    CHECK(mp1_clocks_read(&ops, &c) == MP1_OK);
    CHECK(c.gpu_mhz == 0);
    CHECK(mp1_clocks_format(&c, 0, buf, sizeof buf) > 0);
    CHECK(strstr(buf, "cpu_core_mhz=0,0,0,0,0,0,0,0\ncpu_core_mhz_min=0\ncpu_core_mhz_max=0\n") != NULL);

    uint32_t t[MP1_GFX_WORDS] = {0};
    t[MP1_GFX_MHZ_WORD] = 100;
    CHECK(mp1_gpu_mhz_from_table(t) == 100);
    t[MP1_GFX_MHZ_WORD] = 2500;
    CHECK(mp1_gpu_mhz_from_table(t) == 2500);
    CHECK(mp1_core_mhz_valid(400) == 400 && mp1_core_mhz_valid(4000) == 4000);
}

/* After any failure: unsupported, all zero, and the driver is never touched again. */
static void expect_latched(fake_t *f, mp1_ops_t *ops) {
    int opens = f->opens, gets = f->gets;
    mp1_clocks_t c;
    for (int i = 0; i < 3; i++) {
        f->now += 10000;
        CHECK(mp1_clocks_read(ops, &c) == MP1_UNSUPPORTED);
        CHECK(c.state == MP1_UNSUPPORTED && c.gpu_mhz == 0 && c.core_mhz[0] == 0);
    }
    CHECK(f->opens == opens && f->gets == gets);
    CHECK(f->fd_open == 0);
    char buf[512];
    CHECK(mp1_clocks_format(&c, 1, buf, sizeof buf) > 0);
    CHECK(strstr(buf, "gpu_clock_src=unsupported\ncpu_core_mhz=0,0,0,0,0,0,0,0\n") != NULL);
    CHECK(strstr(buf, "mp1_clk_raw") == NULL);
}

static void test_latch(void) {
    fake_t f;
    mp1_ops_t ops;
    mp1_clocks_t c;

    fake_init(&f, &ops);
    f.open_fails = 1;
    CHECK(mp1_clocks_read(&ops, &c) == MP1_UNSUPPORTED);
    CHECK(f.opens == 1 && f.gets == 0);
    f.open_fails = 0; /* recovery is not retried */
    expect_latched(&f, &ops);

    fake_init(&f, &ops);
    f.gfx_fails = 1;
    CHECK(mp1_clocks_read(&ops, &c) == MP1_UNSUPPORTED);
    CHECK(f.gets == 1 && f.core_gets == 0 && f.closes == 1);
    f.gfx_fails = 0;
    expect_latched(&f, &ops);

    fake_init(&f, &ops);
    f.core_fail_at = 3;
    CHECK(mp1_clocks_read(&ops, &c) == MP1_UNSUPPORTED);
    CHECK(f.core_gets == 4 && f.closes == 1);
    CHECK(c.gpu_mhz == 0); /* nothing half-read is reported */
    f.core_fail_at = -1;
    expect_latched(&f, &ops);

    /* A good read is cached, then a failure on the next real read latches and drops it. */
    fake_init(&f, &ops);
    CHECK(mp1_clocks_read(&ops, &c) == MP1_OK);
    f.now += MP1_CACHE_MS;
    f.gfx_fails = 1;
    CHECK(mp1_clocks_read(&ops, &c) == MP1_UNSUPPORTED);
    f.gfx_fails = 0;
    expect_latched(&f, &ops);

    /* A driver that writes past the 108-byte table is not the one this was written for. */
    fake_init(&f, &ops);
    f.spill = 1;
    CHECK(mp1_clocks_read(&ops, &c) == MP1_UNSUPPORTED);
    CHECK(f.core_gets == 0 && f.closes == 1);
    f.spill = 0;
    expect_latched(&f, &ops);
}

/* "Not now": a game holds the SMU, and one Read sensors click must not cost the session its clocks. */
static void test_not_now_does_not_latch(void) {
    fake_t f;
    mp1_ops_t ops;
    mp1_clocks_t c;
    static const int not_now[] = {EBUSY, EAGAIN, EDEADLK, EINTR, ETIMEDOUT, EMFILE, ENFILE, ENOMEM};
    static const int refused[] = {ENOTTY, ENXIO, ENOENT, EPERM, EACCES, ENODEV, EINVAL, EFAULT, 163};

    for (size_t i = 0; i < sizeof not_now / sizeof not_now[0]; i++) CHECK(mp1_errno_transient(not_now[i]));
    for (size_t i = 0; i < sizeof refused / sizeof refused[0]; i++) CHECK(!mp1_errno_transient(refused[i]));

    /* A good read, then the driver is busy: the last values stay the answer, on every failing step. */
    fake_init(&f, &ops);
    CHECK(mp1_clocks_read(&ops, &c) == MP1_OK);
    CHECK(c.gpu_mhz == 2230);
    f.err = EBUSY;
    f.gfx_fails = 1;
    f.now += MP1_CACHE_MS;
    CHECK(mp1_clocks_read(&ops, &c) == MP1_OK);
    CHECK(c.state == MP1_OK && c.gpu_mhz == 2230 && c.core_mhz[0] == 3500);
    CHECK(f.fd_open == 0);
    f.gfx_fails = 0;
    f.core_fail_at = 5;
    f.err = EAGAIN;
    CHECK(mp1_clocks_read(&ops, &c) == MP1_OK);
    CHECK(c.gpu_mhz == 2230);
    f.core_fail_at = -1;
    f.open_fails = 1;
    f.err = EMFILE;
    CHECK(mp1_clocks_read(&ops, &c) == MP1_OK);
    CHECK(c.gpu_mhz == 2230);
    f.open_fails = 0;
    f.err = 0;
    CHECK(f.fd_open == 0);

    /* And once the driver answers, it is read again, with new values. */
    f.gfx[MP1_GFX_MHZ_WORD] = 1800;
    CHECK(mp1_clocks_read(&ops, &c) == MP1_OK);
    CHECK(c.gpu_mhz == 1800);

    /* Busy from the first read: no values to keep, so zeros = "unknown", not "unsupported". */
    fake_init(&f, &ops);
    f.gfx_fails = 1;
    f.err = EBUSY;
    CHECK(mp1_clocks_read(&ops, &c) == MP1_OK);
    CHECK(c.state == MP1_OK && c.gpu_mhz == 0 && c.core_mhz[0] == 0);
    char buf[512];
    CHECK(mp1_clocks_format(&c, 0, buf, sizeof buf) > 0);
    CHECK(strstr(buf, "gpu_clock_src=mp1\n") != NULL);
    f.gfx_fails = 0;
    f.err = 0;
    CHECK(mp1_clocks_read(&ops, &c) == MP1_OK);
    CHECK(c.gpu_mhz == 2230);

    /* Busy does not excuse a real refusal later. */
    f.now += MP1_CACHE_MS;
    f.gfx_fails = 1;
    CHECK(mp1_clocks_read(&ops, &c) == MP1_UNSUPPORTED);
    f.gfx_fails = 0;
    expect_latched(&f, &ops);
}

static void test_only_reads_reach_the_driver(void) {
    fake_t f;
    mp1_ops_t ops;
    fake_init(&f, &ops);
    uint32_t v[4] = {0};
    /* The clock-changing requests of the same driver, built from their parts so the request
     * numbers never appear in payload/ (check-mp1-readonly.sh). */
    const unsigned long grp = 0xAEUL << 8;
    const unsigned long in4 = 0x80000000UL | (4UL << 16), inout4 = 0xC0000000UL | (4UL << 16);
    const unsigned long in8 = 0x80000000UL | (8UL << 16);
    const unsigned long changes[] = { in4 | grp | 0x17, inout4 | grp | 0x18, in8 | grp | 0x1D };
    for (size_t i = 0; i < sizeof changes / sizeof changes[0]; i++) {
        errno = 0;
        CHECK(mp1_get(&ops, 42, changes[i], v) == -1);
        CHECK(errno == EPERM);
    }
    CHECK(f.gets == 0 && f.other_reqs == 0);
    _Alignas(8) uint32_t table[MP1_GFX_BUF_WORDS] = {0};
    CHECK(mp1_get(&ops, 42, MP1_GFX_IOCTL_GET, table) == 0); /* a read reaches the fake */
    CHECK(f.gets == 1 && table[MP1_GFX_MHZ_WORD] == 2230);
    /* The numbers mp1_clock.c is allowed: IOC_OUT 108 bytes #1, IOC_INOUT 12 bytes #0x1E. */
    CHECK(MP1_GFX_IOCTL_GET == (0x40000000UL | (108UL << 16) | grp | 0x01));
    CHECK(MP1_CORE_IOCTL_GET == (0xC0000000UL | (12UL << 16) | grp | 0x1E));
    CHECK(MP1_GFX_WORDS * 4 == 108 && MP1_GFX_BUF_WORDS * 4 == 256);
}

int main(void) {
    test_good_read();
    test_cache();
    test_out_of_range();
    test_latch();
    test_not_now_does_not_latch();
    test_only_reads_reach_the_driver();
    if (fails) {
        printf("mp1_clock_selftest: %d failure(s)\n", fails);
        return 1;
    }
    printf("mp1_clock_selftest: ok\n");
    return 0;
}
