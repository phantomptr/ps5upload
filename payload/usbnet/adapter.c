/* SPDX-License-Identifier: GPL-3.0-or-later
 * Adapted from tom-ps5-usbnet (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later),
 * src/adapter.c, for PS5Upload's USB adapter daemon (payload/usbnet/README.md). */
/* Finds a supported adapter on the console's USB nodes and moves it between its
 * configurations. No kernel driver is detached: if one is active, the run stops. */
#include "adapter.h"
#include "usbn_log.h"

#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/types.h>

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define CONFIG_WAIT_MS 5000
#define DRIVER_PROBE_MS 500
#define N_BUS 8
#define N_ADDR 32
#define MAX_CONFIGS 8
#define CONFIG_BYTES 2048
#define UNCONFIGURED 0xff
#define SHORT_XFER_OK 0x0004            /* USB_SHORT_XFER_OK: usbdi.h is kernel-only */

/* The vendor's own IDs: the RTL8159, the RTL8157 and the RTL8156 family, whose silicon
 * is told apart by its version once the node is open. An OEM ID is not guessed at. */
static const uint16_t g_products[] = {0x815a, 0x8157, 0x8156};
#define MAX_SEEN 8

int adapter_gone;
static uint32_t g_seen[MAX_SEEN];
static int g_seen_count;

