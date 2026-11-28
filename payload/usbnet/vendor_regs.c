/* SPDX-License-Identifier: GPL-3.0-or-later
 * Adapted from tom-ps5-usbnet (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later),
 * src/vendor_regs.c, for PS5Upload's USB adapter daemon (payload/usbnet/README.md). */
/* See vendor.h. r8156_init(), rtl8156_up() and rtl8156_enable() from Linux's
 * r8152.c, in their order: for RTL_VER_17 on the RTL8159, RTL_VER_16 on the RTL8157,
 * and RTL_VER_12, 13 and 15 on the RTL8156B. The RTL8157 takes the RTL8159's path but
 * for three things: USB_CTRL bit 11 stays, there is no backup-restore wait, and its
 * receive buffer is 32 KiB. Left out on all of them:
 *
 *   - the PHY: no firmware, no SRAM parameters and no OCP register. In the vendor
 *     configuration PLA_PHYSTATUS already showed a 10 Gbit/s link, so the chip's own
 *     flash brought the PHY up, and Linux goes on without the firmware file too.
 *   - suspend, wake-on-LAN and clock speed-down, which only matter when idle.
 *
 * All the writes are in the tables below, and none reaches the PHY, the flash or the
 * USB descriptors. Replugging the adapter undoes all of it.
 *
 * The MAC address is compared and, if it differs, written. It is never printed. */
#include "adapter.h"
#include "usbn_log.h"
#include "vendor.h"

#include <sys/ioctl.h>

#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define PLA RTL_MCU_PLA
#define USB RTL_MCU_USB

#define PLA_IDR 0xc000
#define PLA_RCR 0xc010
#define PLA_RCR1 0xc012
#define PLA_RMS 0xc016
#define PLA_RXFIFO_FULL 0xc0a2
#define PLA_RX_FIFO_FULL 0xc0a6
#define PLA_RX_FIFO_EMPTY 0xc0aa
#define PLA_FMC 0xc0b4
#define PLA_TEREDO_CFG 0xc0bc
#define PLA_MAR 0xcd00
#define PLA_TEREDO_TIMER 0xd2cc
#define PLA_REALWOW_TIMER 0xd2e8
#define PLA_EXTRA_STATUS 0xd398
#define PLA_GPHY_CTRL 0xd3ae
#define PLA_BOOT_CTRL 0xe004
#define PLA_EEEP_CR 0xe080
#define PLA_MAC_PWR_CTRL3 0xe0cc
#define PLA_MAC_PWR_CTRL4 0xe0ce
#define PLA_WDT6_CTRL 0xe428
#define PLA_TCR1 0xe612
#define PLA_MTPS 0xe615
#define PLA_TXFIFO_CTRL 0xe618
#define PLA_TXFIFO_FULL 0xe61a
#define PLA_RSTTALLY 0xe800
#define PLA_CR 0xe813
#define PLA_CRWECR 0xe81c
#define PLA_OOB_CTRL 0xe84f
#define PLA_MISC_1 0xe85a
#define PLA_TALLY 0xe890
#define PLA_SFF_STS_7 0xe8de
#define PLA_POL_GPIO_CTRL 0xdc6a
#define PLA_CPCR 0xe854
#define USB_U2P3_CTRL 0xb460
#define USB_SPEED_OPTION 0xd32a
#define USB_FW_CTRL 0xd334
#define USB_FC_TIMER 0xd340
#define USB_FW_TASK 0xd4e8
#define USB_MSC_TIMER 0xcbfc
#define USB_LPM_CONFIG 0xcfd8
#define USB_ECM_OPTION 0xcfee
#define USB_MISC_2 0xcfff
#define USB_ECM_OP 0xd26b
#define USB_GPHY_CTRL 0xd284
#define USB_USB_CTRL 0xd406
#define USB_RX_BUF_TH 0xd40c
#define USB_RX_EARLY_TIMEOUT 0xd42c
#define USB_RX_EARLY_SIZE 0xd42e
#define USB_RX_EXTRA_AGGR_TMR 0xd432
#define USB_UPT_RXDMA_OWN 0xd437
#define USB_BMU_RESET 0xd4b0
#define USB_U1U2_TIMER 0xd4da
#define USB_RX_AGGR_NUM 0xd4ee
#define USB_ADV_ADDR 0xd5d6
#define USB_ADV_DATA 0xd5d8
#define USB_ADV_CMD 0xd5dc
#define USB_POWER_CUT 0xd80a
#define USB_MISC_0 0xd81a

