/* The helper <-> keyboard & mouse companion contract (payload/vinput/, ps5u-vinput.elf).
 *
 * The console's virtual keyboard and mouse (libSceMouse / libSceKeyboard) are driven from a small
 * process of their own, never from the helper: sceMouseInit / sceKeyboardInit hung the whole helper
 * when called in its process (payload/vinput/README.md). The helper only relays (src/vinput_relay.c).
 *
 * Start: the helper writes VWIRE_TOKEN_HEX hex characters to VWIRE_TOKEN_PATH (0600), sends the
 * embedded companion ELF to the console's own loader (127.0.0.1:9021) and connects to
 * 127.0.0.1:VWIRE_PORT. The companion reads and deletes the token, writes its pid to VWIRE_PID_PATH,
 * binds the port (a second instance cannot, and exits), sets up the devices, listens, and serves the
 * one connection that presents the token. Everything is little-endian:
 *
 *   frame   u8 type, u8 seq, u16 len, then len (<= VWIRE_MAX_PAYLOAD) bytes
 *   answer  the same seq, type | VWIRE_REPLY, payload starting with an i32 rc (VWIRE_RC_*)
 *
 *   HELLO   u16 version, token[32]       -> rc, u16 version, u32 pid, u8 mouse, u8 keyboard
 *   CAPS    -                            -> rc, u8 mouse, u8 keyboard, u8 mouse open, u8 keyboard open
 *   OPEN    u8 mouse, u8 keyboard        -> rc, u8 mouse open, u8 keyboard open
 *           (the devices wanted now: one asked for is made, one not asked for is let go and removed)
 *   REPORT  16 bytes (include/vinput.h)  -> rc
 *   HB      -                            -> rc
 *   CLOSE   -                            -> rc  (everything let go, the devices removed)
 *
 * The companion lets go of everything and removes its devices after VWIRE_HB_TIMEOUT_MS with no
 * frame from the helper; it exits when the helper's connection closes, when nobody presented the
 * token within VWIRE_AUTH_WAIT_MS of the start, and after VWIRE_IDLE_EXIT_MS with no device open. */
#ifndef PS5UPLOAD_VINPUT_WIRE_H
#define PS5UPLOAD_VINPUT_WIRE_H

#include <stddef.h>
#include <stdint.h>

#define VWIRE_PORT 9184
#define VWIRE_VERSION 1
#define VWIRE_NAME "ps5u-vinput" /* never "ps5upload…": the helper's own-instance sweep kills that */
#ifndef VWIRE_DIR                /* host tests point it elsewhere */
#define VWIRE_DIR "/data/ps5upload"
#endif
#define VWIRE_TOKEN_PATH VWIRE_DIR "/vinput.token"
#define VWIRE_PID_PATH VWIRE_DIR "/vinput.pid"
#define VWIRE_LOG_PATH VWIRE_DIR "/vinput.log"
#define VWIRE_HANDLES_PATH VWIRE_DIR "/vinput.handles" /* the open devices (src/vinput_dev.c) */
#define VWIRE_TOKEN_LEN 32
#define VWIRE_TOKEN_HEX (2 * VWIRE_TOKEN_LEN)

#define VWIRE_HB_TIMEOUT_MS 2000LL
#define VWIRE_IDLE_EXIT_MS 30000LL
#define VWIRE_AUTH_WAIT_MS 10000LL

#define VWIRE_HDR 4
#define VWIRE_MAX_PAYLOAD 64
#define VWIRE_MAX_FRAME (VWIRE_HDR + VWIRE_MAX_PAYLOAD)
#define VWIRE_REPLY 0x80u

enum {
    VWIRE_HELLO = 1,
    VWIRE_CAPS = 2,
    VWIRE_OPEN = 3,
    VWIRE_REPORT = 4,
    VWIRE_HB = 5,
    VWIRE_CLOSE = 6,
};

#define VWIRE_RC_OK 0
#define VWIRE_RC_UNSUPPORTED (-1) /* = VINPUT_E_UNSUPPORTED */
#define VWIRE_RC_FAILED (-2)      /* = VINPUT_E_FAILED */
#define VWIRE_RC_BAD_TOKEN (-4)
#define VWIRE_RC_BAD_REQUEST (-5)
#define VWIRE_RC_NO_USER (-6) /* = VINPUT_E_NO_USER */

typedef struct {
    uint8_t type;
    uint8_t seq;
    uint16_t len;
    uint8_t payload[VWIRE_MAX_PAYLOAD];
} vwire_frame_t;

/* Writes one frame into out (cap >= VWIRE_HDR + len). Its length, or -1. */
int vwire_encode(uint8_t *out, size_t cap, uint8_t type, uint8_t seq, const void *payload, size_t len);
/* Reads one frame from the first `have` bytes of buf: the bytes it took, 0 when more are needed, -1
 * when it is not a frame (too long). */
int vwire_decode(const uint8_t *buf, size_t have, vwire_frame_t *out);

void vwire_put16(uint8_t *b, uint16_t v);
void vwire_put32(uint8_t *b, uint32_t v);
uint16_t vwire_get16(const uint8_t *b);
uint32_t vwire_get32(const uint8_t *b);

/* Hex token <-> bytes. 0, or -1 when `hex` is not VWIRE_TOKEN_HEX hex characters. */
int vwire_token_from_hex(const char *hex, size_t len, uint8_t out[VWIRE_TOKEN_LEN]);
void vwire_token_to_hex(const uint8_t tok[VWIRE_TOKEN_LEN], char out[VWIRE_TOKEN_HEX + 1]);
/* Constant-time compare: 1 when equal. */
int vwire_token_equal(const uint8_t a[VWIRE_TOKEN_LEN], const uint8_t b[VWIRE_TOKEN_LEN]);

#endif
