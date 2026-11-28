/* SPDX-License-Identifier: GPL-3.0-or-later
 * Adapted from tom-ps5-usbnet (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later),
 * src/ncm.c, for PS5Upload's USB adapter daemon (payload/usbnet/README.md). */
/* CDC NCM 1.0 NTB16 handling, in memory only. Field layout follows the CDC NCM
 * specification and Linux drivers/net/usb/cdc_ncm.c. */
#include "ncm.h"
#include <string.h>

enum { DT_INTERFACE = 0x04, DT_ENDPOINT = 0x05, DT_CS_INTERFACE = 0x24 };
enum { CDC_UNION = 0x06, CDC_ETHERNET = 0x0f, CDC_NCM = 0x1a };
enum { CDC_SUBCLASS_ECM = 0x06, CDC_SUBCLASS_NCM = 0x0d };
enum { NTH16_LEN = 12, NDP16_MIN_LEN = 16, NDP_LIMIT = 8 };
enum { DT_STRING = 0x03, MAC_STRING_LEN = 26 };
#define NTH16_SIGNATURE 0x484d434eu     /* "NCMH" */
#define NDP16_SIGNATURE 0x304d434eu     /* "NCM0", no CRC */

static uint16_t le16(const uint8_t *p)
{
    return (uint16_t)(p[0] | (p[1] << 8));
}