#define BIT(n) (1u << (n))
#define RCR_ACPT_ALL 0x0fu
#define RCR_APM 0x02u
#define RCR_AM 0x04u
#define RCR_AB 0x08u
#define VLAN_STRIP (BIT(7) | BIT(6))
#define MTU 1500
#define RX_MAX_SIZE (MTU + 18 + 4)      /* mtu_to_size(): VLAN header and FCS */
#define RX_RESERVED (RX_MAX_SIZE + 16 + 16)
#define MTPS_JUMBO (12 * 1024 / 64)
#define FC_PAUSE_ON ((2048 + 6 * 1024) / 16)
#define FC_PAUSE_OFF ((2048 + 14 * 1024) / 16)
#define TX_FIFO_FULL (2048 / 16)        /* ALIGN(rx_max_size + 16, 1024) / 16 */
#define EARLY_TIMEOUT (640 / 8)
#define AUTOLOAD_DONE 0x0002u
#define GPHY_FLASH BIT(1)
#define GPHY_PATCH_DONE BIT(2)
#define BYPASS_FLASH BIT(5)
#define BACKUP_RESTORE BIT(6)
#define PCUT_STATUS 0x0001u
#define CUR_LINK_OK BIT(15)
#define POLL_LINK_CHG BIT(0)
#define CR_TE 0x04u
#define CR_RE 0x08u
#define CR_RST 0x10u
#define SLOT_EN BIT(11)
#define FLOW_CTRL_EN BIT(0)
#define CTRL_TIMER_EN BIT(15)
#define DACK_DET_EN BIT(15)
#define FLOW_CTRL_PATCH_2 BIT(8)
#define AUTO_SPEEDUP BIT(3)
#define FC_PATCH_TASK BIT(1)
#define RX_DETECT8 BIT(3)
#define U2P3_ENABLE BIT(0)
#define RG_PWRDN_EN BIT(8)
#define ALL_SPEED_OFF BIT(9)
#define IDLE_SPDWN_EN BIT(6)
#define RX_AGG_DISABLE 0x0010u
#define RX_ZERO_EN 0x0080u
#define VER_12 0x7400                   /* the one RTL8156B without the flash wait */
#define RESET_TRIES 1000
#define RESET_POLL_US 250
#define RXDY_GATED 0x0008u
#define ADV_BUSY BIT(0)
#define ADV_WRITE BIT(1)
#define BMU_RX_ZERO_LEN 0x2300
#define POLL_US 2000
#define POLL_TRIES 50
#define CONTROL_TIMEOUT_MS 1000

struct step {
    const char *name;
    uint16_t mcu, address;
    uint8_t width, always;              /* always: write even if the value would not change */
    uint32_t clear, set;
};

/* r8156_init(), RTL_VER_16 and 17, up to USB_CTRL */
static const struct step g_init_v2[] = {
    {"undocumented USB 0xcffe", USB, 0xcffe, 1, 0, 0, BIT(3)},
    {"undocumented USB 0xd3ca", USB, 0xd3ca, 1, 0, BIT(0), 0},
    {"ECM_OP.EN_ALL_SPEED", USB, USB_ECM_OP, 1, 0, BIT(0), 0},
    {"ECM_OPTION.BYPASS_MAC_RESET", USB, USB_ECM_OPTION, 2, 0, 0, BIT(5)},
    {"LPM_CONFIG.U1U2_EN", USB, USB_LPM_CONFIG, 2, 0, BIT(0), 0},
    {"MSC_TIMER", USB, USB_MSC_TIMER, 2, 0, 0xffff, 0x0fff},
    {"U1U2_TIMER", USB, USB_U1U2_TIMER, 2, 0, 0xffff, 500},
    {"POWER_CUT.PWR_EN", USB, USB_POWER_CUT, 2, 0, BIT(0), 0},
    {"MISC_0.PCUT_STATUS", USB, USB_MISC_0, 2, 0, PCUT_STATUS, 0},
    {"MISC_2 bit 1", USB, USB_MISC_2, 1, 0, BIT(1), 0},
    {"POWER_CUT.UPS", USB, USB_POWER_CUT, 1, 0, BIT(4) | BIT(5), 0},
    {"MISC_2.UPS_FORCE_PWR_DOWN", USB, USB_MISC_2, 1, 0, BIT(0), 0},
    {"EXTRA_STATUS.POLL_LINK_CHG", PLA, PLA_EXTRA_STATUS, 2, 0, 0, POLL_LINK_CHG},
};

