#ifndef PS5_AI_COMPAT_H
#define PS5_AI_COMPAT_H
#ifndef __ASSEMBLER__
#include <stddef.h>
#ifdef __cplusplus
extern "C" {
#endif
/* FreeBSD 11 SDK headers predate getentropy(). Implemented in freebsd11.c. */
int getentropy(void *buffer, size_t size);
#ifdef __cplusplus
}
#endif
#endif
#endif