static long now_ms(void)
{
    struct timespec ts;

    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static int is_target(const struct usb_device_descriptor *dd)
{
    size_t i;

    if (UGETW(dd->idVendor) != 0x0bda) return 0;
    for (i = 0; i < sizeof g_products / sizeof g_products[0]; i++)
        if (UGETW(dd->idProduct) == g_products[i]) return 1;
    return 0;
}

static int read_config(int fd, int index, uint8_t *data, size_t cap)
{
    struct usb_gen_descriptor gd;

    memset(&gd, 0, sizeof gd);
    gd.ugd_data = data;
    gd.ugd_maxlen = (uint16_t)cap;
    gd.ugd_config_index = (uint8_t)index;
    if (ioctl(fd, USB_GET_FULL_DESC, &gd) != 0) return -1;
    return gd.ugd_actlen < cap ? gd.ugd_actlen : (int)cap;
}

/* The ECM configuration, for its pass-through and its MAC string. Without one, an NCM
 * configuration's MAC string will do, and there is no pass-through. */
static int find_ecm_config(int fd, int configurations, struct adapter *a)
{
    static uint8_t config[CONFIG_BYTES];
    struct ncm_function ncm;
    int i, have_ncm = 0;

    if (configurations > MAX_CONFIGS) configurations = MAX_CONFIGS;
    for (i = 0; i < configurations && a->ecm_index < 0; i++) {
        int len = read_config(fd, i, config, sizeof config);

        if (len <= 0) continue;
        if (ncm_find_ecm_function(config, (size_t)len, &a->ecm) == NCM_OK) a->ecm_index = i;
        else if (!have_ncm && ncm_find_function(config, (size_t)len, &ncm) == NCM_OK) have_ncm = 1;
    }
    if (a->ecm_index < 0 && have_ncm) {
        a->ecm = ncm;
        say("no ECM configuration: the address comes from the NCM one");
    }
    return a->ecm_index >= 0 || have_ncm ? 0 : -1;
}

/* What else is on the bus goes in the log, as vendor:product only, so that an adapter
 * this does not know can be named in a report. */
static void note_device(const struct usb_device_descriptor *dd)
{
    uint32_t id = (uint32_t)UGETW(dd->idVendor) << 16 | UGETW(dd->idProduct);
    int i;

    if (dd->bDeviceClass == 0x09) return;      /* hubs */
    for (i = 0; i < g_seen_count; i++)
        if (g_seen[i] == id) return;
    if (g_seen_count < MAX_SEEN) g_seen[g_seen_count++] = id;
}

static void log_seen(void)
{
    char line[MAX_SEEN * 10 + 1];
    size_t used = 0;
    int i;

    for (i = 0; i < g_seen_count; i++)
        used += (size_t)snprintf(line + used, sizeof line - used, "%s%04x:%04x", i ? " " : "", g_seen[i] >> 16, g_seen[i] & 0xffff);
    if (g_seen_count) say("usb devices: %s", line);
    else say("usb devices: none");
}

/* 1: found, 0: not this node, -1: the adapter, without the configuration it needs. */
static int probe_node(const char *path, struct adapter *a)
{
    struct usb_device_descriptor dd;
    struct stat st;
    int fd, found = 0, value;

    if (stat(path, &st) != 0) return 0;
    fd = open(path, O_RDONLY | O_NONBLOCK);
    if (fd < 0) return 0;
    if (ioctl(fd, USB_GET_DEVICE_DESC, &dd) != 0) {
        close(fd);
        return 0;
    }
    if (!is_target(&dd)) {
        note_device(&dd);
    } else {
        say("adapter %04x:%04x", UGETW(dd.idVendor), UGETW(dd.idProduct));
        a->product = UGETW(dd.idProduct);
        if (ioctl(fd, USB_GET_CONFIG, &value) == 0) a->original_index = value & 0xff;
        if (find_ecm_config(fd, dd.bNumConfigurations, a) == 0) {
            snprintf(a->path, sizeof a->path, "%s", path);
            found = 1;
        } else {
            found = -1;
        }
    }
    close(fd);
    return found;
}

static int find_adapter(struct adapter *a)
{
    char path[32];
    long end = now_ms() + ADAPTER_WAIT_SECONDS * 1000L;
    int bus, addr;

    do {
        for (bus = 0; bus < N_BUS; bus++)
            for (addr = 2; addr < N_ADDR; addr++) {
                int found;

                snprintf(path, sizeof path, "/dev/ugen%d.%d", bus, addr);
                found = probe_node(path, a);
                if (found) return found > 0 ? 0 : -1;
            }
        sleep(1);
    } while (now_ms() < end);
    return -1;
}

/* USB_SET_CONFIG only queues the change, and the node may be recreated. */
static int set_config(struct adapter *a, int index)
{
    long start = now_ms();
    int value = index;

    if (ioctl(a->fd, USB_SET_CONFIG, &value) != 0) {
        say("USB_SET_CONFIG: errno %d", errno);
        return -1;
    }
    while (now_ms() - start < CONFIG_WAIT_MS) {
        value = -1;
        if (ioctl(a->fd, USB_GET_CONFIG, &value) == 0) {
            if ((value & 0xff) == index) return 0;
        } else {
            close(a->fd);
            a->fd = -1;
            usleep(200000);
            a->fd = open(a->path, O_RDWR);
            if (a->fd < 0) continue;
        }
        usleep(20000);
    }
    say("configuration %d did not become active", index);
    return -1;
}

static int set_alternate(struct adapter *a, unsigned iface, unsigned alternate)
{
    struct usb_alt_interface alt;

    memset(&alt, 0, sizeof alt);
    alt.uai_interface_index = (uint8_t)iface;
    alt.uai_alt_index = (uint8_t)alternate;
    if (ioctl(a->fd, USB_SET_ALTINTERFACE, &alt) != 0) {
        say("USB_SET_ALTINTERFACE: errno %d", errno);
        return -1;
    }
    return 0;
}

int adapter_driver_active(struct adapter *a)
{
    int active = 0, iface;

    for (iface = 0; iface < 4; iface++) {
        int value = iface;

        if (ioctl(a->fd, USB_IFACE_DRIVER_ACTIVE, &value) == 0) active = 1;
    }
    return active;
}

int adapter_control_in(int fd, const uint8_t setup[8], uint8_t *data)
{
    struct usb_ctl_request request;

    if (!(setup[0] & 0x80)) return -1;
    memset(&request, 0, sizeof request);
    memcpy(&request.ucr_request, setup, 8);
    request.ucr_data = data;
    request.ucr_flags = SHORT_XFER_OK;
    if (ioctl(fd, USB_DO_REQUEST, &request) != 0) return -1;
    return request.ucr_actlen;
}

int adapter_read_config(struct adapter *a, int index, uint8_t *data, size_t capacity)
{
    return read_config(a->fd, index, data, capacity);
}

int adapter_select_config(struct adapter *a, int index)
{
    if (a->active_index == index) return 0;
    a->active_index = index;
    if (set_config(a, index) != 0) return -1;
    usleep(DRIVER_PROBE_MS * 1000);     /* the kernel probes drivers after the change */
    return 0;
}

static int ecm_packet_filter(struct adapter *a)
{
    struct usb_ctl_request ctl;

    memset(&ctl, 0, sizeof ctl);
    ctl.ucr_request.bmRequestType = 0x21;
    ctl.ucr_request.bRequest = NCM_SET_PACKET_FILTER;
    USETW(ctl.ucr_request.wValue, NCM_FILTER_DIRECTED | NCM_FILTER_BROADCAST | NCM_FILTER_ALL_MULTICAST);
    USETW(ctl.ucr_request.wIndex, a->ecm.comm_iface);
    USETW(ctl.ucr_request.wLength, 0);
    return ioctl(a->fd, USB_DO_REQUEST, &ctl);
}

/* Every run on a real console so far has come through here before the chip's bring-up.
 * Nothing shows that it is needed, and nothing shows that the bring-up works without it. */
int adapter_pass_through_ecm(struct adapter *a)
{
    if (adapter_select_config(a, a->ecm_index) != 0 || adapter_driver_active(a)) return -1;
    if (set_alternate(a, a->ecm.data_iface, a->ecm.data_alt) != 0 || set_alternate(a, a->ecm.data_iface, 0) != 0 ||
            set_alternate(a, a->ecm.data_iface, a->ecm.data_alt) != 0 || ecm_packet_filter(a) != 0)
        return -1;
    return set_alternate(a, a->ecm.data_iface, 0);
}

int adapter_read_mac(struct adapter *a, uint8_t mac[6])
{
    struct usb_gen_descriptor gd;
    uint8_t data[64];

    memset(&gd, 0, sizeof gd);
    gd.ugd_data = data;
    gd.ugd_maxlen = sizeof data;
    gd.ugd_string_index = a->ecm.mac_string;
    gd.ugd_lang_id = 0x0409;
    if (!a->ecm.mac_string || ioctl(a->fd, USB_GET_STRING_DESC, &gd) != 0) return -1;
    return ncm_parse_mac_string(data, gd.ugd_actlen < sizeof data ? gd.ugd_actlen : sizeof data, mac);
}

int adapter_main(void (*run)(struct adapter *adapter))
{
    static struct adapter a;

    a.fd = -1;
    a.original_index = UNCONFIGURED;
    a.ecm_index = -1;
    if (find_adapter(&a) != 0) {
        say("no supported adapter found");
        log_seen();
        return ADAPTER_NONE;
    }
    a.active_index = a.original_index;
    a.fd = open(a.path, O_RDWR);
    if (a.fd < 0) {
        say("adapter: open errno %d", errno);
        return ADAPTER_NONE;
    }
    run(&a);
    if (a.fd >= 0 && !adapter_gone) {
        if (set_alternate(&a, a.data_iface, 0) != 0) say("alternate 0 not restored");
        if (a.original_index != UNCONFIGURED && a.active_index != a.original_index && set_config(&a, a.original_index) != 0)
            say("configuration not restored: replug the adapter");
    }
    if (a.fd >= 0) close(a.fd);
    return a.unusable ? ADAPTER_NONE : 0;
}
