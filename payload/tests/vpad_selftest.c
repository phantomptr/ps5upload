/* Host-side tests for Play Together's virtual controllers (vpad.c) over a fake /dev/hid: the
 * request layouts the console takes; open, resume and busy; who a pad is made for; seq ordering and
 * the batch; the watchdog resting a silent pad once and removing an abandoned one (by hand and on
 * its own thread); close; output; status; and the helper stopping.
 * Build: see the root Makefile test-payload. */
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "vpad.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

#define B(s) s, strlen(s)
#define HAS(s) (strstr(g_out, s) != NULL)

static char g_out[VPAD_OUT_MAX];

#define TOK_A "0123456789abcdef0123456789abcdef"
#define TOK_B "fedcba9876543210fedcba9876543210"

/* ---- a fake /dev/hid that keeps what each pad was sent ---- */

#define MAXH 16
typedef struct {
    int live;
    uint32_t user;
    uint8_t cls, subtype;
    uint8_t block[VPAD_BLOCK]; /* the last state inserted */
    int inserts;
    int deleted;
} fake_pad_t;

static pthread_mutex_t f_mtx = PTHREAD_MUTEX_INITIALIZER;
static fake_pad_t f_pads[MAXH];
static int f_next_handle = 1, f_next_fd = 100;
static int f_open_err, f_add_err, f_insert_err, f_output_err;
static int f_opens, f_closes, f_adds, f_deletes;
static uint8_t f_motors[2] = {10, 20}, f_color[3] = {1, 2, 3};

static void fake_reset(void) {
    pthread_mutex_lock(&f_mtx);
    memset(f_pads, 0, sizeof f_pads);
    f_next_handle = 1;
    f_open_err = f_add_err = f_insert_err = f_output_err = 0;
    f_opens = f_closes = f_adds = f_deletes = 0;
    pthread_mutex_unlock(&f_mtx);
}

static int f_open(void) {
    if (f_open_err) return f_open_err;
    f_opens++;
    return f_next_fd++;
}
static void f_close(int fd) {
    (void)fd;
    f_closes++;
}
static uint64_t rd64(const uint8_t *p) {
    uint64_t v;
    memcpy(&v, p, 8);
    return v;
}
static uint32_t rd32(const uint8_t *p) {
    uint32_t v;
    memcpy(&v, p, 4);
    return v;
}
static int f_ioctl(int fd, unsigned long req, void *arg) {
    const uint8_t *a = arg;
    int rc = 0;
    (void)fd;
    pthread_mutex_lock(&f_mtx);
    if (req == VPAD_IOCTL_ADD) {
        /* {u32 op = 3; pad; u64 desc*; u64 int* handle} */
        const uint8_t *desc = (const uint8_t *)(uintptr_t)rd64(a + 8);
        int *h = (int *)(uintptr_t)rd64(a + 16);
        f_adds++;
        if (rd32(a) != 3) rc = -EINVAL;
        else if (f_add_err) rc = f_add_err;
        else if (f_next_handle >= MAXH) rc = -ENOSPC;
        else {
            fake_pad_t *p = &f_pads[f_next_handle];
            memset(p, 0, sizeof *p);
            p->live = 1;
            p->cls = desc[0];
            p->user = rd32(desc + 0x1C);
            p->subtype = desc[0x20];
            *h = f_next_handle++;
        }
    } else if (req == VPAD_IOCTL_INSERT) {
        /* {u32 handle; pad; u64 state*; u8 0} */
        uint32_t h = rd32(a);
        if (f_insert_err) rc = f_insert_err;
        else if (h >= MAXH || !f_pads[h].live) rc = -ENOENT;
        else {
            memcpy(f_pads[h].block, (const uint8_t *)(uintptr_t)rd64(a + 8), VPAD_BLOCK);
            f_pads[h].inserts++;
        }
    } else if (req == VPAD_IOCTL_DELETE) {
        /* {u8 op = 5; pad7; u32 handle} */
        uint32_t h = rd32(a + 8);
        f_deletes++;
        if (a[0] != 5 || h >= MAXH || !f_pads[h].live) rc = -EINVAL;
        else {
            f_pads[h].live = 0;
            f_pads[h].deleted = 1;
        }
    } else if (req == VPAD_IOCTL_OUTPUT) {
        /* {u32 handle; pad; u64 fields[12]}: motors is field 2, color field 3 */
        uint32_t h = rd32(a);
        if (f_output_err) rc = f_output_err;
        else if (h >= MAXH || !f_pads[h].live) rc = -ENOENT;
        else {
            memcpy((uint8_t *)(uintptr_t)rd64(a + 8 + 2 * 8), f_motors, 2);
            memcpy((uint8_t *)(uintptr_t)rd64(a + 8 + 3 * 8), f_color, 3);
            *(uint32_t *)(uintptr_t)rd64(a + 8) = 1;      /* flag_a */
            *(uint32_t *)(uintptr_t)rd64(a + 8 + 11 * 8) = 1; /* flag_b */
        }
    } else {
        rc = -ENOTTY;
    }
    pthread_mutex_unlock(&f_mtx);
    return rc;
}
static const vpad_hid_t FAKE = {f_open, f_ioctl, f_close};

