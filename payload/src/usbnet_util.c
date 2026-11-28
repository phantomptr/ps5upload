/* The pure parts of the USB adapter feature (usbnet.h), apart from usbnet.c so the host self-test
 * (payload/tests/usbnet_selftest.c) runs them without the console's USB headers. */
#include "usbnet.h"

#include <stdio.h>
#include <string.h>

/* The vendor's own IDs, as the daemon drives them (payload/usbnet/README.md). An OEM ID is not
 * guessed at: an adapter that reports another vendor or product is listed in "seen" instead. */
enum usbnet_chip_id usbnet_supported(uint16_t vid, uint16_t pid) {
    if (vid != 0x0bda) return USBNET_CHIP_NONE;
    if (pid == 0x8156) return USBNET_CHIP_8156B;
    if (pid == 0x8157) return USBNET_CHIP_8157;
    if (pid == 0x815a) return USBNET_CHIP_8159;
    return USBNET_CHIP_NONE;
}

const char *usbnet_chip_name(enum usbnet_chip_id chip) {
    switch (chip) {
    case USBNET_CHIP_8156B: return "RTL8156B";
    case USBNET_CHIP_8157: return "RTL8157";
    case USBNET_CHIP_8159: return "RTL8159";
    default: return "";
    }
}

unsigned usbnet_chip_max_mbps(enum usbnet_chip_id chip) {
    switch (chip) {
    case USBNET_CHIP_8156B: return 2500;
    case USBNET_CHIP_8157: return 5000;
    case USBNET_CHIP_8159: return 10000;
    default: return 0;
    }
}

static int number(const char **p, int max, int *out) {
    int v = 0, digits = 0;
    while (**p >= '0' && **p <= '9') {
        if (digits && v == 0) return -1; /* no leading zero */
        v = v * 10 + (**p - '0');
        if (v > max) return -1;
        (*p)++;
        digits++;
    }
    if (!digits) return -1;
    *out = v;
    return 0;
}

int usbnet_parse_ugen(const char *name, int *bus, int *addr) {
    const char *p = name;
    if (strncmp(p, "ugen", 4) != 0) return -1;
    p += 4;
    if (number(&p, 255, bus) != 0 || *p++ != '.') return -1;
    if (number(&p, 255, addr) != 0 || *p) return -1;
    return 0;
}

int usbnet_ipv4_ok(const char *s) {
    const char *p = s;
    int i, v;
    if (!s || !*s) return 0;
    for (i = 0; i < 4; i++) {
        if (number(&p, 255, &v) != 0) return 0;
        if (i < 3 && *p++ != '.') return 0;
    }
    return *p == '\0';
}

/* Lines: "off" (or "on"), and "ip <a.b.c.d> <mask> <gateway>" with the gateway optional. Anything
 * else is malformed, and the defaults stand. */
int usbnet_parse_settings(const char *text, int *enabled, char ip[16], char mask[16], char gw[16]) {
    char line[160];
    const char *p = text;
    int bad = 0;
    *enabled = 1;
    ip[0] = mask[0] = gw[0] = '\0';
    while (p && *p) {
        size_t n = strcspn(p, "\n");
        char a[32] = "", m[32] = "", g[32] = "", word[16] = "";
        int k;
        if (n >= sizeof line) n = sizeof line - 1;
        memcpy(line, p, n);
        line[n] = '\0';
        p += strcspn(p, "\n");
        if (*p) p++;
        if (n && line[n - 1] == '\r') line[n - 1] = '\0';
        if (!line[0]) continue;
        k = sscanf(line, "%15s %31s %31s %31s", word, a, m, g);
        if (k == 1 && !strcmp(word, "off")) *enabled = 0;
        else if (k == 1 && !strcmp(word, "on")) *enabled = 1;
        else if (k >= 3 && !strcmp(word, "ip") && usbnet_ipv4_ok(a) && usbnet_ipv4_ok(m) && (k == 3 || usbnet_ipv4_ok(g))) {
            snprintf(ip, 16, "%s", a);
            snprintf(mask, 16, "%s", m);
            snprintf(gw, 16, "%s", k == 4 ? g : "");
        } else bad = 1;
    }
    if (bad) {
        /* An "off" line still counts: a damaged file never turns the feature back on. */
        ip[0] = mask[0] = gw[0] = '\0';
        return -1;
    }
    return 0;
}
