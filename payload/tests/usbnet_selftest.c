/* The USB Ethernet adapter's pure parts on the host (payload/usbnet/README.md):
 *   - the helper's: supported IDs, /dev/ugen names, the settings file, IPv4 checks (usbnet_util.c);
 *   - the daemon's codecs: descriptors (rtl.c, ncm.c), the receive aggregate walk and the transmit
 *     descriptor (the ring of bulk transfers carries exactly these), register requests, the PHY status;
 *   - the link debounce and the state the daemon reports (link.c);
 *   - the PROXY line the daemon writes and Minion's server reads (link.c, webui_http.c).
 * The configuration fixtures are real captures of an RTL8159 adapter, taken from tom-ps5-usbnet's
 * tests (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later, tests/mock_descriptors.h). */
#include "usbnet.h"
#include "webui_http.h"

#include "../usbnet/fwd.h"
#include "../usbnet/link.h"
#include "../usbnet/ncm.h"
#include "../usbnet/rtl.h"
#include "usbnet_proto.h"

#include <arpa/inet.h>
#include <stdio.h>
#include <string.h>

static int failures;
static void check(int ok, const char *label) {
    printf("  %s %s\n", ok ? "PASS" : "FAIL", label);
    if (!ok) failures++;
}

static const uint8_t captured_ecm[] = {
    0x09,0x02,0x62,0x00,0x02,0x03,0x00,0xa0,0x44,0x09,0x04,0x00,0x00,0x01,0x02,0x06,
    0x00,0x05,0x05,0x24,0x00,0x10,0x01,0x05,0x24,0x06,0x00,0x01,0x0d,0x24,0x0f,0x03,
    0x1f,0x50,0x31,0x00,0xee,0x05,0x00,0x80,0x00,0x07,0x05,0x83,0x03,0x10,0x00,0x0b,
    0x06,0x30,0x00,0x00,0x08,0x00,0x09,0x04,0x01,0x00,0x00,0x0a,0x00,0x00,0x00,0x09,
    0x04,0x01,0x01,0x02,0x0a,0x00,0x00,0x04,0x07,0x05,0x81,0x02,0x00,0x04,0x00,0x06,
    0x30,0x06,0x00,0x00,0x00,0x07,0x05,0x02,0x02,0x00,0x04,0x00,0x06,0x30,0x06,0x00,
    0x00,0x00,
};
static const uint8_t captured_ssp[] = {
    0x09,0x02,0x68,0x00,0x02,0x02,0x00,0xa0,0x44,0x09,0x04,0x00,0x00,0x01,0x02,0x0d,
    0x00,0x05,0x05,0x24,0x00,0x10,0x01,0x05,0x24,0x06,0x00,0x01,0x0d,0x24,0x0f,0x03,
    0x1f,0x50,0x31,0x00,0xee,0x05,0x00,0x80,0x00,0x06,0x24,0x1a,0x00,0x01,0x2b,0x07,
    0x05,0x83,0x03,0x10,0x00,0x0b,0x06,0x30,0x00,0x00,0x08,0x00,0x09,0x04,0x01,0x00,
    0x00,0x0a,0x00,0x01,0x00,0x09,0x04,0x01,0x01,0x02,0x0a,0x00,0x01,0x04,0x07,0x05,
    0x81,0x02,0x00,0x04,0x00,0x06,0x30,0x06,0x00,0x00,0x00,0x07,0x05,0x02,0x02,0x00,
    0x04,0x00,0x06,0x30,0x06,0x00,0x00,0x00,
};
static const uint8_t captured_vendor_hs[] = {
    0x09,0x02,0x27,0x00,0x01,0x01,0x00,0xa0,0xfa,0x09,0x04,0x00,0x00,0x03,0xff,0xff,
    0x00,0x00,0x07,0x05,0x81,0x02,0x00,0x02,0x00,0x07,0x05,0x02,0x02,0x00,0x02,0x00,
    0x07,0x05,0x83,0x03,0x10,0x00,0x0b,
};

