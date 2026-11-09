/* The running game's frame count from /dev/dce (#774). See game_fps.h for the request, where its
 * numbers come from, and the safety rules. */
#include "game_fps.h"

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

const char *game_fps_state_name(int state) {
    switch (state) {
    case GAME_FPS_OK: return "ok";
    case GAME_FPS_NO_GAME: return "no_game";
    case GAME_FPS_NO_DEVICE: return "no_device";
    default: return "failed";
    }
}

/* How many leading bytes of the block are not the marker any more: what the kernel wrote, give or
 * take trailing bytes that happen to equal the marker (which only makes it look shorter). */
static uint32_t written_len(const unsigned char *block) {
    size_t n = GAME_FPS_BLOCK_BYTES;
    while (n > 0 && block[n - 1] == GAME_FPS_MARKER) n--;
    return (uint32_t)n;
}

static void drop_fd(const game_fps_ops_t *ops, int *fd) {
    if (*fd >= 0) ops->close_dev(ops->ctx, *fd);
    *fd = -1;
}

int game_fps_read(const game_fps_ops_t *ops, int *fd, unsigned char *block, game_fps_sample_t *out) {
    memset(out, 0, sizeof *out);
    if (*fd < 0) {
        *fd = ops->open_dev(ops->ctx);
        if (*fd < 0) {
            out->err = errno;
            out->state = GAME_FPS_NO_DEVICE;
            out->mono_us = ops->now_us(ops->ctx);
            *fd = -1;
            return out->state;
        }
    }
    game_fps_arg_t arg;
    memset(&arg, 0, sizeof arg);
    arg.selector = GAME_FPS_SELECTOR;
    arg.mask = GAME_FPS_MASK;
    arg.output = (uint64_t)(uintptr_t)block;
    memset(block, GAME_FPS_MARKER, GAME_FPS_BLOCK_BYTES);
    int rc = ops->request(ops->ctx, *fd, GAME_FPS_IOCTL, &arg);
    if (rc != 0 && errno == EINVAL) {
        memset(block, GAME_FPS_MARKER, GAME_FPS_BLOCK_BYTES);
        rc = ops->request(ops->ctx, *fd, GAME_FPS_IOCTL_SIGNED, &arg);
    }
    out->mono_us = ops->now_us(ops->ctx);
    if (rc != 0) {
        out->err = errno;
        out->state = out->err == ENOENT ? GAME_FPS_NO_GAME : GAME_FPS_FAILED;
        drop_fd(ops, fd);
        return out->state;
    }
    uint32_t n = written_len(block);
    if (n < GAME_FPS_COUNT_OFFSET + sizeof(uint64_t)) {
        out->state = GAME_FPS_FAILED;
        drop_fd(ops, fd);
        return out->state;
    }
    memcpy(&out->flips, block + GAME_FPS_COUNT_OFFSET, sizeof out->flips);
    out->bytes = n;
    out->state = GAME_FPS_OK;
    return out->state;
}

int game_fps_json(const game_fps_sample_t *s, char *out, size_t cap) {
    int n;
    if (s->state == GAME_FPS_OK) {
        n = snprintf(out, cap, "{\"ok\":true,\"state\":\"ok\",\"flips\":%llu,\"mono_us\":%llu,\"bytes\":%u}",
                     (unsigned long long)s->flips, (unsigned long long)s->mono_us, (unsigned)s->bytes);
    } else {
        n = snprintf(out, cap, "{\"ok\":true,\"state\":\"%s\",\"mono_us\":%llu,\"errno\":%d}",
                     game_fps_state_name(s->state), (unsigned long long)s->mono_us, s->err);
    }
    return (n < 0 || (size_t)n >= cap) ? -1 : n;
}

/* ── The helper's real device ─────────────────────────────────────────── */

static int real_open(void *ctx) {
    (void)ctx;
    return open(GAME_FPS_DEV, O_RDWR);
}

static int real_request(void *ctx, int fd, unsigned long req, game_fps_arg_t *arg) {
    (void)ctx;
    return ioctl(fd, req, arg) < 0 ? -1 : 0;
}

static void real_close(void *ctx, int fd) {
    (void)ctx;
    close(fd);
}

static uint64_t real_now_us(void *ctx) {
    (void)ctx;
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000ull + (uint64_t)ts.tv_nsec / 1000ull;
}

static pthread_mutex_t g_fps_lock = PTHREAD_MUTEX_INITIALIZER;
static unsigned char *g_fps_block; /* GAME_FPS_BLOCK_BYTES, then a PROT_NONE page */
static int g_fps_fd = -1;

/* The block and its guard page, made once. Caller holds g_fps_lock. */
static unsigned char *block_get(void) {
    if (g_fps_block) return g_fps_block;
    unsigned char *m = mmap(NULL, GAME_FPS_BLOCK_BYTES * 2, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANON, -1, 0);
    if (m == MAP_FAILED) return NULL;
    if (mprotect(m + GAME_FPS_BLOCK_BYTES, GAME_FPS_BLOCK_BYTES, PROT_NONE) != 0) {
        munmap(m, GAME_FPS_BLOCK_BYTES * 2);
        return NULL;
    }
    g_fps_block = m;
    return m;
}

int game_fps_read_json(char *out, size_t cap) {
    static const game_fps_ops_t ops = {
        .open_dev = real_open,
        .request = real_request,
        .close_dev = real_close,
        .now_us = real_now_us,
        .ctx = NULL,
    };
    game_fps_sample_t s;
    pthread_mutex_lock(&g_fps_lock);
    unsigned char *block = block_get();
    if (block) {
        game_fps_read(&ops, &g_fps_fd, block, &s);
    } else {
        memset(&s, 0, sizeof s);
        s.state = GAME_FPS_FAILED;
        s.err = ENOMEM;
        s.mono_us = real_now_us(NULL);
    }
    pthread_mutex_unlock(&g_fps_lock);
    return game_fps_json(&s, out, cap);
}
