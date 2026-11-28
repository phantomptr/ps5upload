/* SPDX-License-Identifier: GPL-3.0-or-later
 * Adapted from tom-ps5-usbnet (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later),
 * include/ncm.h, for PS5Upload's USB adapter daemon (payload/usbnet/README.md). */
#ifndef NCM_H
#define NCM_H
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

enum ncm_result {
    NCM_OK = 0, NCM_EINVAL = -1, NCM_ETRUNC = -2, NCM_ESIGNATURE = -3,
    NCM_EBOUNDS = -4, NCM_ENOTFOUND = -5
};

#define NCM_MAX_FRAME 9216u          /* Ethernet bytes */
#define NCM_MIN_FRAME 14u            /* Ethernet header */
#define NCM_NTB_PARAMETERS_LEN 28u
#define NCM_NTB32_SUPPORTED 0x0002u

/* Class requests to the communication interface. */
#define NCM_GET_NTB_PARAMETERS 0x80
#define NCM_SET_NTB_FORMAT 0x84
#define NCM_SET_PACKET_FILTER 0x43
#define NCM_GET_ETHERNET_STATISTIC 0x44
#define NCM_GET_NET_ADDRESS 0x81
#define NCM_SET_NET_ADDRESS 0x82
#define NCM_SET_NTB_INPUT_SIZE 0x86
#define NCM_GET_MAX_DATAGRAM 0x87
#define NCM_SET_MAX_DATAGRAM 0x88
#define NCM_NTB16_FORMAT 0x0000
#define NCM_FILTER_ALL_MULTICAST 0x0002
#define NCM_FILTER_DIRECTED 0x0004
#define NCM_FILTER_BROADCAST 0x0008
#define NCM_STAT_XMIT_OK 1           /* GET_ETHERNET_STATISTIC selectors */
#define NCM_STAT_RCV_OK 2
#define NCM_STAT_XMIT_ERROR 3
#define NCM_STAT_RCV_ERROR 4
#define NCM_CAP_NET_ADDRESS 0x02     /* bmNetworkCapabilities */
#define NCM_CAP_MAX_DATAGRAM 0x08

/* Where the function lives in one configuration descriptor. ECM uses the same
 * fields; `capabilities` stays 0 there. */
struct ncm_function {
    uint8_t comm_iface, data_iface, data_alt;
    uint8_t ep_in, ep_out, ep_notify;
    uint16_t in_mps, out_mps, notify_mps, max_segment;
    uint8_t capabilities;
    uint8_t mac_string;                 /* iMACAddress, a string descriptor index */
};

struct ncm_parameters {
    uint16_t formats, in_divisor, in_remainder, in_alignment;
    uint16_t out_divisor, out_remainder, out_alignment, out_max_datagrams;
    uint32_t in_max, out_max;
};

struct ncm_datagram { uint16_t offset, len; };

/* Ways to lay out an outgoing NTB. Both are valid; devices can differ in what they accept. */
struct ncm_layout {
    int ndp_at_end;                     /* NDP after the datagram, not after the NTH */
    size_t min_block;                   /* pad the block with zeros up to this length */
    size_t ndp_reserve;                 /* room kept for the NDP, as Linux keeps it */
};

struct ncm_block {
    uint16_t sequence, block_len;
    unsigned ndps;
    size_t count;                       /* datagrams stored */
    size_t dropped;                     /* valid datagrams past the caller's capacity */
};

enum ncm_event { NCM_EVENT_NONE, NCM_EVENT_CONNECTION, NCM_EVENT_SPEED };

struct ncm_notification {
    size_t length;              /* bytes this notification takes, header included */
    enum ncm_event event;
    int connected;
    uint32_t down_bps, up_bps;
};

/* All of these are pure memory: input comes from a device, so every length and
 * offset is checked. They open nothing and issue no USB request. */

/* Finds the NCM function in a configuration descriptor. */
int ncm_find_function(const uint8_t *config, size_t len, struct ncm_function *out);
/* The same for a CDC ECM function. */
int ncm_find_ecm_function(const uint8_t *config, size_t len, struct ncm_function *out);
/* Exactly NCM_NTB_PARAMETERS_LEN bytes of a GET_NTB_PARAMETERS reply. */
int ncm_parse_parameters(const uint8_t *data, size_t len, struct ncm_parameters *out);
/* One NTB16 from bulk IN. Every datagram lies inside the block and has a valid
 * Ethernet length, or the block is rejected as a whole. */
int ncm_parse_block(const uint8_t *data, size_t len, struct ncm_datagram *out,
    size_t capacity, struct ncm_block *block);
/* The iMACAddress string descriptor: twelve hex digits in UTF-16LE. */
int ncm_parse_mac_string(const uint8_t *data, size_t len, uint8_t mac[6]);
/* Wraps one Ethernet frame in an NTB16 for bulk OUT, placed as `parameters` requires.
 * Returns the block length, or a negative ncm_result. */
int ncm_build_block(const struct ncm_parameters *parameters, uint16_t sequence,
    const uint8_t *frame, size_t len, uint8_t *out, size_t capacity);
int ncm_build_block_layout(const struct ncm_parameters *parameters, uint16_t sequence,
    const uint8_t *frame, size_t len, const struct ncm_layout *layout, uint8_t *out, size_t capacity);
/* One message from the interrupt endpoint. */
int ncm_parse_notification(const uint8_t *data, size_t len, struct ncm_notification *out);

#ifdef __cplusplus
}
#endif
#endif
