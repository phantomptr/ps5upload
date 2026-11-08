/* ps5u-mbusprobe: a throwaway payload that tries libSceMbus's device binding on its own, before the
 * helper's companion relies on it (payload/tools/mbus-probe/README.md says how to send it and what
 * to look for). Never linked into the helper.
 *
 * What it tries comes from ProsperoSunshine (https://github.com/Darkatek7/ProsperoSunshine, PR #33,
 * src/platform/ps5/profile_seat.c, GPL-3.0-only, measured there on FW 10.20): the handle the
 * /dev/hid add ioctl returns is the bus's device id; sceMbusBindDeviceWithUserId(device, user)
 * seats the pad in that user, and the console then never asks "Who's using this controller?";
 * sceMbusInit(0, 0) must come first; a bind right after the add may answer 0x809b0005 (no such
 * device yet), so it is tried every millisecond, 30 times; sceMbusGetDeviceInfo_ fills 0x100 bytes
 * with the owner at +0xC8. The pad requests are those of src/vpad.c (from the same project's
 * pad.c). Written anew here; this file is GPL-3.0-only like its sources (see LICENSES/README.md).
 *
 * Modes (the config file, else the first argument; nothing given: "init"):
 *   init   load libSceMbus, resolve the four functions, sceMbusInit(0,0) on a thread of its own
 *          while the main thread writes a heartbeat file. No pad. The safest first run.
 *   bind   all of init, then one virtual pad made the way the helper makes it, bound to `user`,
 *          its owner read back, held `hold_ms`, read again, and removed.
 *
 * Every return code goes to LOG_PATH. It exits when done (or after INIT_WAIT_MS stuck in Init). */
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#include <ps5/kernel.h>

#define DIR_PATH "/data/ps5upload"
#define LOG_PATH DIR_PATH "/mbus-probe.log"
#define CONF_PATH DIR_PATH "/mbus-probe.conf"
#define BEAT_PATH DIR_PATH "/mbus-probe.heartbeat"
#define NAME "ps5u-mbusprobe" /* never "ps5upload…": the helper's own-instance sweep kills that */

/* How long sceMbusInit may take before the probe says it hung and leaves. Its connect loop
 * gives up after 50 tries 100 ms apart (payload/vinput/README.md), so 5 s plus margin. */
#define INIT_WAIT_MS 15000
#define BEAT_MS 100

/* libSceMbus by NID (checked with the SDK's prospero-nid). */
#define NID_INIT "wRPXMGtkOq0"   /* sceMbusInit */
#define NID_BIND "Cpo6sQmnZgU"   /* sceMbusBindDeviceWithUserId */
#define NID_INFO "RsKVKhSWt28"   /* sceMbusGetDeviceInfo_ */
#define NID_DISC "WNxCIjZdcTo"   /* sceMbusDisconnectDevice (resolved, never called) */
#define MBUS_NO_DEVICE 0x809b0005u
#define BIND_TRIES 30
#define INFO_SIZE 0x100
#define INFO_OWNER 0xC8

/* /dev/hid (src/vpad.c). */
#define HID_ADD 0xC018482AUL
#define HID_INSERT 0x8018482CUL
#define HID_DELETE 0x80104850UL
#define BLOCK 0xA8

int sceKernelLoadStartModule(const char *path, size_t args, const void *argp, uint32_t flags, const void *option,
                             int *result);
int sceUserServiceInitialize(void *param);
int sceUserServiceGetForegroundUser(int *user);
int sceUserServiceGetLoginUserIdList(int *ids);
int sceUserServiceGetUserName(int user, char *name, size_t size);

typedef int (*init_fn)(uint64_t, uint64_t);
typedef int (*bind_fn)(uint64_t device, int32_t user);
typedef int (*info_fn)(uint64_t device, void *info, uint32_t size);

static long long now_us(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000000 + ts.tv_nsec / 1000;
}

static void plog(const char *fmt, ...) {
    char line[512];
    va_list ap;
    int k = snprintf(line, sizeof line, "%lld.%03lld ", now_us() / 1000000, (now_us() / 1000) % 1000);
    FILE *f;
    va_start(ap, fmt);
    vsnprintf(line + k, sizeof line - (size_t)k, fmt, ap);
    va_end(ap);
    f = fopen(LOG_PATH, "a");
    if (!f) return;
    fprintf(f, "%s\n", line);
    fclose(f);
}