/* BIT(11) is the RTL8159's alone, "with unknown meaning" in Linux. */
static const struct step g_usb_ctrl_8159[] = {
    {"USB_CTRL: aggregate, long descriptors", USB, USB_USB_CTRL, 2, 0, 0x0010 | 0x0400 | BIT(11), 0},
};
static const struct step g_usb_ctrl_8157[] = {
    {"USB_CTRL: aggregate, long descriptors", USB, USB_USB_CTRL, 2, 0, 0x0010 | 0x0400, 0},
};

/* The rest of r8156_init(), RTL_VER_16 and 17 */
static const struct step g_init_v2_tail[] = {
    {"undocumented USB 0xd4ae: TX signature", USB, 0xd4ae, 1, 0, BIT(1), 0},
    {"reset tally", PLA, PLA_RSTTALLY, 2, 0, 0, BIT(0)},
};

/* r8156_init(), RTL_VER_12 to 15, up to USB_FW_CTRL. The U1/U2 and U2P3 power states
 * stay off: Linux turns them back on, and they only save power. */
static const struct step g_init_8156b[] = {
    {"ECM_OP.EN_ALL_SPEED", USB, USB_ECM_OP, 1, 0, BIT(0), 0},
    {"SPEED_OPTION", USB, USB_SPEED_OPTION, 2, 0, 0xffff, 0},
    {"ECM_OPTION.BYPASS_MAC_RESET", USB, USB_ECM_OPTION, 2, 0, 0, BIT(5)},
    {"U2P3_CTRL.RX_DETECT8", USB, USB_U2P3_CTRL, 2, 0, 0, RX_DETECT8},
    {"LPM_CONFIG.U1U2_EN", USB, USB_LPM_CONFIG, 2, 0, BIT(0), 0},
    {"U2P3_CTRL.U2P3_ENABLE", USB, USB_U2P3_CTRL, 2, 0, U2P3_ENABLE, 0},
    {"MSC_TIMER", USB, USB_MSC_TIMER, 2, 0, 0xffff, 0x0fff},
    {"U1U2_TIMER", USB, USB_U1U2_TIMER, 2, 0, 0xffff, 500},
    {"POWER_CUT.PWR_EN", USB, USB_POWER_CUT, 2, 0, BIT(0), 0},
    {"MISC_0.PCUT_STATUS", USB, USB_MISC_0, 2, 0, PCUT_STATUS, 0},
    {"POWER_CUT.UPS", USB, USB_POWER_CUT, 1, 0, BIT(4) | BIT(5), 0},
    {"MISC_2.UPS_FORCE_PWR_DOWN", USB, USB_MISC_2, 1, 0, BIT(0), 0},
    /* Linux clears SLOT_EN alone; the filter goes shut here as well, which rtl8156_up()
     * does anyway, so that nothing is taken in before a transfer waits for it. */
    {"RCR.SLOT_EN, accept nothing", PLA, PLA_RCR, 2, 0, SLOT_EN | RCR_ACPT_ALL, 0},
    {"CPCR.FLOW_CTRL_EN", PLA, PLA_CPCR, 2, 0, 0, FLOW_CTRL_EN},
    {"FC_TIMER: 600 ms", USB, USB_FC_TIMER, 2, 0, 0xffff, CTRL_TIMER_EN | (600 / 8)},
};

/* The rest of r8156_init(), RTL_VER_12 to 15, after USB_FW_CTRL. */
static const struct step g_init_8156b_tail[] = {
    {"FW_TASK.FC_PATCH_TASK", USB, USB_FW_TASK, 2, 0, 0, FC_PATCH_TASK},
    {"MAC_PWR_CTRL3.MCU_SPDWN_EN", PLA, PLA_MAC_PWR_CTRL3, 2, 0, BIT(14), 0},
    {"EXTRA_STATUS.POLL_LINK_CHG", PLA, PLA_EXTRA_STATUS, 2, 0, 0, POLL_LINK_CHG},
    {"USB_CTRL: aggregate, no zero length", USB, USB_USB_CTRL, 2, 0, RX_AGG_DISABLE | RX_ZERO_EN, 0},
    {"reset tally", PLA, PLA_RSTTALLY, 2, 0, 0, BIT(0)},
};

/* rtl8156_up(), up to rtl8152_nic_reset() */
static const struct step g_up_stop[] = {
    {"MISC_1.RXDY_GATED on", PLA, PLA_MISC_1, 2, 0, 0, RXDY_GATED},
    {"TEREDO_CFG", PLA, PLA_TEREDO_CFG, 1, 1, 0xff, 0xff},
    {"WDT6_CTRL", PLA, PLA_WDT6_CTRL, 2, 0, 0xffff, 0x0010},
    {"REALWOW_TIMER", PLA, PLA_REALWOW_TIMER, 2, 0, 0xffff, 0},
    {"TEREDO_TIMER", PLA, PLA_TEREDO_TIMER, 4, 0, 0xffffffffu, 0},
    {"RCR: accept nothing", PLA, PLA_RCR, 4, 0, RCR_ACPT_ALL, 0},
};

