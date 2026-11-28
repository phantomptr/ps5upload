/* SPDX-License-Identifier: GPL-3.0-or-later
 * ps5u-usbnet: PS5Upload's USB Ethernet adapter daemon (EXPERIMENTAL; payload/usbnet/README.md,
 * protocol include/usbnet_proto.h). The helper embeds this ELF and sends it to the console's own
 * loader (:9021) while a supported adapter is plugged in; it is never sent on its own.
 *
 * The adapter's bring-up and data path come from tom-ps5-usbnet
 * (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later): its src/main.c run(),
 * run_network(), watch_link(), wait_for_link() and find_vendor() are the model for the code below.
 * PS5Upload's part: the control port for the helper, the status, a static address, serving only
 * the helper's own ports (fwd.c), and stopping by itself on the signs of rest mode.
 *
 * Threads: main (bring-up, then the link watch), the USB receive thread (vendor.c), lwIP's tcpip
 * thread, the control thread (below), fwd.c's accept thread and two per joined connection. */
#include "adapter.h"
#include "fwd.h"
#include "link.h"
#include "net.h"
#include "usbn_log.h"
#include "vendor.h"

#include "../smb/smbd_json.h"
#include "usbnet_proto.h"

#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define LINK_WAIT_MS 15000L /* for the chip's fastest rate, which a link can still reach while negotiating */
#define LINK_SETTLE_US 1000000
#define TICK_MS 250
#define LINK_POLL_MS 1000L
#define CONFIG_BYTES 2048
#define LINGER_MS 10000L    /* after the adapter's run ended, for the helper to read why */
#define SLEEP_JUMP_S 10     /* the wall clock ran on while the monotonic one stood: the console slept */
#define CTL_IO_MS 2000

static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static struct {
    /* written by main, read by the control thread: under g_mu */
    const char *state, *why;
    enum rtl_chip chip;
    int have_chip, net_started;
    uint16_t product, version;
    struct usbn_link link;
    struct net_lease lease;
    int have_lease;
    /* the helper's wishes (config op) */
    int minion, config_changed;
    char static_ip[16], static_mask[16], static_gw[16];
    long long last_request_ms, started_ms;
    char key[USBNET_KEY_HEX + 1], elf[24];
} g;
static volatile int g_stop;
static const char *g_stop_why;
static int g_ctl_fd = -1;