static int g_picks;
static uint32_t g_pick_uid = 0x1e24b13c;
static uint32_t pick(void) {
    g_picks++;
    return g_pick_uid;
}

static int vopen(const char *body, long long now) {
    return vpad_open_json(B(body), now, pick, g_out, sizeof g_out);
}
static int vstate(const char *body, long long now) {
    return vpad_state_json(B(body), now, g_out, sizeof g_out);
}
static int vstates(const char *body, long long now) {
    return vpad_states_json(B(body), now, g_out, sizeof g_out);
}
static int vhb(const char *body, long long now) {
    return vpad_hb_json(B(body), now, g_out, sizeof g_out);
}
static int vclose(const char *body, long long now) {
    return vpad_close_json(B(body), now, g_out, sizeof g_out);
}
static int voutput(const char *body, long long now) {
    return vpad_output_json(B(body), now, g_out, sizeof g_out);
}

/* The handle a slot's pad got, from the open answer. */
static int handle_from_answer(void) {
    const char *p = strstr(g_out, "\"handle\":");
    return p ? atoi(p + 9) : -1;
}

static float fl(const uint8_t *p) {
    float v;
    memcpy(&v, p, 4);
    return v;
}

static int block_is_neutral(const uint8_t *b) {
    return rd32(b + 0x0C) == 0 && b[0x10] == 128 && b[0x11] == 128 && b[0x12] == 128 && b[0x13] == 128 &&
           b[0x14] == 0 && b[0x15] == 0 && b[0x17] == 1 && b[0x1C] == 0 && fl(b + 0x78 + 12) == 1.0f &&
           fl(b + 0x88) == 0.0f && fl(b + 0x94 + 4) == 1.0f;
}

static void fresh(void) {
    vpad_set_hid(&FAKE); /* removes every pad and clears a stop */
    fake_reset();
    g_picks = 0;
    g_pick_uid = 0x1e24b13c;
}

/* ---- tests ---- */

static void test_encode(void) {
    vpad_state_t s;
    uint8_t b[VPAD_BLOCK];
    vpad_state_neutral(&s);
    vpad_state_encode(&s, b);
    CHECK(block_is_neutral(b));
    CHECK(fl(b + 0x78) == 0.0f && fl(b + 0x94) == 0.0f && fl(b + 0x94 + 8) == 0.0f);
    s.buttons = VPAD_BTN_CROSS | VPAD_BTN_PS;
    s.lx = 0;
    s.ly = 255;
    s.rx = 10;
    s.ry = 20;
    s.l2 = 30;
    s.r2 = 40;
    s.touch_n = 2;
    s.touch[0] = (vpad_touch_t){7, 1919, 1079};
    s.touch[1] = (vpad_touch_t){8, 0x1234, 2};
    s.gyro[2] = -1.5f;
    vpad_state_encode(&s, b);
    CHECK(rd32(b + 0x0C) == 0x14000u);
    CHECK(b[0x10] == 0 && b[0x11] == 255 && b[0x12] == 10 && b[0x13] == 20 && b[0x14] == 30 && b[0x15] == 40);
    CHECK(b[0x17] == 1 && b[0x1C] == 2);
    CHECK(b[0x20] == 7 && b[0x24] == (1919 & 0xFF) && b[0x25] == (1919 >> 8) && b[0x26] == (1079 & 0xFF) &&
          b[0x27] == (1079 >> 8));
    CHECK(b[0x28] == 8 && b[0x2C] == 0x34 && b[0x2D] == 0x12 && b[0x2E] == 2 && b[0x2F] == 0);
    CHECK(fl(b + 0x88 + 8) == -1.5f);
    /* more touches than the device takes are cut to two */
    s.touch_n = 9;
    vpad_state_encode(&s, b);
    CHECK(b[0x1C] == 2);
}