/* rtl8152_nic_reset() for RTL_VER_16 and 17; the RTL8156B's is reset_mac(). */
static const struct step g_stop_v2[] = {
    {"CR: stop", PLA, PLA_CR, 1, 0, CR_RE | CR_TE, 0},
};

/* rtl_reset_bmu() */
static const struct step g_bmu_reset[] = {
    {"BMU_RESET: assert", USB, USB_BMU_RESET, 1, 0, 0x03, 0},
    {"BMU_RESET: release", USB, USB_BMU_RESET, 1, 0, 0, 0x03},
};

/* the rest of rtl8156_up(), then rtl8156_enable(), RTL_VER_16 and 17. EEE+ stays off:
 * Linux turns it on for the RTL8157 only at 10 Mbit/s. */
static const struct step g_up_enable_v2[] = {
    {"OOB_CTRL.NOW_IS_OOB", PLA, PLA_OOB_CTRL, 1, 0, 0x80, 0},
    {"RCR1 bit 3", PLA, PLA_RCR1, 2, 0, BIT(3), 0},
    {"SFF_STS_7.MCU_BORW_EN", PLA, PLA_SFF_STS_7, 2, 0, 0x4000, 0},
    {"RCR1: no VLAN stripping", PLA, PLA_RCR1, 2, 0, VLAN_STRIP, 0},
    {"RMS", PLA, PLA_RMS, 2, 0, 0xffff, RX_MAX_SIZE},
    {"MTPS", PLA, PLA_MTPS, 1, 0, 0xff, MTPS_JUMBO},
    {"RX_FIFO_FULL", PLA, PLA_RX_FIFO_FULL, 2, 0, 0xffff, FC_PAUSE_ON},
    {"RX_FIFO_EMPTY", PLA, PLA_RX_FIFO_EMPTY, 2, 0, 0xffff, FC_PAUSE_OFF},
    {"TXFIFO_CTRL", PLA, PLA_TXFIFO_CTRL, 2, 0, 0xffff, 512 / 64},
    {"TXFIFO_FULL", PLA, PLA_TXFIFO_FULL, 2, 0, 0xffff, TX_FIFO_FULL},
    {"RXFIFO_FULL: share FIFO", PLA, PLA_RXFIFO_FULL, 2, 0, 0x0fff, 0x08},
    {"MAC_PWR_CTRL3.MCU_SPDWN_EN", PLA, PLA_MAC_PWR_CTRL3, 2, 0, BIT(14), 0},
    {"RX_BUF_TH", USB, USB_RX_BUF_TH, 4, 0, 0xffffffffu, 0x00600400},
    {"EEEP_CR.EEEP_TX", PLA, PLA_EEEP_CR, 2, 0, 0x0002, 0},
    {"RX_AGGR_NUM", USB, USB_RX_AGGR_NUM, 2, 0, 0x01ff, 0},
    {"RX_EARLY_TIMEOUT", USB, USB_RX_EARLY_TIMEOUT, 2, 0, 0xffff, EARLY_TIMEOUT},
    {"TCR1: 96 ns gap", PLA, PLA_TCR1, 2, 0, BIT(3) | BIT(9) | BIT(8), BIT(9) | BIT(8)},
    {"MAC_PWR_CTRL4.TX10MIDLE_EN", PLA, PLA_MAC_PWR_CTRL4, 2, 0, 0, 0x0100},
};

/* the rest of rtl8156_up(), then rtl8156_enable() up to the early size, RTL_VER_12 to
 * 15. Idle speed-down stays off whatever the rate: Linux leaves it off at 2.5 Gbit/s. */