/* ── the config ─────────────────────────────────────────────────────────────────────────────── */

typedef struct {
    char mode[16];
    uint32_t user;
    int hold_ms;
} conf_t;

/* key=value lines: mode=init|bind, user=0x1234abcd (hex or decimal), hold_ms=5000. */
static void read_conf(conf_t *c, int argc, char **argv) {
    char buf[512], *line, *save = NULL;
    ssize_t n;
    int fd;
    snprintf(c->mode, sizeof c->mode, "init");
    c->user = 0;
    c->hold_ms = 5000;
    if (argc > 1 && argv[1] && argv[1][0]) snprintf(c->mode, sizeof c->mode, "%s", argv[1]);
    if (argc > 2 && argv[2]) c->user = (uint32_t)strtoul(argv[2], NULL, 0);
    fd = open(CONF_PATH, O_RDONLY);
    if (fd < 0) return;
    n = read(fd, buf, sizeof buf - 1);
    close(fd);
    if (n <= 0) return;
    buf[n] = '\0';
    for (line = strtok_r(buf, "\r\n", &save); line; line = strtok_r(NULL, "\r\n", &save)) {
        char *eq = strchr(line, '=');
        if (!eq || line[0] == '#') continue;
        *eq = '\0';
        if (strcmp(line, "mode") == 0)
            snprintf(c->mode, sizeof c->mode, "%s", eq + 1);
        else if (strcmp(line, "user") == 0)
            c->user = (uint32_t)strtoul(eq + 1, NULL, 0);
        else if (strcmp(line, "hold_ms") == 0)
            c->hold_ms = atoi(eq + 1);
    }
    if (c->hold_ms < 0) c->hold_ms = 0;
    if (c->hold_ms > 60000) c->hold_ms = 60000;
}

/* ── the heartbeat ──────────────────────────────────────────────────────────────────────────── */

static const char *volatile g_stage = "start";

