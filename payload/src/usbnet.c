/* USB Ethernet adapter on the PS5 (EXPERIMENTAL): the helper's side. See usbnet.h for what it
 * does and usbnet_proto.h for the daemon it runs. One watcher thread owns the daemon: it scans the
 * bus, launches, polls, configures and stops it. usbnet_call and the status readers only read the
 * watcher's snapshot (or change the setting) under g_mu, so a management call never waits on USB,
 * the loader or the daemon. Nothing here makes a Sony call except the one notification when the
 * adapter is ready (notif_send_serialised, from the watcher thread). */
#include "usbnet.h"

#include "../smb/smbd_json.h"
#include "notif.h"
#include "power_phase.h"
#include "usbnet_proto.h"
#include "webui_server.h"

#include <arpa/inet.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

#if defined(__FreeBSD__) || defined(__PROSPERO__)
#include <dev/usb/usb.h>
#include <dev/usb/usb_ioctl.h>
#define USBNET_HAVE_UGEN 1
#endif

/* The daemon ELF (stripped), embedded by the Makefile (build/usbnet_elf.c). */
extern unsigned char usbnet_elf[];
extern unsigned int usbnet_elf_len;

#define LOADER_PORT 9021
#define LOADER_RETRY_MS 4000 /* elfldr closes :9021 for about a second while the PC app swaps its build */
#define REQ_TIMEOUT_MS 1500
#define MAX_NODES 48
#define MAX_SEEN 8
#define BACKOFF_BASE_S 5

typedef struct {
    char name[24];
    ino_t ino;
    uint16_t vid, pid;
    uint8_t cls;
} node_t;

static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static struct {
    /* the setting */
    int enabled;
    char sip[16], smask[16], sgw[16];
    int config_dirty;
    /* the outside world */
    int paused, quit, booted;
    /* the bus */
    int present;
    enum usbnet_chip_id chip;
    uint16_t vid, pid;
    char seen[MAX_SEEN][10];
    int n_seen;
    /* the daemon */
    int pid_daemon, alive, answered, restarts, latched;
    long long launched_ms, last_ok_ms, next_try_ms;
    char error[160];
    /* its last status */
    char state[16], why[160], d_chip[12], usb_id[12], ip[16], mask[16], gw[16], source[12];
    unsigned max_mbps, link_mbps, link_changes;
    unsigned long long rx_bytes, tx_bytes, rx_frames, tx_frames, errors, uptime_s;
    int minion_sent;
} g;
static pthread_t g_thread;
static char g_key[USBNET_KEY_HEX + 1];
static char g_elf_id[17];

static long long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static void nap_ms(int ms) {
    struct timespec ts = {ms / 1000, (long)(ms % 1000) * 1000000L};
    while (nanosleep(&ts, &ts) != 0 && errno == EINTR) {
    }
}

/* ---- the setting ---- */

static void settings_load(void) {
    char text[512];
    FILE *f = fopen(USBNET_SETTINGS_PATH, "r");
    size_t n = 0;
    int enabled;
    char ip[16], mask[16], gw[16];
    if (f) {
        n = fread(text, 1, sizeof text - 1, f);
        fclose(f);
    }
    text[n] = '\0';
    if (usbnet_parse_settings(text, &enabled, ip, mask, gw) != 0)
        fprintf(stderr, "[usbnet] %s has a line it does not understand: DHCP\n", USBNET_SETTINGS_PATH);
    pthread_mutex_lock(&g_mu);
    g.enabled = enabled;
    memcpy(g.sip, ip, sizeof ip);
    memcpy(g.smask, mask, sizeof mask);
    memcpy(g.sgw, gw, sizeof gw);
    pthread_mutex_unlock(&g_mu);
}

/* Caller holds g_mu. Written atomically (tmp + rename, same directory). */
static int settings_save_locked(void) {
    char text[160], tmp[] = USBNET_SETTINGS_PATH ".tmp";
    int k = 0, fd;
    mkdir("/data/ps5upload", 0777);
    mkdir(USBNET_DIR, 0700);
    if (!g.enabled) k += snprintf(text + k, sizeof text - (size_t)k, "off\n");
    if (g.sip[0]) k += snprintf(text + k, sizeof text - (size_t)k, "ip %s %s %s\n", g.sip, g.smask, g.sgw);
    fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) return -1;
    if (write(fd, text, (size_t)k) != k || fsync(fd) != 0) {
        close(fd);
        unlink(tmp);
        return -1;
    }
    close(fd);
    if (rename(tmp, USBNET_SETTINGS_PATH) != 0) {
        unlink(tmp);
        return -1;
    }
    return 0;
}