static void test_home_user(void) {
    char dir[] = "/tmp/vpad-home-XXXXXX", p[128];
    const char *names[] = {"zz", "1e24b13c", "0000000a", "abc", "00000000", "1234567g"};
    CHECK(mkdtemp(dir) != NULL);
    for (size_t i = 0; i < sizeof names / sizeof names[0]; i++) {
        snprintf(p, sizeof p, "%s/%s", dir, names[i]);
        CHECK(mkdir(p, 0755) == 0);
    }
    CHECK(vpad_home_user(dir) == 0xa); /* the lowest real id; 0 and non-hex names are not users */
    for (size_t i = 0; i < sizeof names / sizeof names[0]; i++) {
        snprintf(p, sizeof p, "%s/%s", dir, names[i]);
        rmdir(p);
    }
    CHECK(vpad_home_user(dir) == 0);
    rmdir(dir);
    CHECK(vpad_home_user("/nonexistent-vpad-dir") == 0);
}

static void test_open_resume_busy(void) {
    int h;
    fresh();
    /* malformed */
    CHECK(vopen("{\"slot\":1,\"token\":\"" TOK_A "\"}", 0) > 0 && HAS("\"err\":\"bad_request\""));
    CHECK(vopen("{\"slot\":5,\"token\":\"" TOK_A "\"}", 0) > 0 && HAS("bad_request"));
    CHECK(vopen("{\"slot\":2,\"token\":\"0123\"}", 0) > 0 && HAS("bad_request"));
    CHECK(vopen("{\"slot\":2,\"token\":\"0123456789abcdef0123456789abcdeg\"}", 0) > 0 && HAS("bad_request"));
    CHECK(vopen("{\"slot\":2}", 0) > 0 && HAS("bad_request"));
    CHECK(vopen("{\"slot\":2,\"token\":\"" TOK_A "\",\"user\":4294967296}", 0) > 0 && HAS("bad_request"));
    CHECK(f_adds == 0 && g_picks == 0);

    /* a named user: no pick */
    CHECK(vopen("{\"slot\":2,\"token\":\"" TOK_A "\",\"user\":4660}", 1000) > 0);
    CHECK(HAS("\"ok\":true,\"slot\":2,") && HAS("\"user\":4660,\"resumed\":false"));
    h = handle_from_answer();
    CHECK(h > 0 && h < MAXH && g_picks == 0 && f_adds == 1 && f_opens == 1);
    CHECK(f_pads[h].live && f_pads[h].cls == 3 && f_pads[h].subtype == 1 && f_pads[h].user == 4660);
    CHECK(f_pads[h].inserts == 1 && block_is_neutral(f_pads[h].block)); /* made at rest */
    CHECK(vpad_open_count() == 1);

    /* the same token again (any case) resumes: nothing is made again */
    CHECK(vopen("{\"slot\":2,\"token\":\"0123456789ABCDEF0123456789ABCDEF\"}", 2000) > 0);
    CHECK(HAS("\"ok\":true") && HAS("\"resumed\":true") && handle_from_answer() == h);
    CHECK(f_adds == 1 && g_picks == 0);
    /* another token: busy, the pad untouched */
    CHECK(vopen("{\"slot\":2,\"token\":\"" TOK_B "\"}", 2000) > 0 && HAS("\"err\":\"vpad_busy\""));
    CHECK(f_adds == 1 && f_pads[h].live);

    /* no user named: the foreground (pick) */
    CHECK(vopen("{\"slot\":3,\"token\":\"" TOK_B "\"}", 2000) > 0 && HAS("\"ok\":true") && HAS("\"user\":505721148"));
    CHECK(g_picks == 1 && f_adds == 2);
    /* nobody signed in */
    g_pick_uid = 0;
    CHECK(vopen("{\"slot\":4,\"token\":\"" TOK_B "\",\"user\":0}", 2000) > 0 && HAS("\"err\":\"vpad_no_user\""));
    CHECK(g_picks == 2 && f_adds == 2 && vpad_open_count() == 2);

    /* the device refuses */
    g_pick_uid = 7;
    f_open_err = -ENOENT;
    CHECK(vopen("{\"slot\":4,\"token\":\"" TOK_B "\"}", 2000) > 0 && HAS("\"err\":\"vpad_unsupported\",\"code\":-"));
    f_open_err = -EPERM;
    CHECK(vopen("{\"slot\":4,\"token\":\"" TOK_B "\"}", 2000) > 0 && HAS("\"err\":\"vpad_failed\",\"code\":-"));
    f_open_err = 0;
    f_add_err = -EINVAL;
    {
        int closes = f_closes;
        CHECK(vopen("{\"slot\":4,\"token\":\"" TOK_B "\"}", 2000) > 0 && HAS("\"err\":\"vpad_failed\""));
        CHECK(f_closes == closes + 1); /* the fd is not kept */
    }
    f_add_err = 0;
    CHECK(vpad_open_count() == 2);
    /* a third pad: all three players at once */
    CHECK(vopen("{\"slot\":4,\"token\":\"" TOK_B "\"}", 2000) > 0 && HAS("\"ok\":true") && vpad_open_count() == 3);
}

