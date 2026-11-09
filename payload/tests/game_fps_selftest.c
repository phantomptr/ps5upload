/* Host test for game_fps.c with a fake /dev/dce: the count comes from offset 8 of what the kernel
 * wrote, the request carries the selector, mask and block address, EINVAL retries once with the
 * sign-extended request, ENOENT is "no game", a short write is a failure (not a count), a failure
 * closes the descriptor so the next read opens it again, and the JSON keys are the ones the engine
 * parses (ps5upload-core/src/game_fps.rs reads these same literals). */
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include "../src/game_fps.c"

static int fails;
#define CHECK(cond)                                                    \
    do {                                                               \
        if (!(cond)) {                                                 \
            printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond);    \
            fails++;                                                   \
        }                                                              \
    } while (0)

typedef struct {
    uint64_t now;
    int opens, closes, requests, signed_requests, other_requests;
    int open_err;      /* errno for a failing open; 0 = opens */
    int req_err;       /* errno for a failing request; 0 = succeeds */
    int einval_first;  /* the plain request answers EINVAL, the signed one works */
    uint32_t write_len;
    uint64_t count;
    int bad_arg;
} fake_t;

static int fake_open(void *ctx) {
    fake_t *f = ctx;
    f->opens++;
    if (f->open_err) {
        errno = f->open_err;
        return -1;
    }
    return 7;
}

static int fake_request(void *ctx, int fd, unsigned long req, game_fps_arg_t *arg) {
    fake_t *f = ctx;
    f->requests++;
    if (fd != 7) return -1;
    if (req == GAME_FPS_IOCTL_SIGNED) {
        f->signed_requests++;
    } else if (req != GAME_FPS_IOCTL) {
        f->other_requests++;
        errno = ENOTTY;
        return -1;
    }
    if (arg->selector != GAME_FPS_SELECTOR || arg->mask != GAME_FPS_MASK || arg->output == 0 ||
        arg->reserved[0] || arg->reserved[1] || arg->reserved[2] || ((uintptr_t)arg & 63u) != 0)
        f->bad_arg++;
    if (f->einval_first && req == GAME_FPS_IOCTL) {
        errno = EINVAL;
        return -1;
    }
    if (f->req_err) {
        errno = f->req_err;
        return -1;
    }
    unsigned char *out = (unsigned char *)(uintptr_t)arg->output;
    memset(out, 0, f->write_len);
    if (f->write_len >= 16) memcpy(out + 8, &f->count, sizeof f->count);
    return 0;
}

static void fake_close(void *ctx, int fd) {
    fake_t *f = ctx;
    (void)fd;
    f->closes++;
}

static uint64_t fake_now(void *ctx) {
    fake_t *f = ctx;
    return f->now;
}

static unsigned char block[GAME_FPS_BLOCK_BYTES];

int main(void) {
    fake_t f;
    game_fps_ops_t ops = { fake_open, fake_request, fake_close, fake_now, &f };
    game_fps_sample_t s;
    char json[GAME_FPS_JSON_MAX];
    int fd = -1;

    /* a game: the count at offset 8 of a 128-byte block, the descriptor kept */
    memset(&f, 0, sizeof f);
    f.now = 5000000;
    f.write_len = 128;
    f.count = 123456;
    CHECK(game_fps_read(&ops, &fd, block, &s) == GAME_FPS_OK);
    CHECK(s.flips == 123456 && s.bytes == 128 && s.mono_us == 5000000 && s.err == 0);
    CHECK(fd == 7 && f.opens == 1 && f.closes == 0 && f.bad_arg == 0);
    CHECK(game_fps_json(&s, json, sizeof json) > 0);
    CHECK(strcmp(json, "{\"ok\":true,\"state\":\"ok\",\"flips\":123456,\"mono_us\":5000000,\"bytes\":128}") == 0);
    f.count = 123516;
    f.now = 6000000;
    CHECK(game_fps_read(&ops, &fd, block, &s) == GAME_FPS_OK && s.flips == 123516);
    CHECK(f.opens == 1); /* the cached descriptor is reused */

    /* no game: ENOENT, the descriptor dropped, then opened again */
    f.req_err = ENOENT;
    f.now = 7000000;
    CHECK(game_fps_read(&ops, &fd, block, &s) == GAME_FPS_NO_GAME);
    CHECK(fd == -1 && f.closes == 1 && s.err == ENOENT && s.flips == 0);
    CHECK(game_fps_json(&s, json, sizeof json) > 0);
    CHECK(strcmp(json, "{\"ok\":true,\"state\":\"no_game\",\"mono_us\":7000000,\"errno\":2}") == 0);
    f.req_err = 0;
    CHECK(game_fps_read(&ops, &fd, block, &s) == GAME_FPS_OK && f.opens == 2);

    /* EINVAL: one retry with the sign-extended request */
    memset(&f, 0, sizeof f);
    fd = -1;
    f.write_len = 128;
    f.count = 9;
    f.einval_first = 1;
    CHECK(game_fps_read(&ops, &fd, block, &s) == GAME_FPS_OK && s.flips == 9);
    CHECK(f.requests == 2 && f.signed_requests == 1 && f.other_requests == 0);

    /* any other failure is "failed", with its errno */
    memset(&f, 0, sizeof f);
    fd = -1;
    f.req_err = EFAULT;
    CHECK(game_fps_read(&ops, &fd, block, &s) == GAME_FPS_FAILED && s.err == EFAULT && fd == -1);
    CHECK(game_fps_json(&s, json, sizeof json) > 0 && strstr(json, "\"state\":\"failed\"") != NULL);

    /* the kernel wrote too little to hold a count: not a count */
    memset(&f, 0, sizeof f);
    fd = -1;
    f.write_len = 12;
    f.count = 77;
    CHECK(game_fps_read(&ops, &fd, block, &s) == GAME_FPS_FAILED && s.flips == 0 && fd == -1);

    /* no device */
    memset(&f, 0, sizeof f);
    fd = -1;
    f.open_err = ENOENT;
    f.now = 42;
    CHECK(game_fps_read(&ops, &fd, block, &s) == GAME_FPS_NO_DEVICE && fd == -1 && f.requests == 0);
    CHECK(game_fps_json(&s, json, sizeof json) > 0);
    CHECK(strcmp(json, "{\"ok\":true,\"state\":\"no_device\",\"mono_us\":42,\"errno\":2}") == 0);

    /* a buffer too small for the reply is refused, never cut */
    CHECK(game_fps_json(&s, json, 10) == -1);

    if (fails) {
        printf("game_fps selftest: %d failure(s)\n", fails);
        return 1;
    }
    printf("game_fps selftest: ok\n");
    return 0;
}