static void test_helper_parts(void) {
    int bus = -1, addr = -1, on;
    char ip[16], mask[16], gw[16];
    printf("helper: IDs, nodes, settings\n");
    check(usbnet_supported(0x0bda, 0x8156) == USBNET_CHIP_8156B, "0bda:8156 is the RTL8156B family");
    check(usbnet_supported(0x0bda, 0x8157) == USBNET_CHIP_8157, "0bda:8157 is the RTL8157");
    check(usbnet_supported(0x0bda, 0x815a) == USBNET_CHIP_8159, "0bda:815a is the RTL8159");
    check(usbnet_supported(0x0bda, 0x8153) == USBNET_CHIP_NONE, "the gigabit RTL8153 is not driven");
    check(usbnet_supported(0x2357, 0x8156) == USBNET_CHIP_NONE, "an OEM vendor ID is not guessed at");
    check(usbnet_chip_max_mbps(USBNET_CHIP_8157) == 5000 && !strcmp(usbnet_chip_name(USBNET_CHIP_8159), "RTL8159"),
          "names and top rates");
    check(usbnet_parse_ugen("ugen0.3", &bus, &addr) == 0 && bus == 0 && addr == 3, "ugen0.3");
    check(usbnet_parse_ugen("ugen12.31", &bus, &addr) == 0 && bus == 12 && addr == 31, "ugen12.31");
    check(usbnet_parse_ugen("ugen0.3.1", &bus, &addr) != 0 && usbnet_parse_ugen("ugen.3", &bus, &addr) != 0 &&
              usbnet_parse_ugen("usb", &bus, &addr) != 0 && usbnet_parse_ugen("ugen01.2", &bus, &addr) != 0,
          "endpoint nodes, bad names and leading zeros are not devices");
    check(usbnet_ipv4_ok("192.168.1.20") && !usbnet_ipv4_ok("192.168.1") && !usbnet_ipv4_ok("192.168.1.256") &&
              !usbnet_ipv4_ok("192.168.01.2") && !usbnet_ipv4_ok(" 1.2.3.4") && !usbnet_ipv4_ok(""),
          "IPv4 addresses, nothing else");
    check(usbnet_parse_settings("", &on, ip, mask, gw) == 0 && on && !ip[0], "no file: on, DHCP");
    check(usbnet_parse_settings("off\n", &on, ip, mask, gw) == 0 && !on, "off");
    check(usbnet_parse_settings("ip 10.0.0.9 255.255.255.0 10.0.0.1\n", &on, ip, mask, gw) == 0 && on &&
              !strcmp(ip, "10.0.0.9") && !strcmp(mask, "255.255.255.0") && !strcmp(gw, "10.0.0.1"),
          "a static address with a gateway");
    check(usbnet_parse_settings("ip 10.0.0.9 255.255.0.0\r\n", &on, ip, mask, gw) == 0 && !gw[0], "no gateway, CRLF");
    check(usbnet_parse_settings("off\nip 10.0.0.300 255.0.0.0\n", &on, ip, mask, gw) != 0 && !on && !ip[0],
          "a bad line: DHCP, and off stays off");
}