static long long mono_ms(void)
{
    struct timespec ts;

    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static long long wall_s(void)
{
    return (long long)time(NULL);
}

void usbn_net_assert(const char *what, const char *file, int line)
{
    say("lwIP assertion %s at %s:%d", what, file, line);
    checkpoint();
    _exit(3);
}

static void stop_for(const char *why)
{
    if (!g_stop) {
        g_stop_why = why;
        g_stop = 1;
    }
}

static void on_term(int sig)
{
    (void)sig;
    if (!g_stop) g_stop_why = "asked to stop";
    g_stop = 1;
}

static void on_network_down(int err)
{
    (void)err;
    stop_for("the console's network is going away (rest mode)");
}

static void set_state(const char *state, const char *why)
{
    pthread_mutex_lock(&g_mu);
    g.state = state;
    if (why) g.why = why;
    pthread_mutex_unlock(&g_mu);
}

static const char *chip_name(enum rtl_chip chip)
{
    return chip == RTL_8156B ? "RTL8156B" : chip == RTL_8157 ? "RTL8157" : chip == RTL_8159 ? "RTL8159" : "";
}

/* ---- the control port ---------------------------------------------------------- */

static void status_json(sj_buf_t *b)
{
    struct vnd_counts usb;
    struct net_counts net;
    struct fwd_status fwd[FWD_RULES];
    struct usbn_link link;
    struct net_lease lease;
    const char *state, *why;
    int have_chip, net_started, have_lease, i;
    enum rtl_chip chip;
    uint16_t product, version;
    long long started;

    pthread_mutex_lock(&g_mu);
    state = g.state;
    why = g.why;
    have_chip = g.have_chip;
    chip = g.chip;
    product = g.product;
    version = g.version;
    link = g.link;
    lease = g.lease;
    have_lease = g.have_lease;
    net_started = g.net_started;
    started = g.started_ms;
    pthread_mutex_unlock(&g_mu);
    memset(&usb, 0, sizeof usb);
    memset(&net, 0, sizeof net);
    if (have_chip) vnd_counts(&usb);
    if (net_started) net_counts(&net);
    fwd_get_status(fwd);
    sj_fmt(b, "{\"ok\":true,\"elf\":\"%s\",\"pid\":%d", g.elf, (int)getpid());
    sj_kv_str(b, 1, "state", state);
    sj_kv_str(b, 1, "why", why ? why : "");
    sj_kv_str(b, 1, "chip", have_chip ? chip_name(chip) : "");
    if (product) sj_fmt(b, ",\"usb_id\":\"0bda:%04x\"", product);
    else sj_raw(b, ",\"usb_id\":\"\"");
    sj_fmt(b, ",\"chip_version\":\"0x%04x\",\"max_mbps\":%u,\"link_mbps\":%u", version, have_chip ? vnd_regs_max_mbps() : 0,
           link.up ? link.mbps : 0);
    sj_kv_str(b, 1, "ip", have_lease ? lease.ip : "");
    sj_kv_str(b, 1, "mask", have_lease ? lease.mask : "");
    sj_kv_str(b, 1, "gateway", have_lease ? lease.gateway : "");
    sj_kv_str(b, 1, "source", have_lease && lease.source ? lease.source : "");
    sj_fmt(b, ",\"lease_s\":%lu,\"uptime_s\":%lld,\"link_changes\":%u", have_lease ? lease.lease_seconds : 0UL,
           (mono_ms() - started) / 1000, link.changes);
    sj_fmt(b, ",\"rx_frames\":%llu,\"tx_frames\":%llu,\"rx_bytes\":%llu,\"tx_bytes\":%llu,\"rx_drops\":%llu", net.rx_frames,
           net.tx_frames, net.rx_bytes, net.tx_bytes, net.rx_drops + net.tx_drops);
    sj_fmt(b, ",\"usb_errors\":{\"out\":%lu,\"in\":%lu,\"start\":%lu,\"complete\":%lu,\"scan\":%lu}", usb.errors[0], usb.errors[1],
           usb.errors[2], usb.errors[3], usb.errors[4]);
    sj_raw(b, ",\"forwards\":[");
    for (i = 0; i < FWD_RULES; i++)
        sj_fmt(b, "%s{\"port\":%u,\"on\":%s,\"listening\":%s,\"links\":%u,\"bytes\":%llu}", i ? "," : "", fwd[i].port,
               fwd[i].on ? "true" : "false", fwd[i].listening ? "true" : "false", fwd[i].links, (unsigned long long)fwd[i].bytes);
    sj_raw(b, "]}");
}

static int key_ok(const char *got)
{
    unsigned char diff = 0;
    size_t i;

    if (strlen(got) != USBNET_KEY_HEX || strlen(g.key) != USBNET_KEY_HEX) return 0;
    for (i = 0; i < USBNET_KEY_HEX; i++) diff |= (unsigned char)(got[i] ^ g.key[i]);
    return diff == 0;
}

static void reply(int fd, const char *text, size_t n)
{
    while (n) {
        ssize_t w = send(fd, text, n, 0);

        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) return;
        text += w;
        n -= (size_t)w;
    }
}