/* ---- the bus ---- */

/* One look at /dev: the ugen nodes, read only when new (a node's inode changes when it is made
 * again, at a replug). The device descriptor is the only thing asked; the node is opened read-only
 * and closed at once. Returns whether the supported adapter set changed (a replug). */
static node_t g_nodes[MAX_NODES];
static int g_n_nodes;

static int scan_bus(void) {
#ifdef USBNET_HAVE_UGEN
    node_t now[MAX_NODES];
    int n = 0, i, j, present = 0, changed;
    enum usbnet_chip_id chip = USBNET_CHIP_NONE;
    uint16_t vid = 0, pid = 0;
    ino_t adapter_ino = 0;
    static ino_t last_adapter_ino;
    char seen[MAX_SEEN][10];
    int n_seen = 0;
    DIR *d = opendir("/dev");
    struct dirent *e;
    if (!d) return 0;
    while ((e = readdir(d)) != NULL && n < MAX_NODES) {
        int bus, addr;
        char path[64];
        struct stat st;
        if (usbnet_parse_ugen(e->d_name, &bus, &addr) != 0) continue;
        snprintf(path, sizeof path, "/dev/%s", e->d_name);
        if (stat(path, &st) != 0) continue;
        memset(&now[n], 0, sizeof now[n]);
        snprintf(now[n].name, sizeof now[n].name, "%s", e->d_name);
        now[n].ino = st.st_ino;
        for (j = 0; j < g_n_nodes; j++)
            if (!strcmp(g_nodes[j].name, now[n].name) && g_nodes[j].ino == st.st_ino) break;
        if (j < g_n_nodes) {
            now[n] = g_nodes[j];
        } else {
            struct usb_device_descriptor dd;
            int fd = open(path, O_RDONLY | O_NONBLOCK);
            if (fd < 0) continue;
            memset(&dd, 0, sizeof dd);
            if (ioctl(fd, USB_GET_DEVICE_DESC, &dd) != 0) {
                close(fd);
                continue;
            }
            close(fd);
            now[n].vid = UGETW(dd.idVendor);
            now[n].pid = UGETW(dd.idProduct);
            now[n].cls = dd.bDeviceClass;
        }
        n++;
    }
    closedir(d);
    for (i = 0; i < n; i++) {
        enum usbnet_chip_id c = usbnet_supported(now[i].vid, now[i].pid);
        if (c != USBNET_CHIP_NONE && !present) {
            present = 1;
            chip = c;
            vid = now[i].vid;
            pid = now[i].pid;
            adapter_ino = now[i].ino;
        } else if (c == USBNET_CHIP_NONE && now[i].cls != 0x09 && n_seen < MAX_SEEN) { /* hubs left out */
            char id[10];
            snprintf(id, sizeof id, "%04x:%04x", now[i].vid, now[i].pid);
            for (j = 0; j < n_seen; j++)
                if (!strcmp(seen[j], id)) break;
            if (j == n_seen) memcpy(seen[n_seen++], id, sizeof id);
        }
    }
    memcpy(g_nodes, now, sizeof now[0] * (size_t)n);
    g_n_nodes = n;
    changed = adapter_ino != last_adapter_ino;
    last_adapter_ino = adapter_ino;
    pthread_mutex_lock(&g_mu);
    if (present != g.present) fprintf(stderr, "[usbnet] adapter %s (%04x:%04x)\n", present ? "plugged in" : "gone", present ? vid : g.vid, present ? pid : g.pid);
    g.present = present;
    g.chip = chip;
    g.vid = vid;
    g.pid = pid;
    memcpy(g.seen, seen, sizeof seen);
    g.n_seen = n_seen;
    pthread_mutex_unlock(&g_mu);
    return changed && present;
#else
    return 0;
#endif
}

/* ---- the daemon: sockets ---- */

