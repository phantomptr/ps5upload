// PS5Upload Play - drawing into a linear 1920x1080 RGBA picture (R in the low byte, as the
// console's framebuffer takes it). Portable: the same drawing renders screenshots on a computer.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::ui
{
constexpr int kW = 1920, kH = 1080;

constexpr std::uint32_t rgb(unsigned r, unsigned g, unsigned b, unsigned a = 255) noexcept
{
    return (a << 24) | (b << 16) | (g << 8) | r;
}

// The look: deep navy, an indigo accent (the home-screen art's glow), soft white text.
namespace color
{
constexpr std::uint32_t bg = rgb(8, 9, 20), panel = rgb(22, 24, 46), panel_hi = rgb(38, 40, 78),
                        accent = rgb(110, 104, 255), accent_dim = rgb(64, 60, 160), text = rgb(236, 238, 250),
                        muted = rgb(150, 154, 186), good = rgb(80, 210, 140), warn = rgb(255, 190, 90),
                        bad = rgb(255, 110, 110), white = rgb(255, 255, 255), black = rgb(0, 0, 0),
                        cross = rgb(140, 170, 255), circle = rgb(255, 120, 130), square = rgb(240, 150, 220),
                        triangle = rgb(90, 220, 190);
}

enum class Font : std::uint8_t
{
    small = 0, // 24 px
    body = 1,  // 32 px
    title = 2, // 44 px
    big = 3,   // 72 px (ASCII)
    huge = 4,  // 112 px (ASCII)
};

// font.bin (tools/make-font.py), loaded once.
bool font_load(const std::uint8_t *data, std::size_t len) noexcept;
int font_height(Font f) noexcept;
int font_advance(Font f) noexcept;

struct Image
{
    int w = 0, h = 0;
    const std::uint32_t *px = nullptr; // straight-alpha RGBA, R low byte
};
// "w h" u32 LE header, then RGBA (tools/assets/make-art.py's .rgba files).
bool image_from_rgba(const std::uint8_t *data, std::size_t len, Image &img) noexcept;

class Canvas
{
  public:
    explicit Canvas(std::uint32_t *px) noexcept : px_{px}
    {
    }
    std::uint32_t *pixels() noexcept
    {
        return px_;
    }
    void clear(std::uint32_t c) noexcept;
    void fill(int x, int y, int w, int h, std::uint32_t c) noexcept;            // opaque
    void blend(int x, int y, int w, int h, std::uint32_t c, unsigned a) noexcept; // a 0-255 over
    void round_rect(int x, int y, int w, int h, int r, std::uint32_t c, unsigned a = 255) noexcept;
    void round_frame(int x, int y, int w, int h, int r, int thick, std::uint32_t c) noexcept;
    void circle(int cx, int cy, int r, std::uint32_t c, unsigned a = 255) noexcept;
    void ring(int cx, int cy, int r, int thick, std::uint32_t c, float from_turn = 0, float turns = 1) noexcept;
    void line(int x0, int y0, int x1, int y1, int thick, std::uint32_t c) noexcept;
    void image(const Image &img, int x, int y, unsigned alpha = 255) noexcept;
    // The whole picture copied from a background image of the same size.
    void copy_from(const Image &img) noexcept;

    // UTF-8 text; returns its width. max_w > 0: cut with "…" to fit.
    int text(Font f, int x, int y, const char *s, std::uint32_t c, int max_w = 0) noexcept;
    static int text_width(Font f, const char *s) noexcept;
    int text_center(Font f, int cx, int y, const char *s, std::uint32_t c) noexcept;
    // Wrapped into lines of max_w; returns the height used.
    int text_wrap(Font f, int x, int y, int max_w, const char *s, std::uint32_t c, int line_gap = 6) noexcept;

    // The controller's face buttons as little coloured symbols (✕ ○ □ △) in a dark disc.
    enum class Glyph
    {
        cross,
        circle,
        square,
        triangle,
        options,
        touchpad,
        l1,
        r1,
        dpad,
    };
    int button(Glyph g, int x, int y, int size) noexcept; // returns its width

    // A QR code from qrcodegen's buffer, `module` pixels per module, with a quiet zone.
    int qr(const std::uint8_t *qrcode, int x, int y, int module) noexcept;

  private:
    void put_cov(int x, int y, std::uint32_t c, unsigned cov) noexcept;
    std::uint32_t *px_;
};
} // namespace play::ui