static const struct step g_up_enable_8156b[] = {
    {"OOB_CTRL.NOW_IS_OOB", PLA, PLA_OOB_CTRL, 1, 0, 0x80, 0},
    {"SFF_STS_7.MCU_BORW_EN", PLA, PLA_SFF_STS_7, 2, 0, 0x4000, 0},
    {"RCR1: no VLAN stripping", PLA, PLA_RCR1, 2, 0, VLAN_STRIP, 0},
    {"RMS", PLA, PLA_RMS, 2, 0, 0xffff, RX_MAX_SIZE},
    {"MTPS", PLA, PLA_MTPS, 1, 0, 0xff, MTPS_JUMBO},
    {"RX_FIFO_FULL", PLA, PLA_RX_FIFO_FULL, 2, 0, 0xffff, FC_PAUSE_ON},
    {"RX_FIFO_EMPTY", PLA, PLA_RX_FIFO_EMPTY, 2, 0, 0xffff, FC_PAUSE_OFF},
    {"TXFIFO_CTRL", PLA, PLA_TXFIFO_CTRL, 2, 0, 0xffff, 512 / 64},
    {"TXFIFO_FULL", PLA, PLA_TXFIFO_FULL, 2, 0, 0xffff, TX_FIFO_FULL},
    {"RXFIFO_FULL: share FIFO", PLA, PLA_RXFIFO_FULL, 2, 0, 0x0fff, 0x08},
    {"MAC_PWR_CTRL3.MCU_SPDWN_EN", PLA, PLA_MAC_PWR_CTRL3, 2, 0, BIT(14), 0},
    {"SPEED_OPTION: no power-down", USB, USB_SPEED_OPTION, 2, 0, RG_PWRDN_EN | ALL_SPEED_OFF, 0},
    {"RX_BUF_TH", USB, USB_RX_BUF_TH, 4, 0, 0xffffffffu, 0x00600400},
    {"EEEP_CR.EEEP_TX", PLA, PLA_EEEP_CR, 2, 0, 0x0002, 0},
    {"RX_AGGR_NUM", USB, USB_RX_AGGR_NUM, 2, 0, 0x01ff, 0},
    {"RX_EARLY_TIMEOUT", USB, USB_RX_EARLY_TIMEOUT, 2, 0, 0xffff, EARLY_TIMEOUT},
    {"TCR1: 96 ns gap", PLA, PLA_TCR1, 2, 0, BIT(3) | BIT(9) | BIT(8), BIT(9) | BIT(8)},
    {"MAC_PWR_CTRL4.TX10MIDLE_EN", PLA, PLA_MAC_PWR_CTRL4, 2, 0, 0, 0x0100},
    {"MAC_PWR_CTRL4.IDLE_SPDWN_EN", PLA, PLA_MAC_PWR_CTRL4, 2, 0, IDLE_SPDWN_EN, 0},
};

/* The end of rtl8156_enable(), RTL_VER_12 to 15: the flow-control task restarts. */
static const struct step g_fc_patch_off[] = {
    {"FW_TASK.FC_PATCH_TASK off", USB, USB_FW_TASK, 2, 0, FC_PATCH_TASK, 0},
};
static const struct step g_fc_patch_on[] = {
    {"FW_TASK.FC_PATCH_TASK on", USB, USB_FW_TASK, 2, 0, 0, FC_PATCH_TASK},
};

/* rtl_enable() */
static const struct step g_enable[] = {
    {"FMC.MCU_EN off", PLA, PLA_FMC, 2, 0, BIT(0), 0},
    {"FMC.MCU_EN on", PLA, PLA_FMC, 2, 0, 0, BIT(0)},
    {"CR: receive and transmit", PLA, PLA_CR, 1, 0, 0, CR_RE | CR_TE},
    {"UPT_RXDMA_OWN", USB, USB_UPT_RXDMA_OWN, 1, 1, 0xff, 0x03},
    {"MISC_1.RXDY_GATED off", PLA, PLA_MISC_1, 2, 0, RXDY_GATED, 0},
};

/* Every multicast group, because lwIP does not say which it wants. */
static const struct step g_filter[] = {
    {"MAR 0", PLA, PLA_MAR, 4, 1, 0xffffffffu, 0xffffffffu},
    {"MAR 1", PLA, PLA_MAR + 4, 4, 1, 0xffffffffu, 0xffffffffu},
    {"RCR: own, broadcast, multicast", PLA, PLA_RCR, 4, 0, RCR_ACPT_ALL, RCR_AB | RCR_APM | RCR_AM},
};

/* The start of rtl_disable(). */
static const struct step g_stop[] = {
    {"RCR: accept nothing", PLA, PLA_RCR, 4, 0, RCR_ACPT_ALL, 0},
    {"MISC_1.RXDY_GATED on", PLA, PLA_MISC_1, 2, 0, 0, RXDY_GATED},
    {"CR: stop", PLA, PLA_CR, 1, 0, CR_RE | CR_TE, 0},
};

static int g_fd = -1;
static int g_early_kib, g_coalesce_us;
static enum rtl_chip g_chip = RTL_8159;
static uint16_t g_version;

void vnd_regs_attach(int fd, enum rtl_chip chip, uint16_t version)
{
    g_fd = fd;
    g_chip = chip;
    g_version = version;
    g_early_kib = g_coalesce_us = 0;
}

