/* SPDX-License-Identifier: GPL-3.0-or-later
 * Adapted from tom-ps5-usbnet (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later),
 * include/rtl.h, for PS5Upload's USB adapter daemon (payload/usbnet/README.md). */
#ifndef RTL_H
#define RTL_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Pure memory codec; no USB, OS networking, allocation, or mutable globals. */
/* The RTL8157 and the RTL8159 share one descriptor format. */
enum rtl_chip { RTL_8156B = 1, RTL_8159 = 2, RTL_8157 = 3 };
enum rtl_result {
    RTL_OK = 0, RTL_EINVAL = -1, RTL_ETRUNC = -2,
    RTL_EFORMAT = -3, RTL_ENOSPC = -4, RTL_EUNSUPPORTED = -5
};
#define RTL_MAX_FRAME 9216u /* Ethernet bytes, excluding FCS. */
struct rtl_frame { const uint8_t *data; size_t len; };
struct rtl_frame_view {
    const uint8_t *data; size_t len; /* borrowed until input buffer changes */
    uint8_t vlan_present; uint16_t vlan_tci;
    uint32_t status[6]; /* raw host-endian descriptor words; never trust HW csum */
};
struct rtl_format {
    size_t rx_header, tx_header, rx_align, tx_align;
};
int rtl_get_format(enum rtl_chip chip, struct rtl_format *out);

/* All encoders preflight all frames/capacity before changing output. Input and
 * output must not overlap. TX pads short Ethernet frames to 60 bytes; hardware
 * supplies FCS. No checksum, segmentation, or VLAN insertion offload is enabled.
 * Output ends at the final frame, without mandatory final alignment padding. */
int rtl_tx_encode(enum rtl_chip chip,
    const struct rtl_frame *frames, size_t count,
    uint8_t *out, size_t capacity, size_t *written);
/* TX decode accepts only the no-offload profile produced above, validates both
 * v2 length copies/signature, and exposes padded Ethernet frames without FCS. */
int rtl_tx_decode(enum rtl_chip chip, const uint8_t *data, size_t len,
    struct rtl_frame_view *frames, size_t capacity, size_t *count);
/* RX removes four FCS bytes (does not verify CRC). VLAN stripping metadata is
 * preserved in vlan_*; caller must reinsert tag or reject it before bare-Ethernet
 * delivery. status[] preserves descriptor data; stack checksums remain required.
 * Complete aggregate is validated before any views are published; on error
 * count=0 and output array is unchanged. capacity=0 / frames=NULL can count via
 * ENOSPC, with count=required. Empty buffers/counts are invalid. Final RX/TX
 * padding may be absent, partial, or complete up to the next alignment boundary;
 * padding content is ignored (hardware padding need not be zero). */
int rtl_rx_decode(enum rtl_chip chip, const uint8_t *data, size_t len,
    struct rtl_frame_view *frames, size_t capacity, size_t *count);
/* Synthetic TEST fixture generator, NOT a hardware capture or USB operation.
 * Adds a computed Ethernet CRC32 FCS, no RX offload metadata, and inter-frame
 * alignment. Does not emulate PHY/MAC initialization, filtering, or USB timing. */
int rtl_rx_fixture_encode(enum rtl_chip chip,
    const struct rtl_frame *frames, size_t count,
    uint8_t *out, size_t capacity, size_t *written);

struct rtl_usb_device {
    uint16_t vid, pid, bcd_device, bcd_usb;
    uint8_t configurations;
};
struct rtl_usb_interface {
    uint8_t configuration, number, alternate, class_code, subclass, protocol;
    uint8_t bulk_in, bulk_out, interrupt_in;
    uint16_t bulk_in_mps, bulk_out_mps;
};
/* Parse caller-supplied descriptor bytes only. Exact device descriptor (18 bytes)
 * and exact config wTotalLength required; explicit interface+alternate selection.
 * Config validates descriptor lengths, interface/endpoint counts and duplicate
 * addresses; selected interface requires exactly one bulk IN and one bulk OUT.
 * Does not choose configuration, claim interface, enumerate, or open anything. */