static void test_state_and_seq(void) {
    int h;
    fresh();
    CHECK(vopen("{\"slot\":2,\"token\":\"" TOK_A "\",\"user\":1}", 0) > 0 && HAS("\"ok\":true"));
    h = handle_from_answer();
    /* not open, not the owner */
    CHECK(vstate("{\"slot\":3,\"token\":\"" TOK_A "\",\"seq\":1}", 10) > 0 && HAS("\"err\":\"vpad_not_open\""));
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_B "\",\"seq\":1}", 10) > 0 && HAS("\"err\":\"vpad_not_owner\""));
    CHECK(f_pads[h].inserts == 1);
    /* malformed */
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\"}", 10) > 0 && HAS("bad_request"));                    /* no seq */
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":1,\"lx\":256}", 10) > 0 && HAS("bad_request")); /* > 255 */
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":1,\"buttons\":4294967296}", 10) > 0 && HAS("bad_request"));
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":1,\"touch\":[{\"x\":1,\"y\":1},{\"x\":1,\"y\":1},{\"x\":1,\"y\":1}]}",
                 10) > 0 && HAS("bad_request"));
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":1,\"touch\":[{\"x\":70000,\"y\":1}]}", 10) > 0 &&
          HAS("bad_request"));
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":1,\"touch\":[1]}", 10) > 0 && HAS("bad_request"));
    CHECK(f_pads[h].inserts == 1);

    /* a full state */
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":5,\"buttons\":16384,\"lx\":0,\"ly\":255,\"rx\":1,\"ry\":2,"
                 "\"l2\":3,\"r2\":4,\"touch\":[{\"id\":1,\"x\":100,\"y\":200}, {\"id\":2,\"x\":300,\"y\":400}],"
                 "\"gyro\":[0.5, -0.25, 1e-1],\"accel\":[0,-9.5,0]}",
                 20) > 0);
    CHECK(strcmp(g_out, "{\"ok\":true,\"applied\":true}") == 0);
    CHECK(f_pads[h].inserts == 2);
    {
        const uint8_t *b = f_pads[h].block;
        CHECK(rd32(b + 0x0C) == VPAD_BTN_CROSS && b[0x10] == 0 && b[0x11] == 255 && b[0x12] == 1 && b[0x13] == 2 &&
              b[0x14] == 3 && b[0x15] == 4 && b[0x1C] == 2 && b[0x20] == 1 && b[0x24] == 100 && b[0x26] == 200 &&
              b[0x28] == 2 && b[0x2C] == (300 & 0xFF) && b[0x2D] == (300 >> 8));
        CHECK(fl(b + 0x88) == 0.5f && fl(b + 0x8C) == -0.25f && fl(b + 0x90) == 0.1f && fl(b + 0x98) == -9.5f);
    }
    /* the same seq and an older one are ignored */
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":5}", 30) > 0 && strcmp(g_out, "{\"ok\":true,\"applied\":false}") == 0);
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":4}", 30) > 0 && HAS("\"applied\":false"));
    CHECK(f_pads[h].inserts == 2 && rd32(f_pads[h].block + 0x0C) == VPAD_BTN_CROSS);
    /* a newer one with only some fields: the rest is at rest (a state is the whole controller) */
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":6,\"buttons\":1,\"touch\":null}", 40) > 0 &&
          HAS("\"applied\":true"));
    CHECK(rd32(f_pads[h].block + 0x0C) == 1 && f_pads[h].block[0x10] == 128 && f_pads[h].block[0x1C] == 0 &&
          fl(f_pads[h].block + 0x94 + 4) == 1.0f);
    /* a resume starts the seq over */
    CHECK(vopen("{\"slot\":2,\"token\":\"" TOK_A "\"}", 50) > 0 && HAS("\"resumed\":true"));
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":1}", 60) > 0 && HAS("\"applied\":true"));
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":1}", 60) > 0 && HAS("\"applied\":false"));
    /* the device refuses an insert: the seq is not used up */
    f_insert_err = -EIO;
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":2}", 70) > 0 && HAS("\"err\":\"vpad_failed\",\"code\":-"));
    f_insert_err = 0;
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":2}", 70) > 0 && HAS("\"applied\":true"));
    /* a bad motion block is dropped (held still), the rest of the state still goes */
    {
        static const char *bad[] = {"\"gyro\":[1,2]", "\"gyro\":[1,2,3,4]", "\"gyro\":[1,\"x\",3]",
                                    "\"gyro\":[1e7,0,0]", "\"accel\":[0,-2e6,0]", "\"gyro\":[1,2,3],\"accel\":[1e300,0,0]"};
        char body[256];
        for (int i = 0; i < (int)(sizeof bad / sizeof bad[0]); i++) {
            snprintf(body, sizeof body, "{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":%d,\"buttons\":16384,\"lx\":7,%s}",
                     10 + i, bad[i]);
            CHECK(vstate(body, 80) > 0 && HAS("\"applied\":true"));
            CHECK(rd32(f_pads[h].block + 0x0C) == VPAD_BTN_CROSS && f_pads[h].block[0x10] == 7);
            CHECK(fl(f_pads[h].block + 0x88) == 0.0f && fl(f_pads[h].block + 0x94) == 0.0f &&
                  fl(f_pads[h].block + 0x98) == 1.0f);
        }
    }
}

