// PS5Upload Play - HDR pictures shown on an SDR screen: the tables.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "hdr.hpp"

namespace play::hdr
{
namespace
{
// Natural log and exp for the tables only (the app links no maths library).
double ln(double x) noexcept
{
    if (x <= 0)
        return -1e300;
    int e = 0;
    while (x > 2.0)
        x *= 0.5, ++e;
    while (x < 1.0)
        x *= 2.0, --e;
    // ln(x) = 2 atanh((x - 1) / (x + 1)), x in [1, 2]
    const double s = (x - 1.0) / (x + 1.0), s2 = s * s;
    double term = s, sum = 0;
    for (int k = 1; k < 60; k += 2)
    {
        sum += term / k;
        term *= s2;
    }
    return 2.0 * sum + e * 0.69314718055994530942;
}

double ex(double x) noexcept
{
    if (x < -700)
        return 0;
    // e^x = (e^(x / 2^k))^(2^k)
    int k = 0;
    while (x > 0.5 || x < -0.5)
        x *= 0.5, ++k;
    double term = 1, sum = 1;
    for (int n = 1; n < 30; ++n)
    {
        term *= x / n;
        sum += term;
    }
    while (k-- > 0)
        sum *= sum;
    return sum;
}

double pw(double a, double b) noexcept
{
    return a <= 0 ? 0 : ex(b * ln(a));
}

// SMPTE ST 2084: a code value 0..1 to cd/m2.
double pq_to_nits(double n) noexcept
{
    constexpr double m1 = 0.1593017578125, m2 = 78.84375, c1 = 0.8359375, c2 = 18.8515625, c3 = 18.6875;
    const double p = pw(n, 1.0 / m2);
    const double num = p - c1 > 0 ? p - c1 : 0;
    return 10000.0 * pw(num / (c2 - c3 * p), 1.0 / m1);
}

// Relative to white (1.0 = white): unchanged up to the knee, then an extended Reinhard curve
// (slope 1 at the knee, so no visible bend) that brings the source's peak to exactly 1.0.
double roll_off(double x, double peak_rel) noexcept
{
    constexpr double knee = 0.6, span = 1.0 - knee;
    if (x <= knee)
        return x;
    if (peak_rel <= 1.0)
        return x > 1.0 ? 1.0 : x;
    const double u = (x - knee) / span, top = (peak_rel - knee) / span;
    return knee + span * u * (1.0 + u / (top * top)) / (1.0 + u);
}
} // namespace

void build(Tables &t, double peak, double white) noexcept
{
    for (int c = 0; c < 1024; ++c)
    {
        const double nits = pq_to_nits(c / 1023.0);
        double v = nits / 10000.0 * static_cast<double>(1u << kLinBits);
        t.lin[c] = static_cast<std::uint32_t>(v + 0.5);
    }
    const double peak_rel = peak / white;
    for (int i = 0; i < kOutSize; ++i)
    {
        // the middle of the linear values this entry stands for
        double v;
        if (i < 64)
            v = i;
        else
        {
            const int e = 6 + (i - 64) / 64, m = (i - 64) % 64;
            v = static_cast<double>((64 + m) * (1u << (e - 6))) + (1u << (e - 6)) / 2.0;
        }
        const double nits = v / static_cast<double>(1u << kLinBits) * 10000.0;
        double y = roll_off(nits / white, peak_rel);
        y = y > 1.0 ? 1.0 : y < 0 ? 0 : y;
        // BT.1886 / the SDR video gamma (2.4)
        const double g = pw(y, 1.0 / 2.4);
        int o = static_cast<int>(g * 255.0 + 0.5);
        t.out[i] = static_cast<std::uint8_t>(o > 255 ? 255 : o);
    }
}

int shift_of(const std::uint8_t *luma, std::size_t pitch, unsigned w, unsigned h) noexcept
{
    // a few rows across the picture; a low-aligned picture never has a word above 1023
    for (unsigned r = 1; r < 8; ++r)
    {
        const std::uint8_t *row = luma + pitch * (h * r / 8);
        for (unsigned x = 0; x < w; x += 7)
            if ((row[2 * x] | row[2 * x + 1] << 8) > 1023)
                return 6;
    }
    return 0;
}

void row(const Tables &t, const std::uint8_t *y, const std::uint8_t *uv, unsigned w, int shift,
         std::uint32_t *out) noexcept
{
    for (unsigned x = 0; x < w; x += 2)
    {
        const Chroma c = chroma(s10(uv, x, shift), s10(uv, x + 1, shift));
        out[x] = 0xff000000u | pixel(t, s10(y, x, shift), c);
        if (x + 1 < w)
            out[x + 1] = 0xff000000u | pixel(t, s10(y, x + 1, shift), c);
    }
}
} // namespace play::hdr
