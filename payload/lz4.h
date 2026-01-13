/*
 * LZ4 - Fast LZ compression algorithm
 * Copyright (C) 2011-2020, Yann Collet.
 * BSD 2-Clause License (see LICENSE in LZ4 project)
 */

#ifndef LZ4_H
#define LZ4_H

#if defined(__cplusplus)
extern "C" {
#endif

#include <stddef.h>

int LZ4_decompress_safe(const char *src, char *dst, int compressedSize, int dstCapacity);
int LZ4_compress_default(const char *src, char *dst, int srcSize, int dstCapacity);
int LZ4_compressBound(int inputSize);

#if defined(__cplusplus)
}
#endif

#endif /* LZ4_H */
