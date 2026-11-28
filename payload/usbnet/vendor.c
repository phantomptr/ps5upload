/* SPDX-License-Identifier: GPL-3.0-or-later
 * Adapted from tom-ps5-usbnet (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later),
 * src/vendor.c, for PS5Upload's USB adapter daemon (payload/usbnet/README.md). */
/* See vendor.h. The chip's own framing: one bulk transfer holds many frames, each after
 * a descriptor and on a boundary of the chip's (16 bytes and 16 on the RTL8157 and RTL8159; 8 and 4
 * going out, 24 and 8 coming in, on the RTL8156B), and USB sees one frame.
 *
 * Sending: frames are copied into the transfer being filled, which starts at once if
 * fewer than `queue` transfers are in flight, so a lone frame never waits. Under load
 * frames pile up to `tx_bytes` until a completion starts the next transfer.
 *
 * Receiving: every pending transfer is VND_RX_BYTES long, the size Linux uses, and
 * how much the chip puts in one is set at bring-up. That never exceeds the transfer, so
 * an aggregate always ends with its transfer.
 *
 * Failures: each kind is counted on its own and the totals are said when the run ends.
 * A bulk OUT that fails for any reason has the endpoint reset before the next one goes,
 * since a host controller halts an endpoint on a transaction error as it does on a
 * stall. If they keep failing the adapter counts as lost. */
#include "adapter.h"
#include "usbn_log.h"
#include "vendor.h"

#include <sys/ioctl.h>

#include <errno.h>
#include <poll.h>
#include <pthread.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define TX_TRANSFERS (VND_MAX_QUEUE + 1)
#define EP_TX0 VND_MAX_RX
#define EP_COUNT (EP_TX0 + TX_TRANSFERS)
#define MAX_FRAMES (VND_RX_BYTES / 80)  /* a minimum frame with FCS and descriptor */
#define MIN_FRAME 60
#define TX_TIMEOUT_MS 1000
#define RESERVE_TIMEOUT_MS 500L
#define POLL_MS 20
#define THREAD_STACK (1024 * 1024)
#define MAX_RX_ERRORS 1000
#define MAX_LOGGED 4                    /* of each kind; after that, every time the count doubles */
#define USB_STATUS_STALLED 22
#define TX_DEAD_FAILURES 8              /* in a row, and for this long, with none getting through */
#define TX_DEAD_MS 5000LL

enum { ERR_OUT, ERR_IN, ERR_START, ERR_COMPLETE, ERR_SCAN, ERR_KINDS };
static const char *const g_error_names[ERR_KINDS] = {"bulk OUT status", "bulk IN status", "USB_FS_START errno", "USB_FS_COMPLETE errno",
    "bulk IN aggregate cut short"};

static size_t g_tx_align = 16, g_tx_header = 16;
static enum rtl_chip g_chip = RTL_8159;

struct tx_transfer {
    int busy, frames;
    size_t used;
};

static uint8_t g_rx_data[VND_MAX_RX][VND_RX_BYTES];
static uint8_t g_tx_data[TX_TRANSFERS][VND_TX_BYTES] __attribute__((aligned(16)));
static void *g_buffer[EP_COUNT][1];
static uint32_t g_length[EP_COUNT][1];
static struct usb_fs_endpoint g_endpoints[EP_COUNT];
static struct rtl_frame_view g_views[MAX_FRAMES];
static struct frame g_frames[MAX_FRAMES];
static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;

static struct {
    int fd;
    struct vnd_hooks hooks;
    struct vnd_settings set;
    pthread_t thread;
    int thread_running, opened, stop, gone;
    int tx_limit;
    unsigned char rx_busy[VND_MAX_RX];
    int rx_pending, done[VND_MAX_RX], done_count;
    struct tx_transfer tx[TX_TRANSFERS];
    int fill, outstanding;
    int tx_reset;                       /* the next bulk OUT resets the endpoint first */
    unsigned tx_failed_run;
    long long tx_failed_since;
    unsigned rx_errors;
    unsigned long errors[ERR_KINDS], tx_done, rx_done;
} g;