static void handle(int fd)
{
    char req[USBNET_REQ_MAX + 1], key[USBNET_KEY_HEX + 2], op[16];
    size_t got = 0;
    long long until = mono_ms() + CTL_IO_MS;
    sj_buf_t out;

    while (got < USBNET_REQ_MAX && !memchr(req, '\n', got)) {
        struct pollfd p = {fd, POLLIN, 0};
        long long left = until - mono_ms();
        ssize_t n;

        if (left <= 0 || poll(&p, 1, (int)left) <= 0) return;
        n = recv(fd, req + got, USBNET_REQ_MAX - got, 0);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) break;
        got += (size_t)n;
    }
    req[got] = '\0';
    if (sj_str(req, got, "key", key, sizeof key) != 1 || !key_ok(key)) {
        memset(req, 0, sizeof req);
        reply(fd, "{\"ok\":false,\"error\":\"key\"}\n", 27);
        return;
    }
    memset(key, 0, sizeof key);
    pthread_mutex_lock(&g_mu);
    g.last_request_ms = mono_ms();
    pthread_mutex_unlock(&g_mu);
    if (sj_str(req, got, "op", op, sizeof op) != 1) op[0] = '\0';
    sj_init(&out, USBNET_REPLY_MAX);
    if (!strcmp(op, "ping")) {
        sj_fmt(&out, "{\"ok\":true,\"elf\":\"%s\",\"pid\":%d}", g.elf, (int)getpid());
    } else if (!strcmp(op, "status")) {
        status_json(&out);
    } else if (!strcmp(op, "config")) {
        char ip[16] = "", mask[16] = "", gw[16] = "";
        uint64_t minion = 0;

        if (sj_str(req, got, "ip", ip, sizeof ip) < 0 || sj_str(req, got, "mask", mask, sizeof mask) < 0 ||
            sj_str(req, got, "gateway", gw, sizeof gw) < 0 || sj_u64(req, got, "minion", &minion) < 0) {
            sj_raw(&out, "{\"ok\":false,\"error\":\"bad_request\"}");
        } else {
            pthread_mutex_lock(&g_mu);
            if (strcmp(ip, g.static_ip) || strcmp(mask, g.static_mask) || strcmp(gw, g.static_gw)) g.config_changed = 1;
            memcpy(g.static_ip, ip, sizeof ip);
            memcpy(g.static_mask, mask, sizeof mask);
            memcpy(g.static_gw, gw, sizeof gw);
            g.minion = minion != 0;
            pthread_mutex_unlock(&g_mu);
            fwd_set_minion(minion != 0);
            sj_raw(&out, "{\"ok\":true}");
        }
    } else if (!strcmp(op, "quit")) {
        stop_for("asked to stop");
        sj_raw(&out, "{\"ok\":true}");
    } else {
        sj_raw(&out, "{\"ok\":false,\"error\":\"bad_op\"}");
    }
    sj_raw(&out, "\n");
    if (!out.err) reply(fd, out.buf, out.len);
    sj_free(&out);
    memset(req, 0, sizeof req);
}

static void *ctl_main(void *unused)
{
    (void)unused;
    for (;;) {
        struct pollfd p = {g_ctl_fd, POLLIN, 0};
        int fd;

        if (poll(&p, 1, 500) <= 0) continue;
        fd = accept(g_ctl_fd, NULL, NULL);
        if (fd < 0) {
            if (errno == 163) stop_for("the console's network is going away (rest mode)");
            usleep(50000);
            continue;
        }
        {
            struct timeval to = {2, 0};
            int on = 1;
            setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &to, sizeof to);
            (void)on;
#ifdef SO_NOSIGPIPE
            setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &on, sizeof on);
#endif
        }
        handle(fd);
        close(fd);
    }
    return NULL;
}

static int ctl_start(void)
{
    struct sockaddr_in a;
    pthread_t t;
    pthread_attr_t attr;
    int on = 1, rc;

    g_ctl_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (g_ctl_fd < 0) return -1;
    setsockopt(g_ctl_fd, SOL_SOCKET, SO_REUSEADDR, &on, sizeof on);
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons(USBNET_PORT);
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (bind(g_ctl_fd, (struct sockaddr *)&a, sizeof a) != 0 || listen(g_ctl_fd, 8) != 0) {
        close(g_ctl_fd);
        g_ctl_fd = -1;
        return -1;
    }
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, 256 * 1024);
    pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    rc = pthread_create(&t, &attr, ctl_main, NULL);
    pthread_attr_destroy(&attr);
    return rc ? -1 : 0;
}

/* The key file: the key, then the helper's id of this build. */
static int read_key(void)
{
    char text[USBNET_KEY_HEX + 64];
    FILE *f = fopen(USBNET_KEY_PATH, "r");
    size_t n;
    char *nl;

    if (!f) return -1;
    n = fread(text, 1, sizeof text - 1, f);
    fclose(f);
    text[n] = '\0';
    if (n < USBNET_KEY_HEX) return -1;
    memcpy(g.key, text, USBNET_KEY_HEX);
    g.key[USBNET_KEY_HEX] = '\0';
    nl = strchr(text, '\n');
    if (nl) snprintf(g.elf, sizeof g.elf, "%.*s", (int)strcspn(nl + 1, "\r\n"), nl + 1);
    memset(text, 0, sizeof text);
    return 0;
}

