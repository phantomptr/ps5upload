/* SPDX-License-Identifier: GPL-3.0-or-later
 * Adapted from tom-ps5-usbnet (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later),
 * port/arch/cc.h, for PS5Upload's USB adapter daemon (payload/usbnet/README.md). */
#ifndef UN_ARCH_CC_H
#define UN_ARCH_CC_H
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/time.h>
#define LWIP_TIMEVAL_PRIVATE 0
/* An assertion goes to the log. */
void usbn_net_assert(const char *what, const char *file, int line);
#define LWIP_PLATFORM_DIAG(x) do {} while (0)
#define LWIP_PLATFORM_ASSERT(x) usbn_net_assert((x), __FILE__, __LINE__)
#define PACK_STRUCT_STRUCT __attribute__((packed))
#define PACK_STRUCT_BEGIN
#define PACK_STRUCT_END
#define PACK_STRUCT_FIELD(x) x
#endif
