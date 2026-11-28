/*
 * PS5Upload Play spike - reference picture hashes on the build host.
 * Reads raw NV12 pictures (width x height) from stdin and prints one hash per
 * picture: FNV-1a 64 over little-endian 8-byte words, Y rows then UV rows,
 * the same function the app runs on the console (src/main.cpp, picture_hash).
 * SPDX-License-Identifier: GPL-3.0-or-later
 */
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv)
{
    if (argc != 3)
        return 2;
    const size_t width = strtoul(argv[1], NULL, 10), height = strtoul(argv[2], NULL, 10);
    const size_t size = width * height * 3 / 2;
    unsigned char *picture = malloc(size);
    if (!picture || width % 8)
        return 2;
    while (fread(picture, 1, size, stdin) == size) {
        uint64_t h = 0xcbf29ce484222325ull;
        for (size_t i = 0; i < size; i += 8) {
            uint64_t word;
            memcpy(&word, picture + i, 8);
            h ^= word;
            h *= 0x100000001b3ull;
        }
        printf("%016" PRIx64 "\n", h);
    }
    free(picture);
    return 0;
}