void vnd_lock(void)
{
    pthread_mutex_lock(&g_lock);
}

void vnd_unlock(void)
{
    pthread_mutex_unlock(&g_lock);
}

static long long now_ns(void)
{
    struct timespec ts;

    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

static int dead_errno(int code)
{
    return code == ENXIO || code == ENODEV || code == EIO || code == EBADF || code == ENOENT;
}

/* One kind filling the log must not hide another, and a failure that goes on for an hour
 * must still show when it began and how fast it grew. */
static void note_error(int kind, int value)
{
    unsigned long n = ++g.errors[kind];

    if (n <= MAX_LOGGED) say("%s: %d", g_error_names[kind], value);
    else if (!(n & (n - 1))) say("%s: %d, %lu so far", g_error_names[kind], value, n);
}

static void lose(const char *why, int value)
{
    if (g.gone) return;
    g.gone = 1;
    say("adapter lost: %s %d", why, value);
}

/* ---- transfers: the lock is held ----------------------------------------- */

static int start_endpoint(int index)
{
    struct usb_fs_start start;
    int code;

    memset(&start, 0, sizeof start);
    start.ep_index = (uint8_t)index;
    if (ioctl(g.fd, USB_FS_START, &start) == 0) return 0;
    code = errno;
    note_error(ERR_START, code);
    if (dead_errno(code)) lose("USB_FS_START errno", code);
    return -1;
}

static void start_rx(int i)
{
    struct usb_fs_endpoint *ep = &g_endpoints[i];

    g_length[i][0] = VND_RX_BYTES;
    ep->nFrames = 1;
    ep->aFrames = 0;
    ep->status = 0;
    if (start_endpoint(i) != 0) return;
    g.rx_busy[i] = 1;
    g.rx_pending++;
}

/* Starts the transfer being filled, if the queue has room. */
static void launch(void)
{
    struct tx_transfer *t;
    struct usb_fs_endpoint *ep;

    if (g.fill < 0 || g.outstanding >= g.set.queue) return;
    if (g.tx_reset && g.outstanding) return;    /* the endpoint is reset with nothing else on it */
    t = &g.tx[g.fill];
    if (!t->frames) return;
    ep = &g_endpoints[EP_TX0 + g.fill];
    g_length[EP_TX0 + g.fill][0] = (uint32_t)t->used;
    ep->nFrames = 1;
    ep->aFrames = 0;
    ep->status = 0;
    if (g.tx_reset) ep->flags |= USB_FS_FLAG_CLEAR_STALL;
    else ep->flags &= (uint8_t)~USB_FS_FLAG_CLEAR_STALL;
    if (start_endpoint(EP_TX0 + g.fill) != 0) {
        t->frames = 0;
        t->used = 0;
        return;
    }
    g.tx_reset = 0;
    t->busy = 1;
    g.outstanding++;
    g.fill = -1;
}

static void finish_tx(int i, unsigned status)
{
    struct tx_transfer *t = &g.tx[i];

    if (status) {
        note_error(ERR_OUT, (int)status);
        g.tx_reset = 1;
        if (!g.tx_failed_run++) g.tx_failed_since = now_ns();
        else if (g.tx_failed_run >= TX_DEAD_FAILURES && now_ns() - g.tx_failed_since >= TX_DEAD_MS * 1000000LL)
            lose("bulk OUT failures in a row:", (int)g.tx_failed_run);
    } else {
        if (g.tx_failed_run) say("bulk OUT works again after %u failures", g.tx_failed_run);
        g.tx_failed_run = 0;
        g.tx_done++;
    }
    t->busy = 0;
    t->frames = 0;
    t->used = 0;
    g.outstanding--;
    launch();
}

static void finish_rx(int i, unsigned status)
{
    g.rx_busy[i] = 0;
    g.rx_pending--;
    if (status) {
        g.rx_errors++;
        note_error(ERR_IN, (int)status);
        if (status == USB_STATUS_STALLED) g_endpoints[i].flags |= USB_FS_FLAG_CLEAR_STALL;
        if (g.rx_errors >= MAX_RX_ERRORS) lose("bulk IN failures:", (int)g.rx_errors);
        return;
    }
    g.rx_done++;
    g_endpoints[i].flags &= (uint8_t)~USB_FS_FLAG_CLEAR_STALL;
    if (g.done_count < VND_MAX_RX) g.done[g.done_count++] = i;
}

/* Takes every finished transfer. Received ones stay in g.done for the receive thread. */
static int reap(void)
{
    int taken = 0;

    for (;;) {
        struct usb_fs_complete done;
        unsigned status;
        int i;

        memset(&done, 0, sizeof done);
        if (ioctl(g.fd, USB_FS_COMPLETE, &done) != 0) {
            int code = errno;

            if (code != EBUSY) {
                note_error(ERR_COMPLETE, code);
                if (dead_errno(code)) lose("USB_FS_COMPLETE errno", code);
            }
            return taken;
        }
        taken++;
        i = done.ep_index;
        status = g_endpoints[i].status;
        if (i >= EP_TX0) finish_tx(i - EP_TX0, status);
        else finish_rx(i, status);
    }
}

static int wait_for_completion(int ms)
{
    struct pollfd pfd;

    pfd.fd = g.fd;
    pfd.events = POLLIN | POLLRDNORM;
    pfd.revents = 0;
    return poll(&pfd, 1, ms);
}

/* ---- receive thread ------------------------------------------------------ */

static void deliver_transfer(int i, size_t len)
{
    size_t count = 0, k;
    int result = rtl_rx_scan(g_chip, g_rx_data[i], len, g_views, MAX_FRAMES, &count);

    if (result) {                       /* the frames before the bad descriptor still go up */
        pthread_mutex_lock(&g_lock);
        note_error(ERR_SCAN, result);
        pthread_mutex_unlock(&g_lock);
    }

    for (k = 0; k < count; k++) {
        g_frames[k].data = g_views[k].data;
        g_frames[k].len = g_views[k].len;
    }
    if (count) g.hooks.frames(g_frames, (int)count);
}

static void deliver_frames(void)
{
    int indexes[VND_MAX_RX], ready, i;

    pthread_mutex_lock(&g_lock);
    ready = g.done_count;
    memcpy(indexes, g.done, sizeof indexes[0] * (size_t)ready);
    g.done_count = 0;
    pthread_mutex_unlock(&g_lock);
    for (i = 0; i < ready; i++) {
        int k = indexes[i];
        size_t len = g_endpoints[k].aFrames ? g_length[k][0] : 0;

        if (len) deliver_transfer(k, len);
        pthread_mutex_lock(&g_lock);
        if (!g.gone && g.rx_pending < g.set.rx_depth) start_rx(k);
        pthread_mutex_unlock(&g_lock);
    }
}

/* Fills up after a larger rx_depth, or after a transfer failed. */
static void refill_rx(void)
{
    int i;

    for (i = 0; i < VND_MAX_RX && g.rx_pending < g.set.rx_depth && !g.gone; i++)
        if (!g.rx_busy[i]) start_rx(i);
}

static void *receive_thread(void *unused)
{
    int ready = 0;

    (void)unused;
    for (;;) {
        int taken;

        pthread_mutex_lock(&g_lock);
        if (g.stop || g.gone) {
            pthread_mutex_unlock(&g_lock);
            return NULL;
        }
        taken = reap();
        if (!g.done_count) refill_rx();
        pthread_mutex_unlock(&g_lock);
        deliver_frames();
        if (taken) continue;
        if (ready > 0) usleep(50);      /* a poll that stays ready must not spin */
        ready = wait_for_completion(POLL_MS);
    }
}

/* ---- public -------------------------------------------------------------- */

static int open_endpoint(int index, unsigned endpoint, uint32_t bufsize, uint32_t *got_bufsize, uint32_t *got_packet)
{
    struct usb_fs_open op;

    memset(&op, 0, sizeof op);
    op.ep_index = (uint8_t)index;
    op.ep_no = (uint8_t)endpoint;
    op.max_bufsize = bufsize;
    op.max_frames = 1;
    if (ioctl(g.fd, USB_FS_OPEN, &op) != 0) return -1;
    *got_bufsize = op.max_bufsize;
    *got_packet = op.max_packet_length;
    return 0;
}

int vnd_open(int fd, enum rtl_chip chip, unsigned bulk_in, unsigned bulk_out, const struct vnd_hooks *hooks)
{
    struct usb_fs_init init;
    struct rtl_format format;
    uint32_t got = 0, packet = 0;
    int i;

    if (rtl_get_format(chip, &format) != 0) return -1;
    g_chip = chip;
    g_tx_align = format.tx_align;
    g_tx_header = format.tx_header;
    memset(&g, 0, sizeof g);
    g.fd = fd;
    g.hooks = *hooks;
    g.fill = -1;
    g.tx_limit = VND_TX_BYTES;
    g.set.rx_depth = 8;
    g.set.queue = 1;
    g.set.tx_bytes = 16 * 1024;
    memset(g_endpoints, 0, sizeof g_endpoints);
    for (i = 0; i < EP_COUNT; i++) {
        g_buffer[i][0] = i < EP_TX0 ? (void *)g_rx_data[i] : (void *)g_tx_data[i - EP_TX0];
        g_endpoints[i].ppBuffer = g_buffer[i];
        g_endpoints[i].pLength = g_length[i];
        g_endpoints[i].flags = i < EP_TX0 ? USB_FS_FLAG_SINGLE_SHORT_OK : 0;
        g_endpoints[i].timeout = i < EP_TX0 ? 0 : TX_TIMEOUT_MS;
    }
    memset(&init, 0, sizeof init);
    init.pEndpoints = g_endpoints;
    init.ep_index_max = EP_COUNT;
    if (ioctl(fd, USB_FS_INIT, &init) != 0) {
        say("USB_FS_INIT: errno %d", errno);
        return -1;
    }
    g.opened = 1;
    for (i = 0; i < EP_TX0; i++)
        if (open_endpoint(i, bulk_in, VND_RX_BYTES, &got, &packet) != 0 || got < VND_RX_BYTES) {
            say("USB_FS_OPEN bulk IN %d: errno %d, max_bufsize=%u", i, errno, got);
            return -1;
        }
    for (i = EP_TX0; i < EP_COUNT; i++) {
        if (open_endpoint(i, bulk_out, VND_TX_BYTES, &got, &packet) != 0) {
            say("USB_FS_OPEN bulk OUT: errno %d", errno);
            return -1;
        }
        if (got < (uint32_t)g.tx_limit) g.tx_limit = (int)got;
    }
    checkpoint();
    return g.tx_limit >= VND_MIN_TX ? 0 : -1;
}

int vnd_start(void)
{
    pthread_attr_t attr;
    int i, result;

    pthread_mutex_lock(&g_lock);
    for (i = 0; i < g.set.rx_depth; i++) start_rx(i);
    pthread_mutex_unlock(&g_lock);
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, THREAD_STACK);
    result = pthread_create(&g.thread, &attr, receive_thread, NULL);
    pthread_attr_destroy(&attr);
    if (result) {
        say("pthread_create: %d", result);
        return -1;
    }
    g.thread_running = 1;
    return 0;
}