/* ---- the adapter ------------------------------------------------------------------- */

static int stopping(void)
{
    return g_stop || vnd_gone();
}

/* Waits for the chip's fastest rate, which a link that comes up slower can still reach while
 * negotiating. */
static int wait_for_link(void)
{
    long long started = mono_ms();
    int up = 0;

    while (mono_ms() - started < LINK_WAIT_MS && !stopping()) {
        uint16_t status = vnd_regs_phy_status();

        if (rtl_phy_link(status)) {
            up = 1;
            if (rtl_phy_mbps(status) >= vnd_regs_max_mbps()) break;
        }
        usleep(100000);
    }
    return up ? 0 : -1;
}

/* Once a second: the PHY, the address, the state, the forwarder, the helper's config. */
static void watch(long long now, long long *last_poll, int *forwarding, char ip[16])
{
    struct net_lease lease;
    const char *state;
    int have, config, minion;
    char sip[16], smask[16], sgw[16];

    if (now - *last_poll < LINK_POLL_MS) return;
    *last_poll = now;
    {
        uint16_t status = 0;
        int ok = vnd_regs_read_phy(&status) == 0, changed;

        pthread_mutex_lock(&g_mu);
        changed = usbn_link_phy(&g.link, ok, status);
        pthread_mutex_unlock(&g_mu);
        if (!ok && g.link.unreadable == 1) say("link status unreadable, errno %d", errno);
        if (changed == 1) {
            say("link %s%s", g.link.up ? "up" : "down", g.link.up ? "" : " (cable out, or nothing at the other end)");
            if (g.link.up) {
                say("  %u Mbit/s", g.link.mbps);
                if (vnd_regs_enabled() == 0) say("the MAC is no longer enabled: the chip reset itself, replug the adapter");
            }
            vnd_regs_mark_link(g.link.up);
            net_link(g.link.up);
            checkpoint();
        } else if (changed == 2) {
            say("link now %u Mbit/s", g.link.mbps);
        }
    }
    pthread_mutex_lock(&g_mu);
    config = g.config_changed;
    g.config_changed = 0;
    minion = g.minion;
    memcpy(sip, g.static_ip, sizeof sip);
    memcpy(smask, g.static_mask, sizeof smask);
    memcpy(sgw, g.static_gw, sizeof sgw);
    pthread_mutex_unlock(&g_mu);
    (void)minion;
    if (config) {
        say("address: %s", sip[0] ? "static, from the helper" : "DHCP");
        if (net_up(sip, smask, sgw) != 0) say("DHCP did not start");
    }
    have = net_address(&lease) == 0;
    pthread_mutex_lock(&g_mu);
    state = usbn_link_state(&g.link, have, now);
    g.lease = lease;
    g.have_lease = have;
    g.state = state;
    pthread_mutex_unlock(&g_mu);
    if (have && strcmp(lease.ip, ip)) {
        say("address %s from %s", lease.ip, lease.source);
        if (*forwarding) {
            fwd_stop();
            *forwarding = 0;
        }
        snprintf(ip, 16, "%s", lease.ip);
        checkpoint();
    }
    if (have && !*forwarding) {
        if (fwd_start(lease.ip) == 0) *forwarding = 1;
        else say("the ports did not start");
    }
}

