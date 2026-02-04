#ifndef PROTOCOL_DEFS_H
#define PROTOCOL_DEFS_H

#include <stdint.h>

#define MAGIC_FTX1 0x31585446 // 'FTX1' (Little Endian: F=46, T=54, X=58, 1=31 -> 0x31585446)

enum FrameType {
    FRAME_HELLO = 1,
    FRAME_HELLO_ACK = 2,
    FRAME_MANIFEST = 3,
    FRAME_PACK_ACK = 5,
    FRAME_FINISH = 6,
    FRAME_ERROR = 7,
    FRAME_PACK_V4 = 15,
    FRAME_PACK_LZ4_V4 = 16,
    FRAME_PACK_ZSTD_V4 = 17,
    FRAME_PACK_LZMA_V4 = 18
};

struct FrameHeader {
    uint32_t magic;
    uint32_t type;
    uint64_t body_len;
} __attribute__((packed));

// Inside a PACK_V4 frame:
// [RecordCount:4]
// [Record1]
// [Record2]...

// Record Format (V4):
// [PathLen:2] [Flags:2] [PathBytes] [DataLen:8] [Offset?:8] [TotalSize?:8] [DataBytes]

#endif
