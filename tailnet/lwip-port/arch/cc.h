/* lwIP compiler/platform glue for tailnet (macOS, Linux, the PS5 payload, Windows). */
#ifndef TN_LWIP_CC_H
#define TN_LWIP_CC_H

#include <stdio.h>
#include <stdlib.h>

#if defined(_MSC_VER)
/* No __attribute__((packed)): lwIP includes arch/bpstruct.h and arch/epstruct.h
 * around each wire struct instead (#pragma pack). */
#define PACK_STRUCT_USE_INCLUDES
#endif
#ifdef _WIN32
#define LWIP_NO_UNISTD_H 1 /* MSVC has none (MinGW does, so its compile check could not see it) */
#endif
#if defined(_WIN32) && !defined(SSIZE_MAX) /* (src/os.h defines both first in our files) */
#include <basetsd.h>
#include <stdint.h>
typedef SSIZE_T ssize_t;
#define SSIZE_MAX INTPTR_MAX
#endif

#define LWIP_PLATFORM_DIAG(x) do { } while (0)
#define LWIP_PLATFORM_ASSERT(x) do { fprintf(stderr, "lwip assert: %s (%s:%d)\n", (x), __FILE__, __LINE__); abort(); } while (0)

#endif