static void run_network(void)
{
    static const struct vnd_settings joining = {8, 1, 16 * 1024};
    static const struct vnd_settings serving = {8, 1, VND_TX_BYTES};
    static const struct vnd_settings serving_linux = {8, 1, VND_LINUX_TX_BYTES};
    char ip[16] = "", sip[16], smask[16], sgw[16];
    long long last_poll = 0, wall = wall_s(), mono = mono_ms();
    int forwarding = 0;

    vnd_configure(&joining);
    if (wait_for_link() == 0) {
        uint16_t status = vnd_regs_phy_status();

        pthread_mutex_lock(&g_mu);
        usbn_link_phy(&g.link, 1, status);
        pthread_mutex_unlock(&g_mu);
        vnd_regs_mark_link(1);
        net_link(1);
        usleep(LINK_SETTLE_US);
        say("link up, %u Mbit/s", g.link.mbps);
    } else {
        say("no Ethernet link yet");
    }
    if (vnd_regs_open_filter() != 0) {
        set_state(USBNET_ST_FAILED, "the adapter's packet filter could not be set");
        say("the packet filter could not be set");
        return;
    }
    pthread_mutex_lock(&g_mu);
    memcpy(sip, g.static_ip, sizeof sip);
    memcpy(smask, g.static_mask, sizeof smask);
    memcpy(sgw, g.static_gw, sizeof sgw);
    g.config_changed = 0;
    g.net_started = 1;
    pthread_mutex_unlock(&g_mu);
    if (net_up(sip, smask, sgw) != 0) say("DHCP did not start");
    /* One transfer at a time, eight receive transfers: on the console 3.2 Gbit/s up and 3.0 down
     * (tom-ps5-usbnet's measurement). Linux's aggregate size for the RTL8156B and RTL8157: nothing
     * larger has been sent to them. */
    vnd_configure(g.chip == RTL_8159 ? &serving : &serving_linux);
    fwd_set_down_hook(on_network_down);
    set_state(USBNET_ST_NO_ADDRESS, "");
    checkpoint();
    while (!stopping()) {
        long long now = mono_ms(), w = wall_s();

        if ((w - wall) - (now - mono) / 1000 > SLEEP_JUMP_S) stop_for("the console slept with the adapter running");
        wall = w;
        mono = now;
        watch(now, &last_poll, &forwarding, ip);
        pthread_mutex_lock(&g_mu);
        if (now - g.last_request_ms > USBNET_HELPER_SILENT_S * 1000LL) {
            pthread_mutex_unlock(&g_mu);
            stop_for("no word from the helper");
            break;
        }
        pthread_mutex_unlock(&g_mu);
        usleep(TICK_MS * 1000);
    }
    say("serving ended: %s", vnd_gone() ? "the adapter was lost" : g_stop_why ? g_stop_why : "asked to stop");
    set_state(USBNET_ST_STOPPING, vnd_gone() ? "the adapter stopped answering: unplug it and plug it back in" : g_stop_why);
    checkpoint();
    if (forwarding) fwd_stop();
    net_stop();
}

/* The one configuration whose only interface is the vendor's. */
static int find_vendor(struct adapter *a, unsigned *bulk_in, unsigned *bulk_out)
{
    static uint8_t config[CONFIG_BYTES];
    int index;

    for (index = 0; index < 8; index++) {
        struct rtl_usb_interface iface;
        int len = adapter_read_config(a, index, config, sizeof config);

        if (len <= 0) continue;
        if (rtl_parse_config(config, (size_t)len, 0, 0, &iface) == 0 && iface.class_code == 0xff) {
            *bulk_in = iface.bulk_in;
            *bulk_out = iface.bulk_out;
            return index;
        }
    }
    return -1;
}