static void beat(void) {
    char s[96];
    int k = snprintf(s, sizeof s, "%lld %s\n", now_us() / 1000, (const char *)g_stage);
    int fd = open(BEAT_PATH, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return;
    if (write(fd, s, (size_t)k) != k) { /* best effort */
    }
    close(fd);
}

/* ── libSceMbus ─────────────────────────────────────────────────────────────────────────────── */

static void *resolve(uint32_t module, const char *nid, const char *name) {
    intptr_t p = kernel_dynlib_resolve(getpid(), module, nid);
    plog("resolve %s (%s): 0x%llx", name, nid, (unsigned long long)p);
    /* A failed lookup is 0 or a small number; calling it would end the process. */
    if (p <= 0x10000 || (uint64_t)p >= 0x0000800000000000ull) return NULL;
    return (void *)p;
}

typedef struct {
    init_fn init;
    volatile int done;
    int rc;
} init_job_t;

static void *init_main(void *arg) {
    init_job_t *j = arg;
    long long t0 = now_us();
    j->rc = j->init(0, 0);
    plog("sceMbusInit(0,0) = 0x%08x after %lld us", (unsigned)j->rc, now_us() - t0);
    __atomic_store_n(&j->done, 1, __ATOMIC_RELEASE);
    return NULL;
}

/* ── the users ──────────────────────────────────────────────────────────────────────────────── */

static void log_users(const char *when) {
    int ids[16], fg = -1, rc_fg, rc_list;
    for (int i = 0; i < 16; i++) ids[i] = -1;
    rc_fg = sceUserServiceGetForegroundUser(&fg);
    rc_list = sceUserServiceGetLoginUserIdList(ids);
    plog("users %s: foreground 0x%08x (rc 0x%08x), signed-in list rc 0x%08x", when, (unsigned)fg, (unsigned)rc_fg,
         (unsigned)rc_list);
    for (int i = 0; i < 16; i++) {
        char name[64] = {0};
        int rc;
        if (ids[i] == -1) continue;
        rc = sceUserServiceGetUserName(ids[i], name, sizeof name - 1);
        plog("  signed in: 0x%08x \"%s\" (name rc 0x%08x)%s", (unsigned)ids[i], name, (unsigned)rc,
             ids[i] == fg ? " [foreground]" : "");
    }
}

/* ── the pad ────────────────────────────────────────────────────────────────────────────────── */

typedef struct {
    uint32_t op, pad;
    uint64_t desc, handle_out;
} add_req_t;
typedef struct {
    uint32_t handle, pad;
    uint64_t state;
    uint8_t zero, pad2[7];
} insert_req_t;
typedef struct {
    uint8_t op, pad[7];
    uint32_t handle, pad2;
} delete_req_t;

static void owner_of(info_fn info, uint64_t dev, const char *when) {
    uint8_t buf[INFO_SIZE];
    uint32_t owner = 0;
    int rc;
    memset(buf, 0, sizeof buf);
    rc = info(dev, buf, sizeof buf);
    memcpy(&owner, buf + INFO_OWNER, sizeof owner);
    plog("sceMbusGetDeviceInfo_(%llu) %s = 0x%08x, owner @0xC8 = 0x%08x", (unsigned long long)dev, when, (unsigned)rc,
         (unsigned)owner);
    for (int off = 0; off < INFO_SIZE; off += 32) {
        char hex[3 * 32 + 1];
        for (int i = 0; i < 32; i++) snprintf(hex + 3 * i, 4, "%02x ", buf[off + i]);
        plog("  info +0x%02x: %s", off, hex);
    }
}

static int bind_pad(bind_fn bind, info_fn info, uint32_t user, int hold_ms) {
    uint8_t desc[BLOCK], state[BLOCK];
    static const float orient[4] = {0, 0, 0, 1}, accel[3] = {0, 1, 0};
    add_req_t a;
    insert_req_t ins;
    delete_req_t d;
    int32_t handle = -1;
    long long t_add, t;
    int fd, rc, tries = 0, brc = (int)MBUS_NO_DEVICE;

    g_stage = "hid open";
    fd = open("/dev/hid", O_RDONLY);
    plog("open /dev/hid = %d (errno %d)", fd, fd < 0 ? errno : 0);
    if (fd < 0) return -1;
    memset(desc, 0, sizeof desc);
    desc[0x00] = 3; /* a standard controller: class 3, subtype 1 */
    memcpy(desc + 0x1C, &user, 4);
    desc[0x20] = 1;
    memset(&a, 0, sizeof a);
    a.op = 3;
    a.desc = (uint64_t)(uintptr_t)desc;
    a.handle_out = (uint64_t)(uintptr_t)&handle;
    g_stage = "pad add";
    rc = ioctl(fd, HID_ADD, &a);
    t_add = now_us();
    /* Bound before anything else is logged: the console decides about a new pad ~10 ms after it. */
    if (rc == 0 && handle >= 0) {
        g_stage = "bind";
        for (tries = 0; tries < BIND_TRIES && (unsigned)brc == MBUS_NO_DEVICE; tries++) {
            if (tries) usleep(1000);
            brc = bind((uint64_t)(uint32_t)handle, (int32_t)user);
        }
    }
    t = now_us();
    plog("pad add ioctl = %d (errno %d), handle %d (0x%x)", rc, rc ? errno : 0, handle, (unsigned)handle);
    if (rc != 0 || handle < 0) {
        close(fd);
        return -1;
    }
    plog("sceMbusBindDeviceWithUserId(%d, 0x%08x) = 0x%08x after %d tries, %lld us after the add", handle,
         (unsigned)user, (unsigned)brc, tries, t - t_add);

    /* At rest, as the helper's pads start (vpad.c rest_locked). */
    memset(state, 0, sizeof state);
    state[0x10] = state[0x11] = state[0x12] = state[0x13] = 128;
    state[0x17] = 1;
    memcpy(state + 0x78, orient, sizeof orient);
    memcpy(state + 0x94, accel, sizeof accel);
    memset(&ins, 0, sizeof ins);
    ins.handle = (uint32_t)handle;
    ins.state = (uint64_t)(uintptr_t)state;
    plog("insert neutral = %d", ioctl(fd, HID_INSERT, &ins));

    g_stage = "owner";
    owner_of(info, (uint64_t)(uint32_t)handle, "right after");
    log_users("with the pad");
    plog("holding the pad %d ms: look at the TV (a 'Who's using this controller?' question means the bind did not take)",
         hold_ms);
    g_stage = "hold";
    for (int left = hold_ms; left > 0; left -= BEAT_MS) {
        beat();
        usleep(BEAT_MS * 1000);
    }
    owner_of(info, (uint64_t)(uint32_t)handle, "after the hold");

    g_stage = "pad delete";
    plog("insert neutral = %d", ioctl(fd, HID_INSERT, &ins));
    memset(&d, 0, sizeof d);
    d.op = 5;
    d.handle = (uint32_t)handle;
    rc = ioctl(fd, HID_DELETE, &d);
    plog("pad delete ioctl = %d (errno %d)", rc, rc ? errno : 0);
    close(fd);
    return brc == 0 ? 0 : -1;
}

int main(int argc, char **argv) {
    conf_t c;
    init_job_t job;
    pthread_t th;
    uint32_t module = 0;
    int h, start = 0, rc;
    bind_fn bind;
    info_fn info;
    long long t0;

#ifdef SYS_thr_set_name
    (void)syscall(SYS_thr_set_name, -1, NAME);
#endif
    mkdir(DIR_PATH, 0777);
    read_conf(&c, argc, argv);
    plog("==== %s start: pid %d, mode %s, user 0x%08x, hold %d ms", NAME, (int)getpid(), c.mode, (unsigned)c.user,
         c.hold_ms);
    beat();

    rc = sceUserServiceInitialize(NULL);
    plog("sceUserServiceInitialize = 0x%08x", (unsigned)rc);
    log_users("before");

    g_stage = "load";
    if (kernel_dynlib_handle(getpid(), "libSceMbus.sprx", &module) == 0) {
        plog("libSceMbus already loaded: handle 0x%x", module);
    } else {
        h = sceKernelLoadStartModule("/system/common/lib/libSceMbus.sprx", 0, NULL, 0, NULL, &start);
        plog("sceKernelLoadStartModule(libSceMbus.sprx) = 0x%x (start %d)", (unsigned)h, start);
        if (h < 0) {
            plog("==== done: libSceMbus did not load");
            return 1;
        }
        module = (uint32_t)h;
    }
    memset(&job, 0, sizeof job);
    job.init = (init_fn)resolve(module, NID_INIT, "sceMbusInit");
    bind = (bind_fn)resolve(module, NID_BIND, "sceMbusBindDeviceWithUserId");
    info = (info_fn)resolve(module, NID_INFO, "sceMbusGetDeviceInfo_");
    (void)resolve(module, NID_DISC, "sceMbusDisconnectDevice");
    if (!job.init || !bind || !info) {
        plog("==== done: a function did not resolve; nothing called");
        return 1;
    }

    /* Init on a thread of its own; the main thread beats, so a hang shows as a stale stage. */
    g_stage = "sceMbusInit";
    t0 = now_us();
    if (pthread_create(&th, NULL, init_main, &job) != 0) {
        plog("==== done: no thread for Init");
        return 1;
    }
    while (!__atomic_load_n(&job.done, __ATOMIC_ACQUIRE)) {
        beat();
        if (now_us() - t0 > (long long)INIT_WAIT_MS * 1000) {
            plog("==== sceMbusInit HUNG: no answer after %d ms. Leaving it (the process may stay, stuck)", INIT_WAIT_MS);
            g_stage = "sceMbusInit HUNG";
            beat();
            _exit(3);
        }
        usleep(BEAT_MS * 1000);
    }
    pthread_join(th, NULL);
    if (job.rc != 0) {
        plog("==== done: sceMbusInit failed (0x%08x)", (unsigned)job.rc);
        g_stage = "done (Init failed)";
        beat();
        return 1;
    }

    if (strcmp(c.mode, "bind") != 0) {
        plog("==== done: init only, all resolved and sceMbusInit = 0. Next: mode=bind with a user id");
        g_stage = "done (init)";
        beat();
        return 0;
    }
    if (!c.user || c.user == 0xFFFFFFFFu) {
        plog("==== done: mode=bind needs user=<a signed-in user id from the list above>");
        g_stage = "done (no user)";
        beat();
        return 1;
    }
    rc = bind_pad(bind, info, c.user, c.hold_ms);
    log_users("after");
    plog("==== done: bind %s", rc == 0 ? "OK" : "FAILED");
    g_stage = rc == 0 ? "done (bound)" : "done (bind failed)";
    beat();
    return rc == 0 ? 0 : 2;
}