static void test_descriptors(void) {
    struct ncm_function f;
    struct rtl_usb_interface iface;
    struct rtl_usb_device dev = {0x0bda, 0x8156, 0, 0, 0};
    enum rtl_chip chip;
    uint8_t mac[6], s[26];
    static const char hex[] = "00E04C680001";
    int i;
    printf("daemon: descriptors\n");
    check(ncm_find_ecm_function(captured_ecm, sizeof captured_ecm, &f) == NCM_OK && f.ep_in == 0x81 && f.ep_out == 0x02 &&
              f.ep_notify == 0x83 && f.mac_string == 3 && f.data_iface == 1,
          "the ECM configuration: endpoints and the MAC string index");
    check(ncm_find_function(captured_ssp, sizeof captured_ssp, &f) == NCM_OK && f.in_mps == 1024, "the NCM configuration");
    check(ncm_find_ecm_function(captured_vendor_hs, sizeof captured_vendor_hs, &f) != NCM_OK,
          "the vendor configuration has no ECM function");
    check(rtl_parse_config(captured_vendor_hs, sizeof captured_vendor_hs, 0, 0, &iface) == 0 && iface.class_code == 0xff &&
              iface.bulk_in == 0x81 && iface.bulk_out == 0x02,
          "the vendor configuration's bulk pair (the one the daemon picks)");
    check(rtl_parse_config(captured_vendor_hs, sizeof captured_vendor_hs - 1, 0, 0, &iface) != 0, "a cut descriptor is refused");
    check(rtl_identify_target(&dev, 0x7410, &chip) == 0 && chip == RTL_8156B, "8156 + version 0x7410: RTL8156B");
    check(rtl_identify_target(&dev, 0x7030, &chip) != 0, "the first RTL8156 (0x7030) is not driven");
    dev.pid = 0x815a;
    check(rtl_identify_target(&dev, 0x2020, &chip) == 0 && chip == RTL_8159, "815a + 0x2020: RTL8159");
    dev.pid = 0x8157;
    check(rtl_identify_target(&dev, 0x1030, &chip) == 0 && chip == RTL_8157, "8157 + 0x1030: RTL8157");
    dev.vid = 0x2357;
    check(rtl_identify_target(&dev, 0x1030, &chip) != 0, "an OEM vendor fails closed");
    s[0] = 26;
    s[1] = 3;
    for (i = 0; i < 12; i++) {
        s[2 + 2 * i] = (uint8_t)hex[i];
        s[3 + 2 * i] = 0;
    }
    check(ncm_parse_mac_string(s, sizeof s, mac) == NCM_OK && mac[0] == 0x00 && mac[1] == 0xe0 && mac[5] == 0x01,
          "the MAC string");
    s[4] = '1'; /* 01:e0:...: the group bit */
    check(ncm_parse_mac_string(s, sizeof s, mac) != NCM_OK, "a group address is refused");
}

static void test_aggregates(void) {
    uint8_t a[2][100], b[2][64], buf[4096], h[RTL_MAX_TX_HEADER];
    struct rtl_frame frames[2] = {{a[0], 100}, {b[0], 64}};
    struct rtl_frame_view views[8];
    struct rtl_write w;
    uint8_t setup[8], reply[4] = {0, 0, 0x20, 0x20};
    uint16_t version;
    size_t written = 0, count = 0;
    enum rtl_chip chips[3] = {RTL_8156B, RTL_8157, RTL_8159};
    int c;
    printf("daemon: bulk aggregates and registers\n");
    memset(a, 0xa5, sizeof a);
    memset(b, 0x5a, sizeof b);
    for (c = 0; c < 3; c++) {
        char label[96];
        int ok = rtl_rx_fixture_encode(chips[c], frames, 2, buf, sizeof buf, &written) == 0 &&
                 rtl_rx_scan(chips[c], buf, written, views, 8, &count) == 0 && count == 2 && views[0].len == 100 &&
                 views[1].len == 64 && !memcmp(views[1].data, b[0], 64);
        snprintf(label, sizeof label, "chip %d: two frames in one bulk IN transfer come out whole, FCS off", (int)chips[c]);
        check(ok, label);
        ok = rtl_rx_scan(chips[c], buf, written - 10, views, 8, &count) != 0 && count == 1;
        snprintf(label, sizeof label, "chip %d: a cut aggregate still gives the frames before the cut", (int)chips[c]);
        check(ok, label);
    }
    check(rtl_tx_header(RTL_8156B, h, 60) == 8 && rtl_tx_header(RTL_8159, h, 60) == 16 && rtl_tx_header(RTL_8157, h, 59) < 0,
          "transmit descriptors: 8 bytes on the RTL8156B, 16 on the others, no frame under 60");
    check(rtl_tx_encode(RTL_8159, frames, 2, buf, sizeof buf, &written) == 0 &&
              rtl_tx_decode(RTL_8159, buf, written, views, 8, &count) == 0 && count == 2,
          "a transmit aggregate decodes back");
    check(rtl_write_request(&w, RTL_MCU_PLA, 0xe813, 1, 0x0c) == 0 && w.setup[0] == 0x40 && w.setup[2] == 0x10 &&
              w.setup[3] == 0xe8 && w.data[3] == 0x0c,
          "a one-byte register write goes out shifted into its dword");
    check(rtl_write_request(&w, RTL_MCU_PLA, 0xe813, 2, 1) != 0, "an unaligned word write is refused");
    check(rtl_version_request(setup) == 0 && setup[0] == 0xc0 && rtl_version_reply(reply, 4, &version) == 0 &&
              version == 0x2020,
          "the version request and reply");
    check(rtl_phy_link(0x0402) && rtl_phy_mbps(0x0402) == 2500 && rtl_phy_mbps(0x1002) == 5000 &&
              rtl_phy_mbps(0x4002) == 10000 && rtl_phy_mbps(0x0012) == 1000 && !rtl_phy_link(0x0400),
          "PHY status: link bit and rate");
}