/* A loopback connection to `port`: the fd, -2 when refused (nothing listens), -1 otherwise. */
static int dial(int port, int timeout_ms) {
    struct sockaddr_in a;
    struct pollfd p;
    int fd = socket(AF_INET, SOCK_STREAM, 0), fl, rc, soerr = 0;
    socklen_t sl = sizeof soerr;
    if (fd < 0) return -1;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons((uint16_t)port);
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    fl = fcntl(fd, F_GETFL, 0);
    fcntl(fd, F_SETFL, fl | O_NONBLOCK);
    rc = connect(fd, (struct sockaddr *)&a, sizeof a);
    if (rc != 0 && errno != EINPROGRESS) {
        int e = errno;
        close(fd);
        return e == ECONNREFUSED ? -2 : -1;
    }
    if (rc != 0) {
        p.fd = fd;
        p.events = POLLOUT;
        p.revents = 0;
        if (poll(&p, 1, timeout_ms) <= 0 || getsockopt(fd, SOL_SOCKET, SO_ERROR, &soerr, &sl) != 0 || soerr) {
            close(fd);
            return soerr == ECONNREFUSED ? -2 : -1;
        }
    }
    fcntl(fd, F_SETFL, fl);
    return fd;
}

static int send_all(int fd, const char *b, size_t n) {
    while (n) {
        ssize_t w = send(fd, b, n, MSG_NOSIGNAL);
        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) return -1;
        b += w;
        n -= (size_t)w;
    }
    return 0;
}

/* One exchange: 0 with the reply in `out`, -2 nothing listens, -1 failed or timed out. */
static int exchange(const char *fields, char *out, size_t cap) {
    char req[USBNET_REQ_MAX];
    size_t got = 0;
    long long until = now_ms() + REQ_TIMEOUT_MS;
    int k = snprintf(req, sizeof req, "{\"key\":\"%s\",%s}\n", g_key, fields), fd;
    if (k <= 0 || (size_t)k >= sizeof req) return -1;
    fd = dial(USBNET_PORT, 1000);
    if (fd < 0) {
        memset(req, 0, sizeof req);
        return fd;
    }
    if (send_all(fd, req, (size_t)k) != 0) {
        memset(req, 0, sizeof req);
        close(fd);
        return -1;
    }
    memset(req, 0, sizeof req);
    while (got + 1 < cap) {
        struct pollfd p = {fd, POLLIN, 0};
        long long left = until - now_ms();
        ssize_t n;
        if (left <= 0 || poll(&p, 1, (int)left) <= 0) break;
        n = recv(fd, out + got, cap - got - 1, 0);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) break;
        got += (size_t)n;
        if (memchr(out, '\n', got)) break;
    }
    close(fd);
    out[got] = '\0';
    return got && strchr(out, '\n') ? 0 : -1;
}

/* ---- the daemon: start and stop ---- */

static void elf_id_make(void) {
    unsigned long long h = 1469598103934665603ULL;
    unsigned i;
    for (i = 0; i < usbnet_elf_len; i++) {
        h ^= usbnet_elf[i];
        h *= 1099511628211ULL;
    }
    snprintf(g_elf_id, sizeof g_elf_id, "%016llx", h);
}

static int write_key_file(void) {
    char tmp[] = USBNET_KEY_PATH ".tmp", text[USBNET_KEY_HEX + 40];
    int fd, k;
    if (!g_key[0]) {
        static const char d[] = "0123456789abcdef";
        uint8_t raw[32];
        int i;
        arc4random_buf(raw, sizeof raw);
        for (i = 0; i < 32; i++) {
            g_key[2 * i] = d[raw[i] >> 4];
            g_key[2 * i + 1] = d[raw[i] & 15];
        }
        g_key[USBNET_KEY_HEX] = '\0';
        memset(raw, 0, sizeof raw);
    }
    mkdir("/data/ps5upload", 0777);
    mkdir(USBNET_DIR, 0700);
    k = snprintf(text, sizeof text, "%s\n%s\n", g_key, g_elf_id);
    fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) return -1;
    (void)fchmod(fd, 0600);
    if (write(fd, text, (size_t)k) != k || fsync(fd) != 0) {
        memset(text, 0, sizeof text);
        close(fd);
        unlink(tmp);
        return -1;
    }
    memset(text, 0, sizeof text);
    close(fd);
    if (rename(tmp, USBNET_KEY_PATH) != 0) {
        unlink(tmp);
        return -1;
    }
    return 0;
}

/* The key a daemon started by an earlier helper knows (the file's), if it is there. */
static void read_key_file(void) {
    char buf[USBNET_KEY_HEX + 64];
    FILE *f = fopen(USBNET_KEY_PATH, "r");
    size_t n;
    if (!f) return;
    n = fread(buf, 1, sizeof buf - 1, f);
    fclose(f);
    buf[n] = '\0';
    if (n >= USBNET_KEY_HEX && strspn(buf, "0123456789abcdef") >= USBNET_KEY_HEX) {
        memcpy(g_key, buf, USBNET_KEY_HEX);
        g_key[USBNET_KEY_HEX] = '\0';
    }
    memset(buf, 0, sizeof buf);
}

