/* SPDX-License-Identifier: GPL-3.0-or-later
 * Adapted from tom-ps5-usbnet (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later),
 * include/frame.h, for PS5Upload's USB adapter daemon (payload/usbnet/README.md). */
#ifndef FRAME_H
#define FRAME_H
#include <stddef.h>
#include <stdint.h>

#define MAX_FRAME 1514

struct frame {
    const uint8_t *data;
    size_t len;
};
#endif