unsigned vnd_regs_max_mbps(void)
{
    return g_chip == RTL_8156B ? 2500 : g_chip == RTL_8157 ? 5000 : 10000;
}

/* tp->rx_buf_sz, which the early size is taken from: 32 KiB on the RTL8157. */
static int early_kib(void)
{
    return g_chip == RTL_8157 ? 32 : VND_MAX_EARLY;
}

uint16_t vnd_regs_read_version(int fd)
{
    uint8_t setup[8], data[4];
    uint16_t version = 0;
    int tries;

    if (rtl_version_request(setup) != 0) return 0;
    for (tries = 0; tries < 3; tries++)         /* as __rtl_get_hw_ver() does */
        if (adapter_control_in(fd, setup, data) == 4 && rtl_version_reply(data, 4, &version) == 0) return version;
    return 0;
}

static int read_block(unsigned mcu, unsigned address, uint8_t *data, unsigned size)
{
    uint8_t setup[8];
    int got;

    if (rtl_read_request(setup, mcu, address & ~3u, size) != 0) return -1;
    vnd_lock();
    errno = 0;                          /* still 0 after a failure: the reply was short */
    got = adapter_control_in(g_fd, setup, data);
    vnd_unlock();
    return got == (int)size ? 0 : -1;
}

static int read_register(unsigned mcu, unsigned address, unsigned width, uint32_t *value)
{
    uint8_t data[4];
    uint32_t dword;

    if (read_block(mcu, address, data, 4) != 0) return -1;
    dword = (uint32_t)data[0] | (uint32_t)data[1] << 8 | (uint32_t)data[2] << 16 | (uint32_t)data[3] << 24;
    dword >>= (address & 3) * 8;
    *value = width == 4 ? dword : dword & ((1u << (width * 8)) - 1);
    return 0;
}

/* The only way to a host-to-device request. */
static int write_register(unsigned mcu, unsigned address, unsigned width, uint32_t value)
{
    struct rtl_write write;
    struct usb_ctl_request request;
    int result;

    if (rtl_write_request(&write, mcu, address, width, value) != 0) return -1;
    memset(&request, 0, sizeof request);
    memcpy(&request.ucr_request, write.setup, 8);
    request.ucr_data = write.data;
    vnd_lock();
    result = ioctl(g_fd, USB_DO_REQUEST, &request);
    vnd_unlock();
    return result;
}

static int apply(const struct step *s)
{
    uint32_t before, after, read_back = 0;

    if (read_register(s->mcu, s->address, s->width, &before) != 0) {
        say("%s: read failed, errno %d", s->name, errno);
        return -1;
    }
    after = (before & ~s->clear) | s->set;
    if (after == before && !s->always) return 0;
    if (write_register(s->mcu, s->address, s->width, after) != 0 ||
            read_register(s->mcu, s->address, s->width, &read_back) != 0) {
        say("%s: write failed, errno %d", s->name, errno);
        return -1;
    }
    return 0;
}

static int apply_all(const struct step *steps, size_t count)
{
    size_t i;

    for (i = 0; i < count; i++)
        if (apply(&steps[i]) != 0) return -1;
    checkpoint();
    return 0;
}

#define APPLY(table) apply_all(table, sizeof table / sizeof table[0])

static int wait_bits(unsigned mcu, unsigned address, uint32_t mask, uint32_t wanted)
{
    int i;

    for (i = 0; i < POLL_TRIES; i++) {
        uint32_t value;

        if (read_register(mcu, address, 2, &value) != 0) return -1;
        if ((value & mask) == wanted) return 0;
        usleep(POLL_US);
    }
    return -1;
}

/* rtl_bmu_clr_bits(): the BMU's registers sit behind a command register. */
static int clear_rx_zero_length(void)
{
    uint32_t value;

    if (wait_bits(USB, USB_ADV_CMD, ADV_BUSY, 0) != 0) return -1;
    checkpoint();
    if (write_register(USB, USB_ADV_ADDR, 2, BMU_RX_ZERO_LEN) != 0 || write_register(USB, USB_ADV_CMD, 2, ADV_BUSY) != 0 ||
            wait_bits(USB, USB_ADV_CMD, ADV_BUSY, 0) != 0 || read_register(USB, USB_ADV_DATA, 4, &value) != 0)
        return -1;
    if (!(value & BIT(3))) return 0;
    if (write_register(USB, USB_ADV_DATA, 4, value & ~BIT(3)) != 0 || write_register(USB, USB_ADV_ADDR, 2, BMU_RX_ZERO_LEN) != 0 ||
            write_register(USB, USB_ADV_CMD, 2, ADV_WRITE | ADV_BUSY) != 0)
        return -1;
    return wait_bits(USB, USB_ADV_CMD, ADV_BUSY, 0);
}