/* The ELF to the loader: connect (retrying a refused port), send it all, half-close, and give the
 * loader a moment to read it (elfldr then hands this connection to the daemon as its stdio). */
static int send_to_loader(void) {
    long long until = now_ms() + LOADER_RETRY_MS;
    const unsigned char *b = usbnet_elf;
    size_t n = usbnet_elf_len;
    int fd;
    if (!usbnet_elf_len) return -1;
    for (;;) {
        fd = dial(LOADER_PORT, 2000);
        if (fd >= 0) break;
        if (now_ms() >= until) return -1;
        nap_ms(300);
    }
    {
        struct timeval to = {5, 0};
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &to, sizeof to);
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &to, sizeof to);
    }
    if (send_all(fd, (const char *)b, n) != 0) {
        close(fd);
        return -1;
    }
    shutdown(fd, SHUT_WR);
    {
        long long stop = now_ms() + 1000;
        char sink[256];
        while (now_ms() < stop) {
            struct pollfd p = {fd, POLLIN, 0};
            if (poll(&p, 1, 250) <= 0) continue;
            if (recv(fd, sink, sizeof sink, 0) <= 0) break;
        }
    }
    close(fd);
    return 0;
}

static int pid_alive(int pid) {
    return pid > 1 && pid != getpid() && (kill(pid, 0) == 0 || errno != ESRCH);
}

/* SIGTERM, then SIGKILL after a moment: only the pid the daemon gave us. */
static void kill_daemon(int pid) {
    long long until;
    if (pid <= 1 || pid == getpid()) return;
    fprintf(stderr, "[usbnet] SIGTERM daemon pid %d: %d\n", pid, kill(pid, SIGTERM));
    until = now_ms() + USBNET_QUIT_WAIT_MS;
    while (now_ms() < until) {
        if (!pid_alive(pid)) return;
        nap_ms(50);
    }
    fprintf(stderr, "[usbnet] SIGKILL daemon pid %d: %d\n", pid, kill(pid, SIGKILL));
}

/* A clean stop: quit, wait for its port to close, then the pid if it is still there. */
static void stop_daemon(const char *why) {
    char reply[256];
    int pid;
    long long until;
    pthread_mutex_lock(&g_mu);
    pid = g.pid_daemon;
    pthread_mutex_unlock(&g_mu);
    fprintf(stderr, "[usbnet] stopping the daemon: %s\n", why);
    (void)exchange("\"op\":\"quit\"", reply, sizeof reply);
    until = now_ms() + USBNET_QUIT_WAIT_MS;
    while (now_ms() < until) {
        int fd = dial(USBNET_PORT, 300);
        if (fd == -2 && !pid_alive(pid)) break;
        if (fd >= 0) close(fd);
        nap_ms(100);
    }
    if (pid_alive(pid)) kill_daemon(pid);
    pthread_mutex_lock(&g_mu);
    g.alive = 0;
    g.answered = 0;
    g.pid_daemon = 0;
    g.minion_sent = -1;
    snprintf(g.state, sizeof g.state, "%s", "");
    pthread_mutex_unlock(&g_mu);
}

/* {"ok":true,"elf":"<id>","pid":N}: 1 ours (pid set), 0 another build, -1 no daemon answer. */
static int ping(int *pid) {
    char reply[256], elf[32];
    uint64_t p = 0;
    int ok = 0;
    size_t n;
    if (exchange("\"op\":\"ping\"", reply, sizeof reply) != 0) return -1;
    n = strlen(reply);
    if (sj_bool(reply, n, "ok", &ok) != 1 || !ok) return -1;
    if (sj_u64(reply, n, "pid", &p) == 1) *pid = (int)p;
    if (sj_str(reply, n, "elf", elf, sizeof elf) != 1) return 0;
    return strcmp(elf, g_elf_id) == 0;
}