static uint32_t le32(const uint8_t *p)
{
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static void put_le16(uint8_t *p, uint16_t v)
{
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
}

static void put_le32(uint8_t *p, uint32_t v)
{
    put_le16(p, (uint16_t)v);
    put_le16(p + 2, (uint16_t)(v >> 16));
}

static int find_function(const uint8_t *d, size_t n, unsigned subclass, struct ncm_function *out)
{
    struct ncm_function f;
    size_t off, total;
    int have_comm = 0, have_union = 0, have_ncm = 0, in_comm = 0, in_data = 0;
    uint8_t alt = 0;

    if (!d || !out) return NCM_EINVAL;
    if (n < 9 || d[1] != 0x02) return NCM_ETRUNC;
    total = le16(d + 2);
    if (total > n) return NCM_ETRUNC;
    memset(&f, 0, sizeof f);
    for (off = 0; off + 2 <= total; off += d[off]) {
        const uint8_t *p = d + off;
        size_t len = p[0];

        if (len < 2 || len > total - off) return NCM_EBOUNDS;
        if (p[1] == DT_INTERFACE && len >= 9) {
            in_comm = !have_comm && p[3] == 0 && p[5] == 0x02 && p[6] == subclass;
            if (in_comm) {
                have_comm = 1;
                f.comm_iface = p[2];
            }
            in_data = have_union && p[2] == f.data_iface && p[5] == 0x0a && p[4] >= 2 && !f.ep_in;
            alt = p[3];
        } else if (p[1] == DT_CS_INTERFACE && len >= 3 && in_comm) {
            if (p[2] == CDC_UNION && len >= 5) {
                have_union = 1;
                f.data_iface = p[4];
            } else if (p[2] == CDC_NCM && len >= 6) {
                have_ncm = 1;
                f.capabilities = p[5];
            } else if (p[2] == CDC_ETHERNET && len >= 13) {
                f.mac_string = p[3];
                f.max_segment = le16(p + 8);
            }
        } else if (p[1] == DT_ENDPOINT && len >= 7) {
            unsigned type = p[3] & 3u;
            if (in_comm && type == 3 && (p[2] & 0x80)) {
                f.ep_notify = p[2];
                f.notify_mps = le16(p + 4);
            } else if (in_data && type == 2 && (p[2] & 0x80)) {
                f.ep_in = p[2];
                f.in_mps = le16(p + 4);
                f.data_alt = alt;
            } else if (in_data && type == 2) {
                f.ep_out = p[2];
                f.out_mps = le16(p + 4);
                f.data_alt = alt;
            }
        }
    }
    if (subclass == CDC_SUBCLASS_NCM && !have_ncm) return NCM_ENOTFOUND;
    if (!have_comm || !have_union || !f.ep_in || !f.ep_out || !f.ep_notify || !f.in_mps)
        return NCM_ENOTFOUND;
    *out = f;
    return NCM_OK;
}

int ncm_find_function(const uint8_t *d, size_t n, struct ncm_function *out)
{
    return find_function(d, n, CDC_SUBCLASS_NCM, out);
}

int ncm_find_ecm_function(const uint8_t *d, size_t n, struct ncm_function *out)
{
    return find_function(d, n, CDC_SUBCLASS_ECM, out);
}

int ncm_parse_parameters(const uint8_t *d, size_t n, struct ncm_parameters *out)
{
    if (!d || !out) return NCM_EINVAL;
    if (n != NCM_NTB_PARAMETERS_LEN) return NCM_ETRUNC;
    out->formats = le16(d + 2);
    out->in_max = le32(d + 4);
    out->in_divisor = le16(d + 8);
    out->in_remainder = le16(d + 10);
    out->in_alignment = le16(d + 12);
    out->out_max = le32(d + 16);
    out->out_divisor = le16(d + 20);
    out->out_remainder = le16(d + 22);
    out->out_alignment = le16(d + 24);
    out->out_max_datagrams = le16(d + 26);
    return NCM_OK;
}

int ncm_parse_block(const uint8_t *d, size_t n, struct ncm_datagram *out,
    size_t capacity, struct ncm_block *block)
{
    size_t index, used = 0, dropped = 0, total;
    unsigned ndps = 0;

    if (!d || !block || (!out && capacity)) return NCM_EINVAL;
    if (n < NTH16_LEN + NDP16_MIN_LEN) return NCM_ETRUNC;
    if (le32(d) != NTH16_SIGNATURE || le16(d + 4) != NTH16_LEN) return NCM_ESIGNATURE;
    total = le16(d + 8);
    if (total < NTH16_LEN + NDP16_MIN_LEN || total > n) return NCM_EBOUNDS;
    index = le16(d + 10);
    while (index) {
        size_t ndp_len, entry;

        if (ndps == NDP_LIMIT || index < NTH16_LEN || (index & 3) || index > total - NDP16_MIN_LEN)
            return NCM_EBOUNDS;
        if (le32(d + index) != NDP16_SIGNATURE) return NCM_ESIGNATURE;
        ndp_len = le16(d + index + 4);
        if (ndp_len < NDP16_MIN_LEN || (ndp_len & 3) || ndp_len > total - index) return NCM_EBOUNDS;
        for (entry = index + 8; entry + 4 <= index + ndp_len; entry += 4) {
            size_t offset = le16(d + entry), len = le16(d + entry + 2);

            if (!offset || !len) break;         /* the terminator */
            if (offset < NTH16_LEN || len < NCM_MIN_FRAME || len > NCM_MAX_FRAME ||
                len > total || offset > total - len) return NCM_EBOUNDS;
            if (used < capacity) {
                out[used].offset = (uint16_t)offset;
                out[used].len = (uint16_t)len;
                used++;
            } else {
                dropped++;
            }
        }
        ndps++;
        index = le16(d + index + 6);
    }
    if (!ndps) return NCM_EBOUNDS;
    block->sequence = le16(d + 6);
    block->block_len = (uint16_t)total;
    block->ndps = ndps;
    block->count = used;
    block->dropped = dropped;
    return NCM_OK;
}

static int hex_digit(unsigned c)
{
    if (c >= '0' && c <= '9') return (int)c - '0';
    if (c >= 'a' && c <= 'f') return (int)c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return (int)c - 'A' + 10;
    return -1;
}

int ncm_parse_mac_string(const uint8_t *d, size_t n, uint8_t mac[6])
{
    uint8_t parsed[6] = {0}, any = 0;
    size_t i;

    if (!d || !mac) return NCM_EINVAL;
    if (n < MAC_STRING_LEN || d[0] < MAC_STRING_LEN) return NCM_ETRUNC;
    if (d[1] != DT_STRING) return NCM_ESIGNATURE;
    for (i = 0; i < 12; i++) {
        int digit = d[3 + 2 * i] ? -1 : hex_digit(d[2 + 2 * i]);

        if (digit < 0) return NCM_ESIGNATURE;
        parsed[i / 2] = (uint8_t)(parsed[i / 2] << 4 | digit);
    }
    for (i = 0; i < 6; i++) any |= parsed[i];
    if (!any || (parsed[0] & 1)) return NCM_EBOUNDS;  /* zero or group address */
    memcpy(mac, parsed, sizeof parsed);
    return NCM_OK;
}

static size_t round_up(size_t value, size_t multiple)
{
    return (value + multiple - 1) / multiple * multiple;
}

int ncm_build_block_layout(const struct ncm_parameters *p, uint16_t sequence,
    const uint8_t *frame, size_t len, const struct ncm_layout *layout, uint8_t *out, size_t capacity)
{
    size_t align, ndp, offset, total, limit;

    if (!p || !frame || !out || !layout) return NCM_EINVAL;
    if (len < NCM_MIN_FRAME || len > NCM_MAX_FRAME) return NCM_EINVAL;
    if (!p->out_divisor || p->out_remainder >= p->out_divisor) return NCM_EINVAL;
    if (p->out_alignment & (p->out_alignment - 1)) return NCM_EINVAL;
    align = p->out_alignment > 4 ? p->out_alignment : 4;
    if (layout->ndp_at_end) {
        ndp = 0;
        offset = NTH16_LEN;
    } else {
        ndp = round_up(NTH16_LEN, align);
        offset = ndp + (layout->ndp_reserve > NDP16_MIN_LEN ? layout->ndp_reserve : NDP16_MIN_LEN);
    }
    offset += (p->out_remainder + (size_t)p->out_divisor - offset % p->out_divisor) % p->out_divisor;
    if (layout->ndp_at_end) ndp = round_up(offset + len, align);
    total = layout->ndp_at_end ? ndp + NDP16_MIN_LEN : offset + len;
    if (total < layout->min_block) total = layout->min_block;
    limit = p->out_max < capacity ? p->out_max : capacity;
    if (total > limit || total > 0xffff) return NCM_EBOUNDS;
    memset(out, 0, total);
    put_le32(out, NTH16_SIGNATURE);
    put_le16(out + 4, NTH16_LEN);
    put_le16(out + 6, sequence);
    put_le16(out + 8, (uint16_t)total);
    put_le16(out + 10, (uint16_t)ndp);
    put_le32(out + ndp, NDP16_SIGNATURE);
    put_le16(out + ndp + 4, NDP16_MIN_LEN);
    put_le16(out + ndp + 8, (uint16_t)offset);
    put_le16(out + ndp + 10, (uint16_t)len);
    memcpy(out + offset, frame, len);
    return (int)total;
}

int ncm_build_block(const struct ncm_parameters *p, uint16_t sequence,
    const uint8_t *frame, size_t len, uint8_t *out, size_t capacity)
{
    static const struct ncm_layout plain;

    return ncm_build_block_layout(p, sequence, frame, len, &plain, out, capacity);
}

int ncm_parse_notification(const uint8_t *d, size_t n, struct ncm_notification *out)
{
    if (!d || !out) return NCM_EINVAL;
    memset(out, 0, sizeof *out);
    if (n < 8) return NCM_ETRUNC;
    if (d[0] != 0xa1) return NCM_ESIGNATURE;
    out->length = 8u + le16(d + 6);
    if (d[1] == 0x00) {
        out->event = NCM_EVENT_CONNECTION;
        out->connected = le16(d + 2) != 0;
    } else if (d[1] == 0x2a) {
        if (n < 16) return NCM_ETRUNC;
        out->event = NCM_EVENT_SPEED;
        out->down_bps = le32(d + 8);
        out->up_bps = le32(d + 12);
    }
    return NCM_OK;
}