/* r8156b_wait_loading_flash() (not on RTL_VER_12), the autoload, and on the RTL8159
 * alone r8159_wait_backup_restore(). */
static void wait_for_chip(void)
{
    uint32_t gphy = 0, bypass = 0, misc = 0;

    if ((g_chip != RTL_8156B || g_version != VER_12) &&
            read_register(PLA, PLA_GPHY_CTRL, 2, &gphy) == 0 && read_register(USB, USB_GPHY_CTRL, 2, &bypass) == 0 &&
            (gphy & GPHY_FLASH) && !(bypass & BYPASS_FLASH) && wait_bits(USB, USB_GPHY_CTRL, GPHY_PATCH_DONE, GPHY_PATCH_DONE) != 0)
        say("  flash loading did not finish");
    if (wait_bits(PLA, PLA_BOOT_CTRL, AUTOLOAD_DONE, AUTOLOAD_DONE) != 0) say("  autoload did not finish");
    if (g_chip == RTL_8159 && read_register(USB, USB_MISC_0, 2, &misc) == 0 && (misc & PCUT_STATUS) &&
            wait_bits(USB, USB_GPHY_CTRL, BACKUP_RESTORE, BACKUP_RESTORE) != 0)
        say("  backup-restore did not finish");
}

/* The USB_FW_CTRL step of r8156_init(), RTL_VER_12 to 15: one bit depends on another
 * register. */
static int flow_control_patch(void)
{
    uint32_t gpio, value;
    struct step s = {"FW_CTRL", USB, USB_FW_CTRL, 2, 0, 0xffff, 0};

    if (read_register(PLA, PLA_POL_GPIO_CTRL, 2, &gpio) != 0 || read_register(USB, USB_FW_CTRL, 2, &value) != 0) {
        say("FW_CTRL: read failed, errno %d", errno);
        return -1;
    }
    if (!(gpio & DACK_DET_EN)) value |= FLOW_CTRL_PATCH_2;
    s.set = value & ~AUTO_SPEEDUP;
    return apply(&s);
}

/* rtl8152_nic_reset() for RTL_VER_12 to 15: the reset bit clears itself. */
static int reset_mac(void)
{
    uint32_t value = CR_RST;
    int i;

    checkpoint();
    if (write_register(PLA, PLA_CR, 1, CR_RST) != 0) {
        say("CR: reset failed, errno %d", errno);
        return -1;
    }
    for (i = 0; i < RESET_TRIES && (value & CR_RST); i++) {
        usleep(RESET_POLL_US);
        if (read_register(PLA, PLA_CR, 1, &value) != 0) return -1;
    }
    if (value & CR_RST) {
        say("CR: the reset did not finish");
        return -1;
    }
    return 0;
}

/* __rtl8152_set_mac_address(), in two writes. */
static int write_address(const uint8_t mac[6])
{
    uint32_t low = (uint32_t)mac[0] | (uint32_t)mac[1] << 8 | (uint32_t)mac[2] << 16 | (uint32_t)mac[3] << 24;
    uint32_t high = (uint32_t)mac[4] | (uint32_t)mac[5] << 8;

    checkpoint();
    if (write_register(PLA, PLA_CRWECR, 1, 0xc0) != 0 || write_register(PLA, PLA_IDR, 4, low) != 0 ||
            write_register(PLA, PLA_IDR + 4, 2, high) != 0 || write_register(PLA, PLA_CRWECR, 1, 0) != 0)
        return -1;
    return vnd_regs_address_matches(mac) == 1 ? 0 : -1;
}

int vnd_regs_address_matches(const uint8_t mac[6])
{
    uint8_t data[8];

    if (read_block(PLA, PLA_IDR, data, sizeof data) != 0) return -1;
    return memcmp(data, mac, 6) == 0;
}

/* r8153_set_rx_early_size(): in units of 16 bytes on the RTL8157 and RTL8159, and 8 on
 * the RTL8156B.
 * RX_RESERVED is the same for both: 16 + 16 there, 24 + 8 here. */