static void launch(void) {
    int pid = 0, ours;
    read_key_file();
    ours = ping(&pid);
    if (ours == 1) { /* left by an earlier helper of this build: adopt it */
        pthread_mutex_lock(&g_mu);
        g.pid_daemon = pid;
        g.alive = 1;
        g.launched_ms = now_ms();
        pthread_mutex_unlock(&g_mu);
        fprintf(stderr, "[usbnet] daemon pid %d already running: kept\n", pid);
        return;
    }
    if (ours == 0) {
        pthread_mutex_lock(&g_mu);
        g.pid_daemon = pid;
        pthread_mutex_unlock(&g_mu);
        stop_daemon("another build");
    }
    if (write_key_file() != 0 || send_to_loader() != 0) {
        pthread_mutex_lock(&g_mu);
        snprintf(g.error, sizeof g.error, "%s",
                 usbnet_elf_len ? "the console's ELF loader (port 9021) did not take the adapter driver"
                                : "this helper was built without the adapter driver");
        g.next_try_ms = now_ms() + 30000;
        pthread_mutex_unlock(&g_mu);
        fprintf(stderr, "[usbnet] daemon not started (loader or key file); trying again in 30 s\n");
        return;
    }
    fprintf(stderr, "[usbnet] daemon sent to the loader (%u bytes)\n", usbnet_elf_len);
    pthread_mutex_lock(&g_mu);
    g.alive = 1;
    g.answered = 0;
    g.pid_daemon = 0;
    g.minion_sent = -1;
    g.config_dirty = 1;
    g.launched_ms = now_ms();
    g.error[0] = '\0';
    snprintf(g.state, sizeof g.state, "%s", USBNET_ST_STARTING);
    pthread_mutex_unlock(&g_mu);
}

/* ---- the daemon: status ---- */

static void str_field(const char *j, size_t n, const char *key, char *out, size_t cap) {
    if (sj_str(j, n, key, out, cap) != 1) out[0] = '\0';
}

static unsigned long long u64_field(const char *j, size_t n, const char *key) {
    uint64_t v = 0;
    return sj_u64(j, n, key, &v) == 1 ? v : 0;
}

/* The usb_errors object's members summed (they are flat numbers inside one object). */
static unsigned long long usb_errors(const char *j) {
    const char *p = strstr(j, "\"usb_errors\":{"), *end;
    unsigned long long sum = 0;
    if (!p) return 0;
    p += 14;
    end = strchr(p, '}');
    while (p && end && p < end) {
        const char *colon = strchr(p, ':');
        if (!colon || colon > end) break;
        sum += strtoull(colon + 1, NULL, 10);
        p = strchr(colon, ',');
        if (p) p++;
    }
    return sum;
}

static void note_status(const char *j) {
    size_t n = strlen(j);
    char state[16], ip_before[16];
    uint64_t pid = 0;
    int became_ready;
    str_field(j, n, "state", state, sizeof state);
    pthread_mutex_lock(&g_mu);
    memcpy(ip_before, g.ip, sizeof ip_before);
    became_ready = !strcmp(state, USBNET_ST_READY) && strcmp(g.state, USBNET_ST_READY);
    if (sj_u64(j, n, "pid", &pid) == 1) g.pid_daemon = (int)pid;
    memcpy(g.state, state, sizeof state);
    str_field(j, n, "why", g.why, sizeof g.why);
    str_field(j, n, "chip", g.d_chip, sizeof g.d_chip);
    str_field(j, n, "usb_id", g.usb_id, sizeof g.usb_id);
    str_field(j, n, "ip", g.ip, sizeof g.ip);
    str_field(j, n, "mask", g.mask, sizeof g.mask);
    str_field(j, n, "gateway", g.gw, sizeof g.gw);
    str_field(j, n, "source", g.source, sizeof g.source);
    g.max_mbps = (unsigned)u64_field(j, n, "max_mbps");
    g.link_mbps = (unsigned)u64_field(j, n, "link_mbps");
    g.link_changes = (unsigned)u64_field(j, n, "link_changes");
    g.rx_bytes = u64_field(j, n, "rx_bytes");
    g.tx_bytes = u64_field(j, n, "tx_bytes");
    g.rx_frames = u64_field(j, n, "rx_frames");
    g.tx_frames = u64_field(j, n, "tx_frames");
    g.uptime_s = u64_field(j, n, "uptime_s");
    g.errors = usb_errors(j) + u64_field(j, n, "rx_drops");
    g.answered = 1;
    g.last_ok_ms = now_ms();
    if (!strcmp(state, USBNET_ST_READY)) g.restarts = 0;
    {
        char chip[12], ip[16], source[12], msg[160];
        unsigned mbps = g.link_mbps;
        int moved = strcmp(ip_before, g.ip) != 0;
        memcpy(chip, g.d_chip, sizeof chip);
        memcpy(ip, g.ip, sizeof ip);
        memcpy(source, g.source, sizeof source);
        pthread_mutex_unlock(&g_mu);
        if (became_ready) {
            fprintf(stderr, "[usbnet] ready: %s, %u Mbit/s, address %s (%s)\n", chip, mbps, ip, source);
            snprintf(msg, sizeof msg, "PS5Upload: USB adapter connected (%s, %u Mbit/s): %s", chip, mbps, ip);
            if (moved) notif_send_serialised(msg, 0);
        }
        if (moved || became_ready) webui_set_extra_addr(!strcmp(state, USBNET_ST_READY) ? ip : "");
    }
}