static void test_batch(void) {
    char body[VPAD_REQ_MAX];
    int at;
    fresh();
    CHECK(vopen("{\"slot\":2,\"token\":\"" TOK_A "\",\"user\":1}", 0) > 0);
    CHECK(vopen("{\"slot\":3,\"token\":\"" TOK_B "\",\"user\":1}", 0) > 0);
    CHECK(vstates("{\"items\":[{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":1,\"buttons\":2},"
                  "{\"slot\":3,\"token\":\"" TOK_B "\",\"seq\":9},"
                  "{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":1},"
                  "{\"slot\":3,\"token\":\"" TOK_A "\",\"seq\":10},"
                  "{\"slot\":4,\"token\":\"" TOK_A "\",\"seq\":1}]}",
                  10) > 0);
    CHECK(strcmp(g_out, "{\"ok\":true,\"applied\":2,\"stale\":1,\"failed\":[{\"slot\":3,\"err\":\"vpad_not_owner\"},"
                        "{\"slot\":4,\"err\":\"vpad_not_open\"}]}") == 0);
    CHECK(vstates("{\"items\":[]}", 10) > 0 && strcmp(g_out, "{\"ok\":true,\"applied\":0,\"stale\":0,\"failed\":[]}") == 0);
    /* a bad motion block in one item drops only that block: both items apply */
    CHECK(vstates("{\"items\":[{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":50,\"gyro\":[1e9,0,0],\"buttons\":2},"
                  "{\"slot\":3,\"token\":\"" TOK_B "\",\"seq\":50,\"accel\":[0,1,0]}]}",
                  10) > 0);
    CHECK(strcmp(g_out, "{\"ok\":true,\"applied\":2,\"stale\":0,\"failed\":[]}") == 0);
    /* a malformed item refuses the whole batch, nothing applied */
    CHECK(vstates("{\"items\":[{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":51},{\"slot\":2}]}", 10) > 0 &&
          HAS("bad_request"));
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":51}", 10) > 0 && HAS("\"applied\":true"));
    CHECK(vstates("{\"items\":{}}", 10) > 0 && HAS("bad_request"));
    CHECK(vstates("{}", 10) > 0 && HAS("bad_request"));
    /* VPAD_MAX_BATCH fits, one more does not */
    at = snprintf(body, sizeof body, "{\"items\":[");
    for (int i = 0; i < VPAD_MAX_BATCH; i++)
        at += snprintf(body + at, sizeof body - (size_t)at, "%s{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":%d}",
                       i ? "," : "", 100 + i);
    snprintf(body + at, sizeof body - (size_t)at, "]}");
    CHECK(vstates(body, 20) > 0 && HAS("\"applied\":16,"));
    snprintf(body + at, sizeof body - (size_t)at, ",{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":999}]}");
    CHECK(vstates(body, 20) > 0 && HAS("bad_request"));
    /* every item failing still fits the answer */
    at = snprintf(body, sizeof body, "{\"items\":[");
    for (int i = 0; i < VPAD_MAX_BATCH; i++)
        at += snprintf(body + at, sizeof body - (size_t)at, "%s{\"slot\":3,\"token\":\"" TOK_A "\",\"seq\":1}", i ? "," : "");
    snprintf(body + at, sizeof body - (size_t)at, "]}");
    CHECK(vstates(body, 20) > 0 && HAS("\"applied\":0,\"stale\":0,\"failed\":[{") && HAS("}]}"));
}

