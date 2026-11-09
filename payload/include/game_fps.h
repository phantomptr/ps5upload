#ifndef PS5UPLOAD_GAME_FPS_H
#define PS5UPLOAD_GAME_FPS_H

#include <stddef.h>
#include <stdint.h>

/* The running game's frame count, from the display controller (/dev/dce), for the app's sampled
 * frame rate (#774, method `hw.fps`).
 *
 * The display driver counts the pictures it flips to the screen. One read request returns that
 * count; the frame rate is the difference of two reads divided by the time between them, so this
 * file only reads the count and the helper's monotonic clock. The engine keeps the previous read
 * and works out frames per second (ps5upload-core/src/game_fps.rs). Nothing here keeps a thread,
 * a timer or state beyond one cached descriptor: a read happens only when someone asks.
 *
 * The request, its selector and the count's offset were found by porhe911 (Common FPS for PS5) and
 * are used the same way by Marice/ps5-exporter and khalifa007/SimpleFPS, which measured 60 in a 60
 * FPS game on a 13.60 PS5 Pro. This file is our own code; only those three numbers are theirs.
 *
 *   ioctl 0x80308217 on /dev/dce, 48-byte argument {selector, mask, output pointer, 3 reserved},
 *   selector 0x10000000A, mask 0x8000000000. The kernel writes a block to the output pointer
 *   (128 bytes on 13.60, more than some tools reserve); the count is the u64 at offset 8.
 *   Without a game it fails with ENOENT.
 *
 * Safety: the output block is a 16 KiB mapping followed by a PROT_NONE guard page, so a kernel
 * that writes more than expected fails the copy (EFAULT) instead of overwriting our memory. The
 * block is filled with a marker byte first, which tells how much the kernel wrote; fewer than 16
 * bytes is a failure, not a count. Only this one request ever reaches the device. */

#define GAME_FPS_DEV          "/dev/dce"
#define GAME_FPS_IOCTL        0x80308217UL
/* The same request sign-extended, as some firmware wants it (tried once after EINVAL). */
#define GAME_FPS_IOCTL_SIGNED ((unsigned long)0xFFFFFFFF80308217ULL)
#define GAME_FPS_SELECTOR     0x10000000AULL
#define GAME_FPS_MASK         0x8000000000ULL
#define GAME_FPS_COUNT_OFFSET 8
#define GAME_FPS_BLOCK_BYTES  0x4000
#define GAME_FPS_MARKER       0xA5
/* A reply longer than this is a JSON line with room to spare. */
#define GAME_FPS_JSON_MAX     192

enum {
    GAME_FPS_OK = 0,        /* `flips` is the count */
    GAME_FPS_NO_GAME = 1,   /* the driver has no game picture to count (ENOENT) */
    GAME_FPS_NO_DEVICE = 2, /* /dev/dce did not open */
    GAME_FPS_FAILED = 3,    /* the request failed otherwise, or wrote too little */
};

/* The 48-byte argument of GAME_FPS_IOCTL (IOCPARM_LEN 0x30). */
typedef struct {
    uint64_t selector;
    uint64_t mask;
    uint64_t output; /* user address of the block the kernel fills */
    uint64_t reserved[3];
} __attribute__((aligned(64))) game_fps_arg_t;

_Static_assert(offsetof(game_fps_arg_t, reserved) + 3 * sizeof(uint64_t) == 0x30,
               "the request carries 48 bytes");

/* How the device is reached: the helper passes the real calls, the host test a fake.
 * open_dev: an fd, or -1 with errno set. request: one ioctl, 0 or -1 with errno set. */
typedef struct {
    int (*open_dev)(void *ctx);
    int (*request)(void *ctx, int fd, unsigned long req, game_fps_arg_t *arg);
    void (*close_dev)(void *ctx, int fd);
    uint64_t (*now_us)(void *ctx); /* monotonic */
    void *ctx;
} game_fps_ops_t;

typedef struct {
    int state;        /* GAME_FPS_* */
    uint64_t flips;   /* the display's frame count (GAME_FPS_OK only) */
    uint64_t mono_us; /* the helper's monotonic clock at the read */
    int err;          /* errno of a failed open or request, else 0 */
    uint32_t bytes;   /* how many bytes the kernel wrote (GAME_FPS_OK only) */
} game_fps_sample_t;

/* One read through `ops` into `block` (GAME_FPS_BLOCK_BYTES, the guard page after it is the
 * caller's). `*fd` is the cached descriptor (-1 = none): opened here when needed, closed again
 * after any failure so the next read starts over. Always fills *out; returns out->state. */
int game_fps_read(const game_fps_ops_t *ops, int *fd, unsigned char *block, game_fps_sample_t *out);

/* The reply body, one JSON object (no newline):
 *   {"ok":true,"state":"ok","flips":N,"mono_us":T,"bytes":B}
 *   {"ok":true,"state":"no_game","mono_us":T,"errno":E}      (also no_device, failed)
 * The keys are the fields of ps5upload-core's FlipRead; keep them in step. Returns the length,
 * or -1 when `cap` is too small. */
int game_fps_json(const game_fps_sample_t *s, char *out, size_t cap);

/* The state's name in that body. */
const char *game_fps_state_name(int state);

/* The helper's own read: the real device, one mapping kept for the helper's life, one read at a
 * time under a mutex. Returns the JSON length as game_fps_json does. */
int game_fps_read_json(char *out, size_t cap);

#endif