/* The helper's wishes, when they changed: Minion's LAN access and the static address. */
static void send_config(void) {
    char fields[200], reply[128];
    int minion = webui_lan_on(), dirty;
    pthread_mutex_lock(&g_mu);
    dirty = g.config_dirty || g.minion_sent != minion;
    snprintf(fields, sizeof fields, "\"op\":\"config\",\"minion\":%d,\"ip\":\"%s\",\"mask\":\"%s\",\"gateway\":\"%s\"", minion,
             g.sip, g.smask, g.sgw);
    pthread_mutex_unlock(&g_mu);
    if (!dirty || exchange(fields, reply, sizeof reply) != 0 || !strstr(reply, "\"ok\":true")) return;
    pthread_mutex_lock(&g_mu);
    g.config_dirty = 0;
    g.minion_sent = minion;
    pthread_mutex_unlock(&g_mu);
}

/* One poll of a daemon that should be running. */
static void poll_daemon(void) {
    char *reply = malloc(USBNET_REPLY_MAX);
    int rc;
    long long t = now_ms();
    if (!reply) return;
    rc = exchange("\"op\":\"status\"", reply, USBNET_REPLY_MAX);
    if (rc == 0) {
        note_status(reply);
        send_config();
        free(reply);
        return;
    }
    free(reply);
    pthread_mutex_lock(&g_mu);
    if (!g.answered) {
        /* Not answering yet: a bring-up may take seconds, never this long. */
        if (t - g.launched_ms >= USBNET_START_BUDGET_S * 1000LL) {
            int pid = g.pid_daemon;
            g.latched = 1;
            g.alive = 0;
            snprintf(g.state, sizeof g.state, "%s", USBNET_ST_FAILED);
            snprintf(g.error, sizeof g.error, "%s", "the adapter driver did not answer: unplug the adapter and plug it back in");
            pthread_mutex_unlock(&g_mu);
            fprintf(stderr, "[usbnet] daemon silent %d s after its launch: given up until a replug\n", USBNET_START_BUDGET_S);
            if (pid) kill_daemon(pid);
            return;
        }
        pthread_mutex_unlock(&g_mu);
        return;
    }
    /* It answered before and is gone now (or its port is closed): it ended. Its last state says why. */
    if (rc == -2 || t - g.last_ok_ms > 10000) {
        int unsupported = !strcmp(g.state, USBNET_ST_UNSUPPORTED) || !strcmp(g.state, USBNET_ST_NONE);
        g.alive = 0;
        g.answered = 0;
        g.restarts++;
        if (!unsupported && strcmp(g.state, USBNET_ST_FAILED)) snprintf(g.state, sizeof g.state, "%s", USBNET_ST_FAILED);
        if (g.why[0]) snprintf(g.error, sizeof g.error, "%s", g.why);
        if (unsupported || g.restarts >= USBNET_MAX_RESTARTS) g.latched = 1;
        g.next_try_ms = t + (long long)BACKOFF_BASE_S * 1000LL * (1LL << (g.restarts > 3 ? 3 : g.restarts));
        fprintf(stderr, "[usbnet] daemon ended (%s): %s\n", g.state, g.why);
        pthread_mutex_unlock(&g_mu);
        webui_set_extra_addr("");
        return;
    }
    pthread_mutex_unlock(&g_mu);
}

/* ---- the watcher ---- */