static void test_watchdog_by_hand(void) {
    int h, h3;
    const long long t0 = 100000;
    fresh();
    CHECK(vopen("{\"slot\":2,\"token\":\"" TOK_A "\",\"user\":1}", t0) > 0);
    h = handle_from_answer();
    /* a pad made at rest is not rested again */
    CHECK(vpad_tick(t0 + 1000) == 0 && f_pads[h].inserts == 1);
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":1,\"buttons\":16384,\"lx\":0}", t0 + 1000) > 0);
    CHECK(f_pads[h].inserts == 2);
    /* 400 ms of silence is still fine, 401 rests it, once */
    CHECK(vpad_tick(t0 + 1400) == 0);
    CHECK(vpad_tick(t0 + 1401) == 1 && block_is_neutral(f_pads[h].block) && f_pads[h].inserts == 3);
    CHECK(vpad_tick(t0 + 1500) == 0 && vpad_tick(t0 + 5000) == 0 && f_pads[h].inserts == 3);
    CHECK(vpad_status_json(t0 + 5000, g_out, sizeof g_out) > 0 && HAS("\"neutral\":true,\"neutral_ms\":3599"));
    CHECK(vhb("{\"slot\":2,\"token\":\"" TOK_A "\"}", t0 + 5000) > 0 && strcmp(g_out, "{\"ok\":true,\"neutral\":true}") == 0);
    /* the next state takes over again */
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":2,\"buttons\":16384}", t0 + 6000) > 0 && HAS("\"applied\":true"));
    CHECK(rd32(f_pads[h].block + 0x0C) == VPAD_BTN_CROSS);
    CHECK(vpad_status_json(t0 + 6000, g_out, sizeof g_out) > 0 && HAS("\"neutral\":false,\"neutral_ms\":0"));
    /* a plain hb does not hold the state, a "live" one does */
    CHECK(vhb("{\"slot\":2,\"token\":\"" TOK_A "\"}", t0 + 6300) > 0 && HAS("\"neutral\":false"));
    CHECK(vpad_tick(t0 + 6401) == 1);
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":3,\"buttons\":16384}", t0 + 7000) > 0);
    CHECK(vhb("{\"slot\":2,\"token\":\"" TOK_A "\",\"live\":true}", t0 + 7300) > 0);
    CHECK(vpad_tick(t0 + 7401) == 0 && rd32(f_pads[h].block + 0x0C) == VPAD_BTN_CROSS);
    CHECK(vpad_tick(t0 + 7701) == 1);
    CHECK(vhb("{\"slot\":2,\"token\":\"" TOK_A "\",\"live\":7}", t0 + 7800) > 0 && HAS("bad_request"));
    CHECK(vhb("{\"slot\":2,\"token\":\"" TOK_B "\"}", t0 + 7800) > 0 && HAS("vpad_not_owner"));
    CHECK(vhb("{\"slot\":3,\"token\":\"" TOK_A "\"}", t0 + 7800) > 0 && HAS("vpad_not_open"));
    CHECK(vhb("{\"slot\":2,\"token\":\"" TOK_A "\"}", t0 + 7800) > 0 && HAS("\"ok\":true"));

    /* removal: 90 s after the last open, state or hb (a refused hb does not count) */
    CHECK(vopen("{\"slot\":3,\"token\":\"" TOK_B "\",\"user\":1}", t0 + 7300) > 0);
    h3 = handle_from_answer();
    CHECK(vpad_tick(t0 + 7300 + 90000) == 0);
    CHECK(vpad_tick(t0 + 7300 + 90001) == 100 + 0 && !f_pads[h3].live && f_pads[h3].deleted);
    CHECK(block_is_neutral(f_pads[h3].block));
    CHECK(f_pads[h].live); /* its last hb was at +7800 */
    CHECK(vpad_tick(t0 + 7800 + 90001) == 100 && !f_pads[h].live && block_is_neutral(f_pads[h].block));
    CHECK(vpad_open_count() == 0 && f_closes == 2);
    CHECK(vstate("{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":9}", t0 + 200000) > 0 && HAS("vpad_not_open"));
    /* the slot is free for anyone */
    CHECK(vopen("{\"slot\":2,\"token\":\"" TOK_B "\",\"user\":1}", t0 + 200000) > 0 && HAS("\"resumed\":false"));
}