void vnd_close(void)
{
    struct usb_fs_uninit uninit;

    pthread_mutex_lock(&g_lock);
    g.stop = 1;
    pthread_mutex_unlock(&g_lock);
    if (g.thread_running) pthread_join(g.thread, NULL);
    g.thread_running = 0;
    if (g.opened)
        say("usb: out %lu ok, %lu failed; in %lu ok, %lu failed, %lu cut short; start errors %lu, completion errors %lu", g.tx_done,
            g.errors[ERR_OUT], g.rx_done, g.errors[ERR_IN], g.errors[ERR_SCAN], g.errors[ERR_START], g.errors[ERR_COMPLETE]);
    if (g.opened && !g.gone) {
        memset(&uninit, 0, sizeof uninit);
        if (ioctl(g.fd, USB_FS_UNINIT, &uninit) != 0) say("USB_FS_UNINIT: errno %d", errno);
    }
    if (g.gone) adapter_gone = 1;
    g.opened = 0;
}

static int clamp(int value, int low, int high)
{
    return value < low ? low : value > high ? high : value;
}

void vnd_configure(const struct vnd_settings *wanted)
{
    pthread_mutex_lock(&g_lock);
    g.set.rx_depth = clamp(wanted->rx_depth, 1, VND_MAX_RX);
    g.set.queue = clamp(wanted->queue, 1, VND_MAX_QUEUE);
    g.set.tx_bytes = clamp(wanted->tx_bytes, VND_MIN_TX, g.tx_limit);
    if (g.fill >= 0 && g.tx[g.fill].frames) launch();
    pthread_mutex_unlock(&g_lock);
}