static void *watcher_main(void *unused) {
    long long next_scan = 0, next_poll = 0;
    (void)unused;
    for (;;) {
        long long t = now_ms();
        int want, alive, quit, latched;
        if (t >= next_scan) {
            if (scan_bus()) {
                /* plugged in again: whatever failed before may work now */
                pthread_mutex_lock(&g_mu);
                g.latched = 0;
                g.restarts = 0;
                g.next_try_ms = 0;
                g.error[0] = '\0';
                pthread_mutex_unlock(&g_mu);
            }
            next_scan = t + USBNET_SCAN_S * 1000LL;
        }
        pthread_mutex_lock(&g_mu);
        quit = g.quit;
        want = g.enabled && g.present && !g.paused && !quit && power_phase_current() == PP_NORMAL;
        alive = g.alive;
        latched = g.latched;
        pthread_mutex_unlock(&g_mu);
        if (alive && !want) {
            stop_daemon(quit ? "the helper is stopping" : "the feature was turned off, the adapter went away, or rest mode");
            webui_set_extra_addr("");
        } else if (!alive && want && !latched && t >= g.next_try_ms) {
            launch();
            next_poll = t + 1000;
        } else if (alive && t >= next_poll) {
            poll_daemon();
            next_poll = t + USBNET_POLL_S * 1000LL;
        }
        if (quit && !alive) break;
        nap_ms(250);
    }
    return NULL;
}

int usbnet_boot(void) {
    pthread_attr_t attr;
    int rc;
    elf_id_make();
    settings_load();
    pthread_mutex_lock(&g_mu);
    g.minion_sent = -1;
    pthread_mutex_unlock(&g_mu);
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, 512 * 1024);
    rc = pthread_create(&g_thread, &attr, watcher_main, NULL);
    pthread_attr_destroy(&attr);
    if (rc != 0) {
        fprintf(stderr, "[usbnet] watcher thread not started: %d\n", rc);
        return -1;
    }
    pthread_mutex_lock(&g_mu);
    g.booted = 1;
    pthread_mutex_unlock(&g_mu);
    return 0;
}

void usbnet_shutdown(void) {
    int booted;
    pthread_mutex_lock(&g_mu);
    booted = g.booted;
    g.quit = 1;
    pthread_mutex_unlock(&g_mu);
    if (booted) pthread_join(g_thread, NULL);
}

void usbnet_set_paused(int paused) {
    pthread_mutex_lock(&g_mu);
    if (paused != g.paused) fprintf(stderr, "[usbnet] %s\n", paused ? "paused: rest mode (the daemon is stopped)" : "resumed");
    g.paused = paused;
    pthread_mutex_unlock(&g_mu);
}

/* ---- answers ---- */

static const char *state_locked(void) {
    if (!g.enabled) return "off";
    if (!g.present) return USBNET_ST_NONE;
    if (g.paused || power_phase_current() != PP_NORMAL) return "paused";
    if (g.latched) return g.state[0] && !strcmp(g.state, USBNET_ST_UNSUPPORTED) ? USBNET_ST_UNSUPPORTED : USBNET_ST_FAILED;
    if (!g.alive || !g.state[0]) return g.error[0] ? USBNET_ST_FAILED : USBNET_ST_STARTING;
    return g.state;
}

static int status_json(char *out, size_t cap) {
    sj_buf_t b;
    const char *state;
    int i, n;
    sj_init(&b, cap);
    pthread_mutex_lock(&g_mu);
    state = state_locked();
    sj_fmt(&b, "{\"ok\":true,\"experimental\":true,\"enabled\":%s", g.enabled ? "true" : "false");
    sj_kv_str(&b, 1, "state", state);
    sj_kv_str(&b, 1, "error",
              !strcmp(state, USBNET_ST_FAILED) || !strcmp(state, USBNET_ST_UNSUPPORTED) ? (g.error[0] ? g.error : g.why) : "");
    if (g.present)
        sj_fmt(&b, ",\"adapter\":{\"usb_id\":\"%04x:%04x\",\"chip\":\"%s\",\"max_mbps\":%u}", g.vid, g.pid,
               g.d_chip[0] ? g.d_chip : usbnet_chip_name(g.chip), g.max_mbps ? g.max_mbps : usbnet_chip_max_mbps(g.chip));
    else
        sj_raw(&b, ",\"adapter\":null");
    {
        int live = g.alive && g.answered;
        sj_fmt(&b, ",\"link_mbps\":%u", live ? g.link_mbps : 0);
        sj_kv_str(&b, 1, "ip", live && !strcmp(state, USBNET_ST_READY) ? g.ip : "");
        sj_kv_str(&b, 1, "mask", live ? g.mask : "");
        sj_kv_str(&b, 1, "gateway", live ? g.gw : "");
        sj_kv_str(&b, 1, "source", live ? g.source : "");
    }
    sj_kv_str(&b, 1, "static_ip", g.sip);
    sj_kv_str(&b, 1, "static_mask", g.smask);
    sj_kv_str(&b, 1, "static_gateway", g.sgw);
    sj_fmt(&b, ",\"port\":%d,\"minion\":%s", USBNET_AVA1_PORT, g.minion_sent == 1 ? "true" : "false");
    sj_fmt(&b, ",\"rx_bytes\":%llu,\"tx_bytes\":%llu,\"rx_frames\":%llu,\"tx_frames\":%llu,\"errors\":%llu", g.rx_bytes,
           g.tx_bytes, g.rx_frames, g.tx_frames, g.errors);
    sj_fmt(&b, ",\"link_changes\":%u,\"uptime_s\":%llu,\"restarts\":%d", g.link_changes, g.uptime_s, g.restarts);
    sj_raw(&b, ",\"seen\":[");
    for (i = 0; i < g.n_seen; i++) sj_fmt(&b, "%s\"%s\"", i ? "," : "", g.seen[i]);
    sj_raw(&b, "]}");
    pthread_mutex_unlock(&g_mu);
    if (b.err || b.len >= cap) {
        sj_free(&b);
        return -1;
    }
    memcpy(out, b.buf, b.len + 1);
    n = (int)b.len;
    sj_free(&b);
    return n;
}

