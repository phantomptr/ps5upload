// PS5Upload Play - HDR pictures (HEVC Main10, PQ, BT.2020) shown on an SDR screen.
// The console's HDR stream is 10-bit Y'CbCr, limited range, BT.2020 non-constant luminance,
// SMPTE ST 2084 (PQ). Each pixel: Y'CbCr -> R'G'B' (PQ codes), PQ -> linear light (table),
// BT.2020 -> BT.709 primaries (integer matrix), a highlight roll-off above the HDR reference
// white (203 cd/m2, BT.2408) and the SDR gamma (one table indexed by a float-like log of the
// linear value). Integers only per pixel; the tables are made once.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::hdr
{
// Linear light: 1 << kLinBits is 10000 cd/m2.
constexpr int kLinBits = 22;
// The output table: 64 small values one by one, then 64 steps per power of two.
constexpr int kOutSize = 64 + (kLinBits + 1 - 6) * 64;

struct Tables
{
    std::uint32_t lin[1024]; // PQ code (10-bit) -> linear
    std::uint8_t out[kOutSize];
};

// peak: the brightest the source is taken to reach (cd/m2); white: the HDR level shown as SDR
// white-ish (the roll-off starts below it).
void build(Tables &t, double peak = 1000.0, double white = 203.0) noexcept;

inline int out_index(std::uint32_t v) noexcept
{
    if (v < 64)
        return static_cast<int>(v);
    if (v >= (2u << kLinBits))
        v = (2u << kLinBits) - 1;
    const int e = 31 - __builtin_clz(v); // 6 .. kLinBits
    return 64 + (e - 6) * 64 + static_cast<int>((v >> (e - 6)) & 63);
}

// Y'CbCr (10-bit, limited range) -> R'G'B' PQ codes 0..1023, chroma terms per pixel pair.
struct Chroma
{
    int r, g, b;
};
inline Chroma chroma(int cb, int cr) noexcept
{
    cb -= 512;
    cr -= 512;
    // BT.2020 NCL, scaled by 1023/896 and 4096
    return {6896 * cr, -770 * cb - 2672 * cr, 8799 * cb};
}
inline int clamp10(int v) noexcept
{
    return v < 0 ? 0 : v > 1023 ? 1023 : v;
}

// One pixel: 8-bit R, G, B (BT.709, SDR gamma) packed as 0x00BBGGRR.
inline std::uint32_t pixel(const Tables &t, int y, const Chroma &c) noexcept
{
    const int l = 4783 * (y - 64) + 2048; // 1023/876 * 4096
    const std::uint32_t r = t.lin[clamp10((l + c.r) >> 12)];
    const std::uint32_t g = t.lin[clamp10((l + c.g) >> 12)];
    const std::uint32_t b = t.lin[clamp10((l + c.b) >> 12)];
    // BT.2020 -> BT.709 primaries (x256; each row sums to 256 so greys stay grey)
    const int r7 = (425 * static_cast<int>(r) - 150 * static_cast<int>(g) - 19 * static_cast<int>(b)) >> 8;
    const int g7 = (-32 * static_cast<int>(r) + 290 * static_cast<int>(g) - 2 * static_cast<int>(b)) >> 8;
    const int b7 = (-5 * static_cast<int>(r) - 26 * static_cast<int>(g) + 287 * static_cast<int>(b)) >> 8;
    const std::uint32_t o_r = t.out[out_index(r7 < 0 ? 0u : static_cast<std::uint32_t>(r7))];
    const std::uint32_t o_g = t.out[out_index(g7 < 0 ? 0u : static_cast<std::uint32_t>(g7))];
    const std::uint32_t o_b = t.out[out_index(b7 < 0 ? 0u : static_cast<std::uint32_t>(b7))];
    return o_r | o_g << 8 | o_b << 16;
}

// A sample of a 16-bit plane: libSceVideodec2's Main10 pictures keep the 10 bits in the LOW bits
// of each little-endian word (shift 0); P010 keeps them in the top bits (shift 6).
inline int s10(const std::uint8_t *row, unsigned i, int shift) noexcept
{
    return ((row[2 * i] | row[2 * i + 1] << 8) >> shift) & 1023;
}

// Which of the two a picture is: any luma sample above 1023 can only be P010's.
int shift_of(const std::uint8_t *luma, std::size_t pitch, unsigned w, unsigned h) noexcept;

// A whole row (luma row, interleaved CbCr row of the same picture line pair) into 0xAABBGGRR
// pixels, alpha 255. For tests and the simple path.
void row(const Tables &t, const std::uint8_t *y, const std::uint8_t *uv, unsigned w, int shift,
         std::uint32_t *out) noexcept;
} // namespace play::hdr