static void test_link(void) {
    struct usbn_link s;
    memset(&s, 0, sizeof s);
    printf("daemon: link and state\n");
    check(usbn_link_phy(&s, 1, 0x0402) == 1 && s.up && s.mbps == 2500, "link up at 2.5G");
    check(!strcmp(usbn_link_state(&s, 0, 1000), USBNET_ST_NO_ADDRESS), "no address yet");
    check(!strcmp(usbn_link_state(&s, 1, 2000), USBNET_ST_READY), "an address: ready");
    check(usbn_link_phy(&s, 0, 0) == 0 && usbn_link_phy(&s, 0, 0) == 0 && s.up, "two unreadable status reads: still up");
    check(usbn_link_phy(&s, 0, 0) == 1 && !s.up, "the third: down");
    check(!strcmp(usbn_link_state(&s, 1, 3000), USBNET_ST_READY), "down for 0 s: still ready (a blink is not a switch)");
    check(!strcmp(usbn_link_state(&s, 1, 5999), USBNET_ST_READY), "down for 2.999 s: still ready");
    check(!strcmp(usbn_link_state(&s, 1, 6000), USBNET_ST_NO_LINK), "down for 3 s: no link (transfers use the console)");
    check(usbn_link_phy(&s, 1, 0x0402) == 1 && !strcmp(usbn_link_state(&s, 1, 6500), USBNET_ST_READY),
          "back up with an address: ready at once");
    check(usbn_link_phy(&s, 1, 0x1002) == 2 && s.mbps == 5000, "a new rate is a change of its own");
    check(s.changes == 3, "three up/down changes counted");
}

static void test_proxy(void) {
    char line[USBNET_PROXY_LINE_MAX + 1];
    uint32_t client = 0, peer;
    size_t n;
    int r;
    printf("Minion's PROXY line\n");
    inet_pton(AF_INET, "192.168.1.77", &peer);
    n = fwd_proxy_line(line, sizeof line, peer, 51234, "192.168.1.50", 9180);
    check(n > 0 && !strcmp(line, "PROXY TCP4 192.168.1.77 192.168.1.50 51234 9180\r\n"), "the daemon writes it");
    r = webui_proxy_line(line, n, &client);
    check(r == (int)n && client == 0xC0A8014Du, "Minion reads the client from it");
    check(webui_proxy_line(line, 9, &client) == -1, "half a line: wait for the rest");
    check(webui_proxy_line("GET / HTTP/1.1\r\n", 16, &client) == 0, "a plain request is not one");
    check(webui_proxy_line("PROXY TCP4 1.2.3\r\n", 18, &client) == -2, "a bad address is refused");
    check(webui_proxy_line("PROXY TCP6 ::1 ::1 1 2\r\n", 24, &client) == 0, "TCP6 is not ours: not a PROXY line here");
}

int main(void) {
    test_helper_parts();
    test_descriptors();
    test_aggregates();
    test_link();
    test_proxy();
    printf("%s (%d failed)\n", failures ? "FAILED" : "ok", failures);
    return failures ? 1 : 0;
}
