/* lwIP compiler/platform glue for tailnet (macOS, Linux, the PS5 payload, Windows). */
#ifndef TN_LWIP_CC_H
#define TN_LWIP_CC_H

#include <stdio.h>
#include <stdlib.h>

#define LWIP_PLATFORM_DIAG(x) do { } while (0)
#define LWIP_PLATFORM_ASSERT(x) do { fprintf(stderr, "lwip assert: %s (%s:%d)\n", (x), __FILE__, __LINE__); abort(); } while (0)

#endif
