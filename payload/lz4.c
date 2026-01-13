/*
 * Minimal LZ4 block decompressor (LZ4_decompress_safe)
 * Based on LZ4 v1.9.x (BSD 2-Clause License).
 */

#include "lz4.h"

#include <string.h>
#include <stdint.h>

typedef uint8_t  BYTE;
typedef uint16_t U16;
typedef uint32_t U32;
typedef uint64_t U64;

#define MINMATCH 4
#define WILDCOPYLENGTH 8

static unsigned LZ4_hash(U32 sequence) {
    return (sequence * 2654435761U) >> (32 - 16);
}

int LZ4_compressBound(int inputSize) {
    if (inputSize < 0) {
        return 0;
    }
    return inputSize + (inputSize / 255) + 16;
}

int LZ4_compress_default(const char *source, char *dest, int inputSize, int maxOutputSize) {
    const BYTE *ip = (const BYTE *)source;
    const BYTE *const iend = ip + inputSize;
    const BYTE *anchor = ip;

    BYTE *op = (BYTE *)dest;
    BYTE *const oend = op + maxOutputSize;

    const BYTE *const mflimit = iend - MINMATCH;
    const BYTE *const matchlimit = iend - WILDCOPYLENGTH;
    U32 hashTable[1 << 16];
    memset(hashTable, 0, sizeof(hashTable));

    if (inputSize <= MINMATCH) {
        if (op + 1 + inputSize > oend) {
            return 0;
        }
        *op++ = (BYTE)(inputSize << 4);
        memcpy(op, anchor, (size_t)inputSize);
        op += inputSize;
        return (int)(op - (BYTE*)dest);
    }

    while (ip <= mflimit) {
        U32 sequence = *(const U32*)ip;
        unsigned h = LZ4_hash(sequence);
        const BYTE *ref = (const BYTE *)source + hashTable[h];
        hashTable[h] = (U32)(ip - (const BYTE *)source);

        if (ref < ip && *(const U32*)ref == sequence) {
            const BYTE *match = ref;
            const BYTE *start = ip;
            BYTE *token;

            // Literal length
            int lit_len = (int)(start - anchor);
            if (op + 1 + lit_len + 2 + 1 > oend) {
                return 0;
            }
            token = op++;
            if (lit_len >= 15) {
                *token = 15 << 4;
                int len = lit_len - 15;
                while (len >= 255) {
                    *op++ = 255;
                    len -= 255;
                }
                *op++ = (BYTE)len;
            } else {
                *token = (BYTE)(lit_len << 4);
            }
            memcpy(op, anchor, (size_t)lit_len);
            op += lit_len;

            // Offset
            U16 offset = (U16)(start - match);
            *op++ = (BYTE)offset;
            *op++ = (BYTE)(offset >> 8);

            // Match length
            ip += MINMATCH;
            match += MINMATCH;
            while (ip < matchlimit && *ip == *match) {
                ip++;
                match++;
            }
            int match_len = (int)(ip - start - MINMATCH);
            if (match_len >= 15) {
                *token |= 15;
                int len = match_len - 15;
                while (len >= 255) {
                    *op++ = 255;
                    len -= 255;
                }
                *op++ = (BYTE)len;
            } else {
                *token |= (BYTE)match_len;
            }

            anchor = ip;
            if (ip > mflimit) {
                break;
            }
            continue;
        }
        ip++;
    }

    // Last literals
    {
        int lit_len = (int)(iend - anchor);
        if (op + 1 + lit_len > oend) {
            return 0;
        }
        if (lit_len >= 15) {
            *op++ = (BYTE)(15 << 4);
            int len = lit_len - 15;
            while (len >= 255) {
                *op++ = 255;
                len -= 255;
            }
            *op++ = (BYTE)len;
        } else {
            *op++ = (BYTE)(lit_len << 4);
        }
        memcpy(op, anchor, (size_t)lit_len);
        op += lit_len;
    }

    return (int)(op - (BYTE*)dest);
}

static unsigned LZ4_readLE16(const void *memPtr) {
    return (unsigned) ((const BYTE*)memPtr)[0]
        | ((unsigned) ((const BYTE*)memPtr)[1] << 8);
}

int LZ4_decompress_safe(const char *source, char *dest, int compressedSize, int dstCapacity) {
    const BYTE *ip = (const BYTE *)source;
    const BYTE *const iend = ip + compressedSize;
    BYTE *op = (BYTE *)dest;
    BYTE *const oend = op + dstCapacity;

    while (ip < iend) {
        unsigned token = *ip++;
        unsigned literal_length = token >> 4;

        if (literal_length) {
            if (literal_length == 15) {
                BYTE s;
                do {
                    if (ip >= iend) return -1;
                    s = *ip++;
                    literal_length += s;
                } while (s == 255);
            }
            if (op + literal_length > oend || ip + literal_length > iend) return -1;
            memcpy(op, ip, literal_length);
            ip += literal_length;
            op += literal_length;
        }

        if (ip >= iend) {
            return (int)(op - (BYTE*)dest);
        }

        if (ip + 2 > iend) return -1;
        unsigned offset = LZ4_readLE16(ip);
        ip += 2;
        if (offset == 0) return -1;
        BYTE *match = op - offset;
        if (match < (BYTE*)dest) return -1;

        unsigned match_length = token & 0x0F;
        if (match_length == 15) {
            BYTE s;
            do {
                if (ip >= iend) return -1;
                s = *ip++;
                match_length += s;
            } while (s == 255);
        }
        match_length += MINMATCH;

        if (op + match_length > oend) return -1;
        while (match_length--) {
            *op++ = *match++;
        }
    }

    return (int)(op - (BYTE*)dest);
}