int rtl_parse_device(const uint8_t *data, size_t len,
    struct rtl_usb_device *out);
int rtl_parse_config(const uint8_t *data, size_t len,
    uint8_t interface_number, uint8_t alternate,
    struct rtl_usb_interface *out);
/* chip_version is PLA_TCR1 register value, NOT USB bcdDevice. Reference Realtek
 * VID/PIDs plus compatible observed silicon revision are required. Returns only
 * a target family declaration, not proof of commercial adapter compatibility.
 * 8156B: 0x7400/0x7410/0x7420; 8157: 0x1030; 8159: 0x2020 (mask 0x7cf0). OEM IDs fail
 * closed. */
int rtl_identify_target(const struct rtl_usb_device *device,
    uint16_t chip_version, enum rtl_chip *out);
/* Pure 8-byte USB SETUP data for the upstream read-only PLA_TCR0 version
 * request (c0 05 10 e6 00 01 04 00). Does not issue the request. Reply helper
 * requires all four bytes and extracts (little-endian word >> 16) & 0x7cf0. */
int rtl_version_request(uint8_t setup[8]);
int rtl_version_reply(const uint8_t *data, size_t len, uint16_t *version);
/* The same for the read-only PLA_PHYSTATUS register (c0 05 08 e9 00 01 04 00). */
/* Register blocks of the MAC and of the USB side, as the r8152 driver addresses them. */
#define RTL_MCU_PLA 0x0100u
#define RTL_MCU_USB 0x0000u
#define RTL_TALLY_LEN 64u
struct rtl_tally {
    uint64_t tx_packets, rx_packets, tx_errors;
    unsigned rx_errors, rx_missed;      /* rx_missed is 16 bits and wraps */
    unsigned tx_aborted, tx_underrun;
};

/* A dword-aligned register read of `size` bytes. Fails for an unaligned address. */
int rtl_read_request(uint8_t setup[8], unsigned mcu, unsigned address, unsigned size);
/* wIndex of a one-byte write to `address`, whose dword goes out shifted into place. */
unsigned rtl_byte_write_index(unsigned mcu, unsigned address);
/* A register write as the driver sends it: a dword-aligned address, byte enables in
 * wIndex and the value shifted into place. `width` is 1, 2 or 4, and the address
 * must be aligned to it. */
struct rtl_write { uint8_t setup[8], data[4]; };
int rtl_write_request(struct rtl_write *out, unsigned mcu, unsigned address, unsigned width, uint32_t value);
/* The TX descriptor of one frame of `len` bytes, no offload: 8 bytes on the RTL8156B,
 * 16 on the RTL8157 and RTL8159. Returns its length, or RTL_EINVAL. */
#define RTL_MAX_TX_HEADER 16u
int rtl_tx_header(enum rtl_chip chip, uint8_t *header, size_t len);
/* Walks a bulk IN aggregate as Linux's rx_bottom does: every frame before the first bad
 * descriptor is published, so one bad frame costs only what follows it. `count` is
 * always set. Returns 0 if the whole buffer was walked, else why it stopped. */
int rtl_rx_scan(enum rtl_chip chip, const uint8_t *data, size_t len, struct rtl_frame_view *frames, size_t capacity, size_t *count);
int rtl_tally_reply(const uint8_t *data, size_t len, struct rtl_tally *out);
int rtl_phy_status_request(uint8_t setup[8]);
int rtl_phy_status_reply(const uint8_t *data, size_t len, uint16_t *status);
/* Whether a PLA_PHYSTATUS value has link, and its rate in Mbit/s; 0 if unknown. */
int rtl_phy_link(uint16_t status);
unsigned rtl_phy_mbps(uint16_t status);

#ifdef __cplusplus
}
#endif
#endif