int vnd_gone(void)
{
    int gone;

    pthread_mutex_lock(&g_lock);
    gone = g.gone;
    pthread_mutex_unlock(&g_lock);
    return gone;
}

void vnd_counts(struct vnd_counts *out)
{
    int i;

    pthread_mutex_lock(&g_lock);
    out->tx_done = g.tx_done;
    out->rx_done = g.rx_done;
    for (i = 0; i < ERR_KINDS; i++) out->errors[i] = g.errors[i];
    pthread_mutex_unlock(&g_lock);
}

static size_t next_header(const struct tx_transfer *t)
{
    return (t->used + g_tx_align - 1) & ~(g_tx_align - 1);
}

uint8_t *vnd_tx_reserve(void)
{
    long long deadline = now_ns() + RESERVE_TIMEOUT_MS * 1000000LL;

    pthread_mutex_lock(&g_lock);
    for (;;) {
        int i;

        if (g.gone || g.stop) break;
        if (g.fill < 0)
            for (i = 0; i < TX_TRANSFERS; i++)
                if (!g.tx[i].busy) {
                    g.fill = i;
                    break;
                }
        if (g.fill >= 0) {
            size_t header = next_header(&g.tx[g.fill]);

            if (header + g_tx_header + MAX_FRAME <= (size_t)g.set.tx_bytes)
                return g_tx_data[g.fill] + header + g_tx_header;  /* the commit unlocks */
        }
        if (now_ns() >= deadline) break;
        if (!reap()) {
            pthread_mutex_unlock(&g_lock);
            wait_for_completion(1);
            pthread_mutex_lock(&g_lock);
        }
    }
    pthread_mutex_unlock(&g_lock);
    return NULL;
}

void vnd_tx_commit(size_t len)
{
    struct tx_transfer *t = &g.tx[g.fill];
    size_t header = next_header(t);
    uint8_t *slot = g_tx_data[g.fill] + header + g_tx_header;

    if (len < MIN_FRAME) {
        memset(slot + len, 0, MIN_FRAME - len);
        len = MIN_FRAME;
    }
    rtl_tx_header(g_chip, g_tx_data[g.fill] + header, len);
    t->used = header + g_tx_header + len;
    t->frames++;
    launch();
    pthread_mutex_unlock(&g_lock);
}