static void test_close_output_status(void) {
    int h;
    fresh();
    CHECK(vopen("{\"slot\":4,\"token\":\"" TOK_A "\",\"user\":9}", 1000) > 0);
    h = handle_from_answer();
    /* output */
    CHECK(voutput("{\"slot\":4}", 1100) > 0 &&
          strcmp(g_out, "{\"ok\":true,\"slot\":4,\"motors\":[10,20],\"color\":[1,2,3]}") == 0);
    f_motors[0] = 255;
    f_color[2] = 200;
    CHECK(voutput("{\"slot\":4}", 1100) > 0 && HAS("\"motors\":[255,20],\"color\":[1,2,200]"));
    CHECK(voutput("{\"slot\":3}", 1100) > 0 && HAS("vpad_not_open"));
    CHECK(voutput("{\"slot\":9}", 1100) > 0 && HAS("bad_request"));
    f_output_err = -EIO;
    CHECK(voutput("{\"slot\":4}", 1100) > 0 && HAS("\"err\":\"vpad_failed\""));
    f_output_err = 0;

    /* status */
    CHECK(vstate("{\"slot\":4,\"token\":\"" TOK_A "\",\"seq\":1}", 1200) > 0);
    CHECK(vpad_status_json(1500, g_out, sizeof g_out) > 0);
    CHECK(HAS("{\"ok\":true,\"slots\":[{\"slot\":2,\"player\":2,\"open\":false},{\"slot\":3,\"player\":3,\"open\":false},"
              "{\"slot\":4,\"player\":4,\"open\":true,\"owner\":\""));
    CHECK(HAS("\"user\":9,\"since_ms\":500,\"last_state_ms\":300,\"neutral\":false,\"neutral_ms\":0}]}"));
    CHECK(!HAS(TOK_A) && !HAS("0123456789")); /* the token never shows */
    {
        char tiny[16];
        CHECK(vpad_status_json(1500, tiny, sizeof tiny) == -1);
        CHECK(vpad_state_json(B("{\"slot\":4,\"token\":\"" TOK_A "\",\"seq\":1}"), 1500, tiny, 8) == -1);
    }

    /* close: only the owner; a free slot is fine */
    CHECK(vclose("{\"slot\":4,\"token\":\"" TOK_B "\"}", 1600) > 0 && HAS("vpad_not_owner") && f_pads[h].live);
    CHECK(vclose("{\"slot\":4}", 1600) > 0 && HAS("bad_request"));
    CHECK(vclose("{\"slot\":4,\"token\":\"" TOK_A "\"}", 1600) > 0 && strcmp(g_out, "{\"ok\":true}") == 0);
    CHECK(!f_pads[h].live && f_pads[h].deleted && block_is_neutral(f_pads[h].block) && f_closes == 1);
    CHECK(vclose("{\"slot\":4,\"token\":\"" TOK_A "\"}", 1700) > 0 && strcmp(g_out, "{\"ok\":true}") == 0);
    CHECK(f_deletes == 1);
    CHECK(vpad_status_json(1700, g_out, sizeof g_out) > 0 && HAS("{\"slot\":4,\"player\":4,\"open\":false}]}"));
}