static int set_aggregation(int early_kib, int coalesce_us)
{
    uint32_t unit = g_chip == RTL_8156B ? 8 : 16;
    uint32_t early = ((uint32_t)early_kib * 1024 - RX_RESERVED) / unit, timer = (uint32_t)coalesce_us * 1000 / 8;
    const struct step size = {"RX_EARLY_SIZE", USB, USB_RX_EARLY_SIZE, 2, 0, 0xffff, early};
    const struct step wait = {"RX_EXTRA_AGGR_TMR", USB, USB_RX_EXTRA_AGGR_TMR, 2, 0, 0xffff, timer};

    if (early_kib == g_early_kib && coalesce_us == g_coalesce_us) return 0;
    if (early_kib < VND_MIN_EARLY || early_kib > VND_MAX_EARLY || timer > 0xffff) return -1;
    if (apply(&size) != 0 || apply(&wait) != 0) return -1;
    g_early_kib = early_kib;
    g_coalesce_us = coalesce_us;
    return 0;
}

int vnd_regs_mark_link(int up)
{
    const struct step on = {"EXTRA_STATUS.CUR_LINK_OK", PLA, PLA_EXTRA_STATUS, 2, 0, 0, CUR_LINK_OK};
    const struct step off = {"EXTRA_STATUS.CUR_LINK_OK", PLA, PLA_EXTRA_STATUS, 2, 0, CUR_LINK_OK, 0};

    return apply(up ? &on : &off);
}

int vnd_regs_bring_up(const uint8_t mac[6])
{
    int matches;

    wait_for_chip();
    if (g_chip == RTL_8156B) {
        if (APPLY(g_init_8156b) != 0 || flow_control_patch() != 0 || APPLY(g_init_8156b_tail) != 0) return -1;
    } else {
        if (APPLY(g_init_v2) != 0) return -1;
        if (g_chip == RTL_8159 ? APPLY(g_usb_ctrl_8159) != 0 : APPLY(g_usb_ctrl_8157) != 0) return -1;
        if (APPLY(g_init_v2_tail) != 0) return -1;
        if (clear_rx_zero_length() != 0) {
            say("  BMU access failed");
            return -1;
        }
    }
    if (APPLY(g_up_stop) != 0) return -1;
    if (g_chip == RTL_8156B ? reset_mac() != 0 : APPLY(g_stop_v2) != 0) return -1;
    if (APPLY(g_bmu_reset) != 0) return -1;
    matches = vnd_regs_address_matches(mac);
    if (matches < 0 || (!matches && write_address(mac) != 0)) return -1;
    if (g_chip == RTL_8156B) {
        if (APPLY(g_up_enable_8156b) != 0 || set_aggregation(early_kib(), VND_LINUX_COALESCE) != 0 || APPLY(g_fc_patch_off) != 0)
            return -1;
        usleep(1500);
        if (APPLY(g_fc_patch_on) != 0) return -1;
    } else if (APPLY(g_up_enable_v2) != 0 || set_aggregation(early_kib(), VND_LINUX_COALESCE) != 0) {
        return -1;
    }
    return APPLY(g_enable);
}

int vnd_regs_open_filter(void)
{
    return APPLY(g_filter);
}

/* The chip's own counters since bring-up reset them, as Linux reads them for its
 * statistics. Missed frames are ones the chip had no room for: the console was not
 * taking them fast enough. */
static void say_tally(void)
{
    uint8_t data[RTL_TALLY_LEN];
    struct rtl_tally tally;

    if (read_block(PLA, PLA_TALLY, data, sizeof data) != 0 || rtl_tally_reply(data, sizeof data, &tally) != 0) {
        say("chip counters: unreadable, errno %d", errno);
        return;
    }
    say("chip counters: rx %llu frames, %u errors, %u missed; tx %llu frames, %llu errors, %u aborted, %u underrun",
        (unsigned long long)tally.rx_packets, tally.rx_errors, tally.rx_missed, (unsigned long long)tally.tx_packets,
        (unsigned long long)tally.tx_errors, tally.tx_aborted, tally.tx_underrun);
}

void vnd_regs_stop(void)
{
    say_tally();
    if (APPLY(g_stop) != 0) say("the MAC was not stopped: replug the adapter");
}

int vnd_regs_read_phy(uint16_t *status)
{
    uint32_t value = 0;

    if (read_register(PLA, 0xe908, 2, &value) != 0) return -1;
    *status = (uint16_t)value;
    return 0;
}

uint16_t vnd_regs_phy_status(void)
{
    uint16_t status = 0;

    return vnd_regs_read_phy(&status) == 0 ? status : 0;
}

int vnd_regs_enabled(void)
{
    uint32_t value = 0;

    if (read_register(PLA, PLA_CR, 1, &value) != 0) return -1;
    return (value & (CR_RE | CR_TE)) == (CR_RE | CR_TE);
}