static void run(struct adapter *a)
{
    static const struct vnd_hooks hooks = {net_frames};
    static const struct link_ops link = {vnd_tx_reserve, vnd_tx_commit};
    struct rtl_usb_device device = {0x0bda, a->product, 0, 0, 0};
    uint8_t mac[6];
    uint16_t version;
    unsigned bulk_in = 0, bulk_out = 0;
    enum rtl_chip chip;
    int vendor;

    pthread_mutex_lock(&g_mu);
    g.product = a->product;
    pthread_mutex_unlock(&g_mu);
    checkpoint();
    version = vnd_regs_read_version(a->fd);
    pthread_mutex_lock(&g_mu);
    g.version = version;
    pthread_mutex_unlock(&g_mu);
    if (rtl_identify_target(&device, version, &chip) != 0) {
        say("chip version %#06x on 0bda:%04x is not supported", version, a->product);
        set_state(USBNET_ST_UNSUPPORTED, "this adapter's chip revision is not one PS5Upload drives yet");
        a->unusable = 1;
        return;
    }
    pthread_mutex_lock(&g_mu);
    g.chip = chip;
    g.have_chip = 1;
    pthread_mutex_unlock(&g_mu);
    say("chip %s, version %#06x", chip_name(chip), version);
    vendor = find_vendor(a, &bulk_in, &bulk_out);
    if (vendor < 0) {
        say("this adapter has no vendor configuration");
        set_state(USBNET_ST_UNSUPPORTED, "this adapter has no configuration PS5Upload can drive");
        a->unusable = 1;
        return;
    }
    if (adapter_read_mac(a, mac) != 0) {
        set_state(USBNET_ST_FAILED, "the adapter's hardware address could not be read");
        return;
    }
    if (adapter_select_config(a, vendor) != 0) {
        set_state(USBNET_ST_FAILED, "the adapter did not switch to its vendor configuration");
        return;
    }
    if (a->ecm_index >= 0) adapter_pass_through_ecm(a); /* every proven run did this; failing costs nothing */
    if (adapter_select_config(a, vendor) != 0) {
        set_state(USBNET_ST_FAILED, "the adapter did not switch to its vendor configuration");
        return;
    }
    a->data_iface = 0;
    if (adapter_driver_active(a)) {
        say("a kernel driver has the adapter: stopping");
        set_state(USBNET_ST_UNSUPPORTED, "the console's own system is using this adapter");
        return;
    }
    vnd_regs_attach(a->fd, chip, version);
    checkpoint();
    if (vnd_regs_bring_up(mac) != 0) {
        say("chip bring-up failed");
        set_state(USBNET_ST_FAILED, "the adapter did not start: unplug it and plug it back in");
        return;
    }
    if (vnd_open(a->fd, chip, bulk_in, bulk_out, &hooks) == 0) {
        if (net_start(mac, &link) != 0) {
            say("the network stack did not start: errno %d", errno);
            set_state(USBNET_ST_FAILED, "not enough memory for the adapter's network");
        } else if (vnd_start() == 0) {
            run_network();
        } else {
            set_state(USBNET_ST_FAILED, "the adapter's receive thread did not start");
        }
    } else {
        set_state(USBNET_ST_FAILED, "the adapter's USB transfers could not be opened");
    }
    memset(mac, 0, sizeof mac);
    vnd_regs_stop();
    vnd_close();
}

int main(void)
{
    struct sigaction sa;
    int fd, result;
    long long ended;

#ifdef SYS_thr_set_name
    (void)syscall(SYS_thr_set_name, -1, USBNET_NAME);
#endif
    signal(SIGPIPE, SIG_IGN);
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_term;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGTERM, &sa, NULL);
    /* elfldr hands its connection over as stdio: let it go. */
    fd = open("/dev/null", O_RDWR);
    if (fd >= 0) {
        dup2(fd, STDIN_FILENO);
        dup2(fd, STDOUT_FILENO);
        dup2(fd, STDERR_FILENO);
        if (fd > STDERR_FILENO) close(fd);
    }
    mkdir("/data/ps5upload", 0777);
    mkdir(USBNET_DIR, 0700);
    usbn_log_open();
    g.state = USBNET_ST_STARTING;
    g.why = "";
    g.started_ms = g.last_request_ms = mono_ms();
    if (read_key() != 0) {
        say("no key file: exit");
        return 1;
    }
    if (ctl_start() != 0) {
        say("port %d is taken (another daemon runs): exit", USBNET_PORT);
        return 1;
    }
    {
        FILE *pf = fopen(USBNET_PID_PATH, "w");
        if (pf) {
            fprintf(pf, "%d\n", (int)getpid());
            fclose(pf);
        }
    }
    checkpoint();
    result = adapter_main(run);
    if (result == ADAPTER_NONE) {
        pthread_mutex_lock(&g_mu);
        if (!strcmp(g.state, USBNET_ST_STARTING)) {
            g.state = USBNET_ST_NONE;
            g.why = "no supported adapter on the USB ports";
        }
        pthread_mutex_unlock(&g_mu);
    } else {
        pthread_mutex_lock(&g_mu);
        if (strcmp(g.state, USBNET_ST_UNSUPPORTED) && strcmp(g.state, USBNET_ST_FAILED)) {
            g.state = adapter_gone ? USBNET_ST_FAILED : USBNET_ST_STOPPING;
            if (adapter_gone) g.why = "the adapter stopped answering: unplug it and plug it back in";
        }
        pthread_mutex_unlock(&g_mu);
    }
    say("adapter run over: %s", g.why ? g.why : "");
    checkpoint();
    /* The helper reads why before this exits, unless it asked for the stop. */
    ended = mono_ms();
    while (!g_stop && mono_ms() - ended < LINGER_MS) usleep(100000);
    unlink(USBNET_PID_PATH);
    say("exit");
    checkpoint();
    return 0;
}