int usbnet_call(const char *body, size_t len, char *out, size_t cap) {
    char op[16] = "";
    if (len && sj_str(body, len, "op", op, sizeof op) < 0) return snprintf(out, cap, "{\"ok\":false,\"error\":\"bad_request\"}");
    if (!op[0] || !strcmp(op, "status")) return status_json(out, cap);
    if (!strcmp(op, "enable") || !strcmp(op, "disable")) {
        int rc;
        pthread_mutex_lock(&g_mu);
        g.enabled = !strcmp(op, "enable");
        if (g.enabled) { /* a fresh start: a failure before may have been fixed by the user */
            g.latched = 0;
            g.restarts = 0;
            g.next_try_ms = 0;
            g.error[0] = '\0';
        }
        rc = settings_save_locked();
        pthread_mutex_unlock(&g_mu);
        fprintf(stderr, "[usbnet] turned %s%s\n", !strcmp(op, "enable") ? "on" : "off", rc ? " (the setting was not saved)" : "");
        return status_json(out, cap);
    }
    if (!strcmp(op, "address")) {
        char ip[16] = "", mask[16] = "", gw[16] = "";
        if (sj_str(body, len, "ip", ip, sizeof ip) < 0 || sj_str(body, len, "mask", mask, sizeof mask) < 0 ||
            sj_str(body, len, "gateway", gw, sizeof gw) < 0 || (ip[0] && !usbnet_ipv4_ok(ip)) ||
            (ip[0] && !usbnet_ipv4_ok(mask[0] ? mask : "255.255.255.0")) || (gw[0] && !usbnet_ipv4_ok(gw)))
            return snprintf(out, cap, "{\"ok\":false,\"error\":\"bad_address\"}");
        pthread_mutex_lock(&g_mu);
        snprintf(g.sip, sizeof g.sip, "%s", ip);
        snprintf(g.smask, sizeof g.smask, "%s", ip[0] ? (mask[0] ? mask : "255.255.255.0") : "");
        snprintf(g.sgw, sizeof g.sgw, "%s", ip[0] ? gw : "");
        g.config_dirty = 1;
        (void)settings_save_locked();
        pthread_mutex_unlock(&g_mu);
        fprintf(stderr, "[usbnet] address: %s\n", ip[0] ? "static" : "DHCP");
        return status_json(out, cap);
    }
    return snprintf(out, cap, "{\"ok\":false,\"error\":\"bad_op\"}");
}

int usbnet_status_brief(char *out, size_t cap) {
    int n;
    pthread_mutex_lock(&g_mu);
    if (!strcmp(state_locked(), USBNET_ST_READY) && g.ip[0])
        n = snprintf(out, cap, "{\"ip\":\"%s\",\"port\":%d,\"mbps\":%u,\"chip\":\"%s\"}", g.ip, USBNET_AVA1_PORT, g.link_mbps,
                     g.d_chip);
    else
        n = snprintf(out, cap, "null");
    pthread_mutex_unlock(&g_mu);
    return n > 0 && (size_t)n < cap ? n : -1;
}

void usbnet_current_ip(char out[16]) {
    pthread_mutex_lock(&g_mu);
    snprintf(out, 16, "%s", !strcmp(state_locked(), USBNET_ST_READY) ? g.ip : "");
    pthread_mutex_unlock(&g_mu);
}
