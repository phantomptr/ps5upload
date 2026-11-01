/* See include/vinput_wire.h. Shared by the helper (src/vinput_relay.c) and the companion. */
#include "vinput_wire.h"

#include <string.h>

void vwire_put16(uint8_t *b, uint16_t v) {
    b[0] = (uint8_t)v;
    b[1] = (uint8_t)(v >> 8);
}

void vwire_put32(uint8_t *b, uint32_t v) {
    b[0] = (uint8_t)v;
    b[1] = (uint8_t)(v >> 8);
    b[2] = (uint8_t)(v >> 16);
    b[3] = (uint8_t)(v >> 24);
}

uint16_t vwire_get16(const uint8_t *b) {
    return (uint16_t)(b[0] | (b[1] << 8));
}

uint32_t vwire_get32(const uint8_t *b) {
    return (uint32_t)b[0] | ((uint32_t)b[1] << 8) | ((uint32_t)b[2] << 16) | ((uint32_t)b[3] << 24);
}

int vwire_encode(uint8_t *out, size_t cap, uint8_t type, uint8_t seq, const void *payload, size_t len) {
    if (!out || len > VWIRE_MAX_PAYLOAD || cap < VWIRE_HDR + len || (len && !payload)) return -1;
    out[0] = type;
    out[1] = seq;
    vwire_put16(out + 2, (uint16_t)len);
    if (len) memcpy(out + VWIRE_HDR, payload, len);
    return (int)(VWIRE_HDR + len);
}

int vwire_decode(const uint8_t *buf, size_t have, vwire_frame_t *out) {
    uint16_t len;
    if (have < VWIRE_HDR) return 0;
    len = vwire_get16(buf + 2);
    if (len > VWIRE_MAX_PAYLOAD) return -1;
    if (have < (size_t)VWIRE_HDR + len) return 0;
    out->type = buf[0];
    out->seq = buf[1];
    out->len = len;
    memset(out->payload, 0, sizeof out->payload);
    if (len) memcpy(out->payload, buf + VWIRE_HDR, len);
    return VWIRE_HDR + len;
}

static int hexval(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

int vwire_token_from_hex(const char *hex, size_t len, uint8_t out[VWIRE_TOKEN_LEN]) {
    if (!hex || len < VWIRE_TOKEN_HEX) return -1;
    for (size_t i = 0; i < VWIRE_TOKEN_LEN; i++) {
        int hi = hexval(hex[2 * i]), lo = hexval(hex[2 * i + 1]);
        if (hi < 0 || lo < 0) return -1;
        out[i] = (uint8_t)(hi << 4 | lo);
    }
    return 0;
}

void vwire_token_to_hex(const uint8_t tok[VWIRE_TOKEN_LEN], char out[VWIRE_TOKEN_HEX + 1]) {
    static const char d[] = "0123456789abcdef";
    for (size_t i = 0; i < VWIRE_TOKEN_LEN; i++) {
        out[2 * i] = d[tok[i] >> 4];
        out[2 * i + 1] = d[tok[i] & 15];
    }
    out[VWIRE_TOKEN_HEX] = '\0';
}

int vwire_token_equal(const uint8_t a[VWIRE_TOKEN_LEN], const uint8_t b[VWIRE_TOKEN_LEN]) {
    uint8_t d = 0;
    for (size_t i = 0; i < VWIRE_TOKEN_LEN; i++) d |= (uint8_t)(a[i] ^ b[i]);
    return d == 0;
}
