// PS5Upload Play - drawing into a linear 1920x1080 RGBA picture.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "canvas.hpp"

#include "../core/json.hpp"
#include "i18n.hpp"
#include "../../third_party/qrcodegen/qrcodegen.h"

#include <cstring>

namespace play::ui
{
namespace
{
struct Face
{
    int px = 0, cw = 0, ch = 0, base = 0, count = 0;
    const std::uint32_t *codes = nullptr;
    const std::uint8_t *cov = nullptr;
};
Face faces[5];
int nfaces = 0;

float fsqrt(float v) noexcept
{
    return v > 0 ? __builtin_sqrtf(v) : 0.0f;
}

// atan2 in turns (0-1, starting at 12 o'clock, clockwise): enough for progress rings.
float turn_of(float dx, float dy) noexcept
{
    // angle from "up", clockwise: atan2(dx, -dy)
    const float x = -dy, y = dx;
    const float ax = x < 0 ? -x : x, ay = y < 0 ? -y : y;
    const float mx = ax > ay ? ax : ay, mn = ax > ay ? ay : ax;
    if (mx == 0)
        return 0;
    const float a = mn / mx, s = a * a;
    float r = ((-0.0464964749f * s + 0.15931422f) * s - 0.327622764f) * s * a + a; // atan(a), radians
    if (ay > ax)
        r = 1.57079637f - r;
    if (x < 0)
        r = 3.14159274f - r;
    if (y < 0)
        r = -r;
    float t = r / 6.28318548f;
    return t < 0 ? t + 1 : t;
}

std::uint32_t mix(std::uint32_t d, std::uint32_t c, unsigned a) noexcept
{
    if (a >= 255)
        return c | 0xff000000u;
    const unsigned ia = 255 - a;
    const unsigned r = ((c & 0xff) * a + (d & 0xff) * ia) / 255;
    const unsigned g = (((c >> 8) & 0xff) * a + ((d >> 8) & 0xff) * ia) / 255;
    const unsigned b = (((c >> 16) & 0xff) * a + ((d >> 16) & 0xff) * ia) / 255;
    return 0xff000000u | (b << 16) | (g << 8) | r;
}

const Face &face(Font f) noexcept
{
    static const Face none{};
    const int i = static_cast<int>(f);
    return i < nfaces ? faces[i] : none;
}

const std::uint8_t *glyph(const Face &fc, std::uint32_t code) noexcept
{
    int lo = 0, hi = fc.count - 1;
    while (lo <= hi)
    {
        const int mid = (lo + hi) / 2;
        if (fc.codes[mid] == code)
            return fc.cov + static_cast<std::size_t>(mid) * fc.cw * fc.ch;
        if (fc.codes[mid] < code)
            lo = mid + 1;
        else
            hi = mid - 1;
    }
    return nullptr;
}

std::uint32_t rd32(const std::uint8_t *p) noexcept
{
    return static_cast<std::uint32_t>(p[0]) | p[1] << 8 | p[2] << 16 | static_cast<std::uint32_t>(p[3]) << 24;
}
} // namespace

bool font_load(const std::uint8_t *d, std::size_t len) noexcept
{
    nfaces = 0;
    if (len < 8 || std::memcmp(d, "PPF1", 4) != 0)
        return false;
    const std::uint32_t n = rd32(d + 4);
    std::size_t at = 8;
    for (std::uint32_t i = 0; i < n && i < 5; ++i)
    {
        if (at + 20 > len)
            return false;
        Face f;
        f.px = static_cast<int>(rd32(d + at));
        f.cw = static_cast<int>(rd32(d + at + 4));
        f.ch = static_cast<int>(rd32(d + at + 8));
        f.base = static_cast<int>(rd32(d + at + 12));
        f.count = static_cast<int>(rd32(d + at + 16));
        at += 20;
        const std::size_t codes = static_cast<std::size_t>(f.count) * 4;
        const std::size_t cov = static_cast<std::size_t>(f.count) * f.cw * f.ch;
        if (at + codes + cov > len || (at & 3))
            return false;
        f.codes = reinterpret_cast<const std::uint32_t *>(d + at);
        f.cov = d + at + codes;
        at = (at + codes + cov + 3) & ~static_cast<std::size_t>(3);
        faces[nfaces++] = f;
    }
    return nfaces > 0;
}

int font_height(Font f) noexcept
{
    return face(f).ch;
}

int font_advance(Font f) noexcept
{
    return face(f).cw;
}

bool image_from_rgba(const std::uint8_t *d, std::size_t len, Image &img) noexcept
{
    if (len < 8)
        return false;
    const std::uint32_t w = rd32(d), h = rd32(d + 4);
    if (!w || !h || w > 4096 || h > 4096 || 8 + static_cast<std::size_t>(w) * h * 4 > len)
        return false;
    img.w = static_cast<int>(w);
    img.h = static_cast<int>(h);
    img.px = reinterpret_cast<const std::uint32_t *>(d + 8);
    return true;
}

void Canvas::clear(std::uint32_t c) noexcept
{
    for (int i = 0; i < kW * kH; ++i)
        px_[i] = c;
}

void Canvas::fill(int x, int y, int w, int h, std::uint32_t c) noexcept
{
    const int x0 = x < 0 ? 0 : x, y0 = y < 0 ? 0 : y;
    const int x1 = x + w > kW ? kW : x + w, y1 = y + h > kH ? kH : y + h;
    for (int yy = y0; yy < y1; ++yy)
        for (int xx = x0; xx < x1; ++xx)
            px_[yy * kW + xx] = c | 0xff000000u;
}

void Canvas::blend(int x, int y, int w, int h, std::uint32_t c, unsigned a) noexcept
{
    const int x0 = x < 0 ? 0 : x, y0 = y < 0 ? 0 : y;
    const int x1 = x + w > kW ? kW : x + w, y1 = y + h > kH ? kH : y + h;
    for (int yy = y0; yy < y1; ++yy)
        for (int xx = x0; xx < x1; ++xx)
            px_[yy * kW + xx] = mix(px_[yy * kW + xx], c, a);
}

void Canvas::put_cov(int x, int y, std::uint32_t c, unsigned cov) noexcept
{
    if (x < 0 || y < 0 || x >= kW || y >= kH || !cov)
        return;
    std::uint32_t &p = px_[y * kW + x];
    p = mix(p, c, cov);
}

void Canvas::round_rect(int x, int y, int w, int h, int r, std::uint32_t c, unsigned a) noexcept
{
    if (r * 2 > h)
        r = h / 2;
    if (r * 2 > w)
        r = w / 2;
    for (int yy = 0; yy < h; ++yy)
    {
        const int py = y + yy;
        if (py < 0 || py >= kH)
            continue;
        for (int xx = 0; xx < w; ++xx)
        {
            // distance outside the rounded corner, for its antialiased edge
            float cov = 1;
            const int cx = xx < r ? r : xx >= w - r ? w - r - 1 : xx;
            const int cy = yy < r ? r : yy >= h - r ? h - r - 1 : yy;
            if (cx != xx && cy != yy)
            {
                const float dx = static_cast<float>(xx - cx), dy = static_cast<float>(yy - cy);
                const float d = fsqrt(dx * dx + dy * dy) - static_cast<float>(r) + 0.5f;
                cov = d <= 0 ? 1 : d >= 1 ? 0 : 1 - d;
            }
            if (cov > 0)
                put_cov(x + xx, py, c, static_cast<unsigned>(cov * static_cast<float>(a)));
        }
    }
}

void Canvas::round_frame(int x, int y, int w, int h, int r, int t, std::uint32_t c) noexcept
{
    // a frame as the difference of two rounded rectangles, drawn as four bands and the corners
    for (int yy = 0; yy < h; ++yy)
        for (int xx = 0; xx < w; ++xx)
        {
            const int cx = xx < r ? r : xx >= w - r ? w - r - 1 : xx;
            const int cy = yy < r ? r : yy >= h - r ? h - r - 1 : yy;
            float d;
            if (cx != xx && cy != yy)
            {
                const float dx = static_cast<float>(xx - cx), dy = static_cast<float>(yy - cy);
                d = static_cast<float>(r) - fsqrt(dx * dx + dy * dy); // inside distance to the edge
            }
            else
            {
                const int e1 = xx < w - 1 - xx ? xx : w - 1 - xx;
                const int e2 = yy < h - 1 - yy ? yy : h - 1 - yy;
                d = static_cast<float>(e1 < e2 ? e1 : e2) + 0.5f;
            }
            float cov = 0;
            if (d > -0.5f && d < static_cast<float>(t) + 0.5f)
            {
                cov = 1;
                if (d < 0.5f)
                    cov = d + 0.5f;
                else if (d > static_cast<float>(t) - 0.5f)
                    cov = static_cast<float>(t) + 0.5f - d;
            }
            if (cov > 0)
                put_cov(x + xx, y + yy, c, static_cast<unsigned>((cov > 1 ? 1 : cov) * 255));
        }
}

void Canvas::circle(int cx, int cy, int r, std::uint32_t c, unsigned a) noexcept
{
    for (int y = -r - 1; y <= r + 1; ++y)
        for (int x = -r - 1; x <= r + 1; ++x)
        {
            const float d = fsqrt(static_cast<float>(x * x + y * y)) - static_cast<float>(r) + 0.5f;
            const float cov = d <= 0 ? 1 : d >= 1 ? 0 : 1 - d;
            if (cov > 0)
                put_cov(cx + x, cy + y, c, static_cast<unsigned>(cov * static_cast<float>(a)));
        }
}

void Canvas::ring(int cx, int cy, int r, int t, std::uint32_t c, float from, float turns) noexcept
{
    const float ro = static_cast<float>(r), ri = static_cast<float>(r - t);
    for (int y = -r - 1; y <= r + 1; ++y)
        for (int x = -r - 1; x <= r + 1; ++x)
        {
            const float dist = fsqrt(static_cast<float>(x * x + y * y));
            float cov = 1;
            if (dist > ro - 0.5f)
                cov = ro + 0.5f - dist;
            else if (dist < ri + 0.5f)
                cov = dist - (ri - 0.5f);
            if (cov <= 0)
                continue;
            if (turns < 1)
            {
                float tt = turn_of(static_cast<float>(x), static_cast<float>(y)) - from;
                if (tt < 0)
                    tt += 1;
                if (tt > turns)
                    continue;
            }
            put_cov(cx + x, cy + y, c, static_cast<unsigned>((cov > 1 ? 1 : cov) * 255));
        }
}

void Canvas::line(int x0, int y0, int x1, int y1, int thick, std::uint32_t c) noexcept
{
    // a thick segment: distance from each pixel in its box to the segment
    const int minx = (x0 < x1 ? x0 : x1) - thick, maxx = (x0 > x1 ? x0 : x1) + thick;
    const int miny = (y0 < y1 ? y0 : y1) - thick, maxy = (y0 > y1 ? y0 : y1) + thick;
    const float vx = static_cast<float>(x1 - x0), vy = static_cast<float>(y1 - y0);
    const float len2 = vx * vx + vy * vy;
    const float half = static_cast<float>(thick) / 2;
    for (int y = miny; y <= maxy; ++y)
        for (int x = minx; x <= maxx; ++x)
        {
            const float px = static_cast<float>(x - x0), py = static_cast<float>(y - y0);
            float t = len2 > 0 ? (px * vx + py * vy) / len2 : 0;
            t = t < 0 ? 0 : t > 1 ? 1 : t;
            const float dx = px - t * vx, dy = py - t * vy;
            const float d = fsqrt(dx * dx + dy * dy) - half + 0.5f;
            const float cov = d <= 0 ? 1 : d >= 1 ? 0 : 1 - d;
            if (cov > 0)
                put_cov(x, y, c, static_cast<unsigned>(cov * 255));
        }
}

void Canvas::image(const Image &img, int x, int y, unsigned alpha) noexcept
{
    if (!img.px)
        return;
    for (int yy = 0; yy < img.h; ++yy)
    {
        const int py = y + yy;
        if (py < 0 || py >= kH)
            continue;
        for (int xx = 0; xx < img.w; ++xx)
        {
            const int px = x + xx;
            if (px < 0 || px >= kW)
                continue;
            const std::uint32_t s = img.px[yy * img.w + xx];
            const unsigned a = (s >> 24) * alpha / 255;
            if (a)
                px_[py * kW + px] = mix(px_[py * kW + px], s & 0xffffffu, a);
        }
    }
}

void Canvas::copy_from(const Image &img) noexcept
{
    if (img.px && img.w == kW && img.h == kH)
        std::memcpy(px_, img.px, sizeof(std::uint32_t) * kW * kH);
    else
        clear(color::bg);
}

namespace
{
// Text is drawn as pieces: a run reference (a pre-rendered word or cluster, i18n.hpp) or plain
// characters from the UI font. A right-to-left language draws a line's pieces in reverse order
// (each piece itself left to right): its words read right to left, names and numbers stay as
// they are.
struct Piece
{
    std::size_t at, len; // bytes in the string
    int run_table, run_id; // run_id < 0: plain characters
    int width;
};
constexpr int kMaxPieces = 384;

int run_size(Font f) noexcept
{
    return f == Font::small ? 0 : f == Font::body ? 1 : 2;
}

int plain_width(const Face &fc, const char *s, std::size_t len) noexcept
{
    int n = 0;
    for (std::size_t i = 0; i < len;)
    {
        json::utf8_next(s, len, &i);
        ++n;
    }
    return n * fc.cw;
}

// Splits s into pieces; plain text is cut at spaces (each space its own piece) so lines can
// break there. Returns how many; *has_run says whether any run is in it.
int pieces(Font f, const char *s, Piece *out, int cap, bool *has_run) noexcept
{
    const Face &fc = face(f);
    const std::size_t len = std::strlen(s);
    int n = 0;
    *has_run = false;
    std::size_t i = 0;
    while (i < len && n < cap)
    {
        int table = 0, id = 0;
        if (i18n::run_ref(s + i, len - i, &table, &id))
        {
            i18n::Run r;
            const int w = i18n::run(table, id, run_size(f), r) ? r.w : 0;
            out[n++] = Piece{i, i18n::kRunRef, table, id, w};
            *has_run = true;
            i += i18n::kRunRef;
            continue;
        }
        std::size_t j = i;
        if (s[i] == ' ')
            ++j;
        else
            while (j < len && s[j] != ' ')
            {
                int t2, id2;
                if (i18n::run_ref(s + j, len - j, &t2, &id2))
                    break;
                json::utf8_next(s, len, &j);
            }
        out[n++] = Piece{i, j - i, 0, -1, plain_width(fc, s + i, j - i)};
        i = j;
    }
    return n;
}
} // namespace

void Canvas::draw_run(int table, int id, Font f, int x, int y, std::uint32_t c) noexcept
{
    i18n::Run r;
    if (!i18n::run(table, id, run_size(f), r))
        return;
    // Line the run's baseline up with the font's (the same for small, body and title).
    const Face &fc = face(f);
    const int top = y + fc.base - r.base;
    int px = 0, py = 0;
    for (std::size_t k = 0; k < r.len && py < r.h; ++k)
    {
        const unsigned level = r.rle[k] >> 4, rep = (r.rle[k] & 15u) + 1;
        for (unsigned m = 0; m < rep; ++m)
        {
            if (level)
                put_cov(x + px, top + py, c, level * 17);
            if (++px >= r.w)
                px = 0, ++py;
        }
    }
}

void Canvas::draw_plain(Font f, int x, int y, const char *s, std::size_t len, std::uint32_t c) noexcept
{
    const Face &fc = face(f);
    int pen = x;
    for (std::size_t i = 0; i < len;)
    {
        const std::uint32_t cp = json::utf8_next(s, len, &i);
        const std::uint8_t *g = glyph(fc, cp);
        if (!g)
            g = glyph(fc, '?');
        if (g && cp != ' ')
            for (int gy = 0; gy < fc.ch; ++gy)
                for (int gx = 0; gx < fc.cw; ++gx)
                    put_cov(pen + gx, y + gy, c, g[gy * fc.cw + gx]);
        pen += fc.cw;
    }
}

int Canvas::text_width(Font f, const char *s) noexcept
{
    static Piece p[kMaxPieces];
    bool has_run;
    const int n = pieces(f, s, p, kMaxPieces, &has_run);
    int w = 0;
    for (int i = 0; i < n; ++i)
        w += p[i].width;
    return w;
}

int Canvas::draw_line(Font f, int x, int y, const char *s, const void *pv, int first, int count, bool rtl,
                      std::uint32_t c) noexcept
{
    const auto *p = static_cast<const Piece *>(pv);
    int pen = x;
    auto one = [&](const Piece &pc) {
        if (pc.run_id >= 0)
            draw_run(pc.run_table, pc.run_id, f, pen, y, c);
        else
            draw_plain(f, pen, y, s + pc.at, pc.len, c);
        pen += pc.width;
    };
    if (!rtl)
    {
        for (int k = 0; k < count; ++k)
            one(p[first + k]);
        return pen - x;
    }
    // Right to left: the runs (words) from the last to the first. Plain text between runs is cut
    // into left-to-right islands (from a letter or digit to the last one: "PS5Upload Play",
    // "192.168.1.20:19113") kept whole, and single neutral characters (spaces, punctuation) that
    // go right to left with the words, brackets mirrored. A simple form of the Unicode bidi rules
    // that is enough for the app's sentences (as PS5Term does it).
    const Face &fc = face(f);
    // a letter or digit of the UI font's left-to-right scripts (not typographic punctuation)
    auto letter_at = [s](std::size_t i, std::size_t end) {
        std::size_t j = i;
        const std::uint32_t cp = json::utf8_next(s, end, &j);
        return (cp >= '0' && cp <= '9') || (cp >= 'a' && cp <= 'z') || (cp >= 'A' && cp <= 'Z') ||
               (cp >= 0xC0 && cp < 0x2000 && cp != 0xD7 && cp != 0xF7);
    };
    int k = first + count;
    while (k > first)
    {
        if (p[k - 1].run_id >= 0)
        {
            one(p[--k]);
            continue;
        }
        int start = k - 1;
        while (start > first && p[start - 1].run_id < 0)
            --start;
        // the group's bytes, contiguous in s
        const std::size_t gs = p[start].at, ge = p[k - 1].at + p[k - 1].len;
        struct Unit
        {
            std::size_t at, len;
        };
        Unit units[160];
        int nu = 0;
        for (std::size_t i = gs; i < ge && nu < 160;)
        {
            if (letter_at(i, ge))
            {
                // an island: up to the last letter or digit before a run of neutrals at its end
                std::size_t j = i, last = i;
                while (j < ge)
                {
                    const auto cj = static_cast<unsigned char>(s[j]);
                    if (letter_at(j, ge))
                    {
                        json::utf8_next(s, ge, &j);
                        last = j;
                    }
                    else if (cj == ' ' || cj == '.' || cj == ':' || cj == '-' || cj == '_' || cj == '/' || cj == '\'' ||
                             cj == '@' || cj == ',')
                        ++j;
                    else
                        break;
                }
                units[nu++] = Unit{i, last - i};
                i = last;
            }
            else
            {
                std::size_t j = i;
                json::utf8_next(s, ge, &j);
                units[nu++] = Unit{i, j - i};
                i = j;
            }
        }
        for (int u = nu - 1; u >= 0; --u)
        {
            const char *t = s + units[u].at;
            std::size_t len = units[u].len;
            char mirrored[2] = {0, 0};
            if (len == 1)
            {
                const char m = *t == '(' ? ')' : *t == ')' ? '(' : *t == '[' ? ']' : *t == ']' ? '[' : *t == '<' ? '>' : *t == '>' ? '<' : 0;
                if (m)
                    mirrored[0] = m, t = mirrored;
            }
            draw_plain(f, pen, y, t, len, c);
            pen += plain_width(fc, t, len);
        }
        k = start;
    }
    return pen - x;
}

int Canvas::text(Font f, int x, int y, const char *s, std::uint32_t c, int max_w) noexcept
{
    const Face &fc = face(f);
    if (!fc.count)
        return 0;
    static Piece p[kMaxPieces];
    bool has_run;
    int n = pieces(f, s, p, kMaxPieces, &has_run);
    const bool rtl = has_run && i18n::right_to_left();
    int total = 0;
    for (int i = 0; i < n; ++i)
        total += p[i].width;
    if (max_w <= 0 || total <= max_w)
        return draw_line(f, x, y, s, p, 0, n, rtl, c);
    // Too long: as many pieces as fit (a plain piece cut by characters), then "…".
    const int room = max_w - fc.cw;
    int used = 0, k = 0;
    while (k < n && used + p[k].width <= room)
        used += p[k++].width;
    if (k < n && p[k].run_id < 0 && room - used >= fc.cw)
    {
        // part of the next plain piece
        std::size_t take = 0;
        int w = 0;
        const char *ps = s + p[k].at;
        while (take < p[k].len && used + w + fc.cw <= room)
        {
            json::utf8_next(ps, p[k].len, &take);
            w += fc.cw;
        }
        p[k].len = take;
        p[k].width = w;
        used += w;
        ++k;
    }
    int drawn;
    if (rtl)
    {
        // the ellipsis on the left, where the cut end of a right-to-left line is
        draw_plain(f, x, y, "\xe2\x80\xa6", 3, c);
        drawn = fc.cw + draw_line(f, x + fc.cw, y, s, p, 0, k, true, c);
    }
    else
    {
        drawn = draw_line(f, x, y, s, p, 0, k, false, c);
        draw_plain(f, x + drawn, y, "\xe2\x80\xa6", 3, c);
        drawn += fc.cw;
    }
    return drawn;
}

int Canvas::text_center(Font f, int cx, int y, const char *s, std::uint32_t c) noexcept
{
    return text(f, cx - text_width(f, s) / 2, y, s, c);
}

int Canvas::text_wrap(Font f, int x, int y, int max_w, const char *s, std::uint32_t c, int gap) noexcept
{
    const Face &fc = face(f);
    if (!fc.count)
        return 0;
    static Piece p[kMaxPieces];
    bool has_run;
    const int n = pieces(f, s, p, kMaxPieces, &has_run);
    const bool rtl = has_run && i18n::right_to_left();
    int lines = 0, i = 0;
    while (i < n)
    {
        // skip spaces at the start of a line
        while (i < n && p[i].run_id < 0 && p[i].len == 1 && s[p[i].at] == ' ')
            ++i;
        if (i >= n)
            break;
        int w = 0, j = i;
        while (j < n && (j == i || w + p[j].width <= max_w))
            w += p[j++].width;
        // drop spaces at the end of the line
        int end = j;
        while (end > i + 1 && p[end - 1].run_id < 0 && p[end - 1].len == 1 && s[p[end - 1].at] == ' ')
            --end;
        draw_line(f, x, y + lines * (fc.ch + gap), s, p, i, end - i, rtl, c);
        ++lines;
        i = j;
    }
    return lines * (fc.ch + gap);
}

int Canvas::button(Glyph g, int x, int y, int size) noexcept
{
    const int r = size / 2, cx = x + r, cy = y + r;
    const int t = size / 12 + 2;
    switch (g)
    {
    case Glyph::cross:
    case Glyph::circle:
    case Glyph::square:
    case Glyph::triangle:
    {
        circle(cx, cy, r, color::panel_hi);
        const int s = size * 2 / 9;
        if (g == Glyph::cross)
        {
            line(cx - s, cy - s, cx + s, cy + s, t, color::cross);
            line(cx - s, cy + s, cx + s, cy - s, t, color::cross);
        }
        else if (g == Glyph::circle)
            ring(cx, cy, s + t / 2, t, color::circle);
        else if (g == Glyph::square)
            round_frame(cx - s, cy - s, s * 2, s * 2, 2, t, color::square);
        else
        {
            line(cx, cy - s, cx - s, cy + s * 3 / 4, t, color::triangle);
            line(cx - s, cy + s * 3 / 4, cx + s, cy + s * 3 / 4, t, color::triangle);
            line(cx + s, cy + s * 3 / 4, cx, cy - s, t, color::triangle);
        }
        return size;
    }
    default:
        break;
    }
    // Labelled keys: a rounded pill with a short word.
    const char *label = g == Glyph::options ? "OPTIONS" : g == Glyph::touchpad ? "TOUCHPAD"
                        : g == Glyph::l1     ? "L1"
                        : g == Glyph::r1     ? "R1"
                                             : "D-PAD";
    const int tw = text_width(Font::small, label);
    const int w = tw + size / 2;
    round_rect(x, y + size / 8, w, size * 3 / 4, size * 3 / 8, color::panel_hi);
    text(Font::small, x + (w - tw) / 2, y + size / 2 - font_height(Font::small) / 2, label, color::text);
    return w;
}

int Canvas::qr(const std::uint8_t *code, int x, int y, int module) noexcept
{
    const int n = qrcodegen_getSize(code);
    const int quiet = 2;
    const int side = (n + quiet * 2) * module;
    fill(x, y, side, side, color::white);
    for (int my = 0; my < n; ++my)
        for (int mx = 0; mx < n; ++mx)
            if (qrcodegen_getModule(code, mx, my))
                fill(x + (mx + quiet) * module, y + (my + quiet) * module, module, module, color::black);
    return side;
}
} // namespace play::ui