static void test_stop(void) {
    fresh();
    CHECK(vopen("{\"slot\":2,\"token\":\"" TOK_A "\",\"user\":1}", 0) > 0);
    CHECK(vopen("{\"slot\":3,\"token\":\"" TOK_B "\",\"user\":1}", 0) > 0);
    vpad_stop();
    CHECK(vpad_open_count() == 0 && f_deletes == 2 && f_closes == 2);
    CHECK(vopen("{\"slot\":2,\"token\":\"" TOK_A "\",\"user\":1}", 10) > 0 && HAS("\"err\":\"vpad_stopping\""));
    vpad_stop(); /* again: nothing to do */
    CHECK(f_deletes == 2);
    vpad_set_hid(&FAKE); /* clears the stop */
    CHECK(vopen("{\"slot\":2,\"token\":\"" TOK_A "\",\"user\":1}", 10) > 0 && HAS("\"ok\":true"));
    vpad_set_hid(&FAKE); /* replacing the device layer removes the pad first */
    CHECK(vpad_open_count() == 0 && f_deletes == 3);
}

/* The watchdog thread rests a silent pad on its own (real clock). */
static void test_watchdog_thread(void) {
    int h;
    char body[160];
    fresh();
    vpad_test_no_watchdog(0);
    CHECK(vopen("{\"slot\":2,\"token\":\"" TOK_A "\",\"user\":1}", vpad_now_ms()) > 0);
    h = handle_from_answer();
    snprintf(body, sizeof body, "{\"slot\":2,\"token\":\"" TOK_A "\",\"seq\":1,\"buttons\":%u}", VPAD_BTN_CROSS);
    CHECK(vstate(body, vpad_now_ms()) > 0 && HAS("\"applied\":true"));
    usleep(200 * 1000);
    CHECK(rd32(f_pads[h].block + 0x0C) == VPAD_BTN_CROSS); /* still held */
    for (int i = 0; i < 40 && !block_is_neutral(f_pads[h].block); i++) usleep(50 * 1000);
    CHECK(block_is_neutral(f_pads[h].block));
    CHECK(vclose("{\"slot\":2,\"token\":\"" TOK_A "\"}", vpad_now_ms()) > 0 && HAS("\"ok\":true"));
    usleep(150 * 1000); /* the thread ends with no pad open */
    vpad_test_no_watchdog(1);
}

/* Three players' states at once from three threads, with status and output polls in between. */
typedef struct {
    int slot;
    const char *tok;
} hammer_arg_t;
static void *hammer(void *p) {
    const hammer_arg_t *a = p;
    char body[192], out[VPAD_OUT_MAX];
    for (int i = 1; i <= 2000; i++) {
        snprintf(body, sizeof body, "{\"slot\":%d,\"token\":\"%s\",\"seq\":%d,\"buttons\":%d}", a->slot, a->tok, i, i & 0xFF);
        if (vpad_state_json(B(body), 0, out, sizeof out) <= 0 || !strstr(out, "\"applied\":true")) return (void *)1;
        if (i % 100 == 0) {
            (void)vpad_status_json(0, out, sizeof out);
            snprintf(body, sizeof body, "{\"slot\":%d}", a->slot);
            (void)vpad_output_json(B(body), 0, out, sizeof out);
        }
    }
    return NULL;
}
static void test_threads(void) {
    static const char *toks[3] = {TOK_A, TOK_B, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"};
    pthread_t t[3];
    hammer_arg_t a[3];
    void *r;
    char body[96];
    fresh();
    for (int i = 0; i < 3; i++) {
        a[i].slot = 2 + i;
        a[i].tok = toks[i];
        snprintf(body, sizeof body, "{\"slot\":%d,\"token\":\"%s\",\"user\":1}", a[i].slot, a[i].tok);
        CHECK(vopen(body, 0) > 0 && HAS("\"ok\":true"));
    }
    for (int i = 0; i < 3; i++) CHECK(pthread_create(&t[i], NULL, hammer, &a[i]) == 0);
    for (int i = 0; i < 3; i++) {
        CHECK(pthread_join(t[i], &r) == 0 && r == NULL);
    }
    for (int h = 1; h <= 3; h++) CHECK(f_pads[h].inserts == 2001 && f_pads[h].block[0x0C] == (2000 & 0xFF));
    vpad_stop();
}

int main(void) {
    vpad_test_no_watchdog(1);
    test_encode();
    test_home_user();
    test_open_resume_busy();
    test_state_and_seq();
    test_batch();
    test_watchdog_by_hand();
    test_close_output_status();
    test_stop();
    test_watchdog_thread();
    test_threads();
    if (failures) {
        fprintf(stderr, "vpad selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("vpad selftest: ok\n");
    return 0;
}
