// PS5Upload Play - the on-screen keyboard (textkb.hpp).
// SPDX-License-Identifier: GPL-3.0-or-later
#include "textkb.hpp"

#include "json.hpp"

#include <cstdio>
#include <cstring>

namespace play::textkb
{
namespace
{
// Three rows of 10 keys for each page.
const char *const kLetters[3] = {"qwertyuiop", "asdfghjkl'", "zxcvbnm,.?"};
const char *const kCaps[3] = {"QWERTYUIOP", "ASDFGHJKL'", "ZXCVBNM,.!"};
const char *const kSymbols[3] = {"1234567890", "-_@#&*()+=", "/:;!\"%<>~$"};
// One character as a string, per page and place (static storage for labels).
char cells[3][3][10][2];
bool cells_ready = false;

void make_cells() noexcept
{
    if (cells_ready)
        return;
    const char *const *pages[3] = {kLetters, kCaps, kSymbols};
    for (int p = 0; p < 3; ++p)
        for (int r = 0; r < 3; ++r)
            for (int c = 0; c < 10; ++c)
            {
                cells[p][r][c][0] = pages[p][r][c];
                cells[p][r][c][1] = 0;
            }
    cells_ready = true;
}
} // namespace

void Keyboard::reset(const char *text, int max_chars, bool secret) noexcept
{
    std::snprintf(text_, sizeof text_, "%s", text ? text : "");
    max_ = max_chars < 1 ? 1 : max_chars > 64 ? 64 : max_chars;
    row_ = col_ = 0;
    shift_ = symbols_ = false;
    secret_ = secret;
}

int Keyboard::chars() const noexcept
{
    const std::size_t len = std::strlen(text_);
    std::size_t i = 0;
    int n = 0;
    while (i < len)
    {
        json::utf8_next(text_, len, &i);
        ++n;
    }
    return n;
}

Special Keyboard::key(int row, int col, const char **label) const noexcept
{
    make_cells();
    *label = "";
    if (row < 3)
    {
        const int page = symbols_ ? 2 : shift_ ? 1 : 0;
        *label = cells[page][row][col];
        return k_char;
    }
    if (col == 0)
        return k_shift;
    if (col == 1)
        return k_symbols;
    if (col <= 6)
        return k_space;
    if (col <= 8)
        return k_delete;
    return k_done;
}

void Keyboard::move(int dx, int dy) noexcept
{
    if (dy)
    {
        row_ = (row_ + dy + kRows) % kRows;
        // the wide keys of the bottom row: land on their first column
        if (row_ == 3)
            col_ = col_ <= 1 ? col_ : col_ <= 6 ? 2 : col_ <= 8 ? 7 : 9;
    }
    if (dx)
    {
        if (row_ == 3)
        {
            // Shift, Symbols, Space, Delete, Done as five keys
            static const int starts[5] = {0, 1, 2, 7, 9};
            int at = col_ <= 1 ? col_ : col_ <= 6 ? 2 : col_ <= 8 ? 3 : 4;
            at = (at + dx + 5) % 5;
            col_ = starts[at];
        }
        else
            col_ = (col_ + dx + kCols) % kCols;
    }
}

void Keyboard::type(const char *s) noexcept
{
    if (!s || !*s)
        return;
    const std::size_t len = std::strlen(text_), add = std::strlen(s);
    if (chars() >= max_ || len + add + 1 > sizeof text_)
        return;
    std::memcpy(text_ + len, s, add + 1);
}

void Keyboard::del() noexcept
{
    std::size_t len = std::strlen(text_);
    if (!len)
        return;
    // one whole character
    --len;
    while (len && (static_cast<unsigned char>(text_[len]) & 0xc0) == 0x80)
        --len;
    text_[len] = 0;
}

bool Keyboard::press() noexcept
{
    const char *label;
    switch (key(row_, col_, &label))
    {
    case k_char:
        type(label);
        // one capital, then back to small letters
        if (shift_ && !symbols_)
            shift_ = false;
        return false;
    case k_shift: shift_ = !shift_; return false;
    case k_symbols: symbols_ = !symbols_; return false;
    case k_space: space(); return false;
    case k_delete: del(); return false;
    case k_done: return true;
    }
    return false;
}

void Keyboard::shown(char *out, std::size_t cap, bool last_visible) const noexcept
{
    if (!secret_)
    {
        std::snprintf(out, cap, "%s", text_);
        return;
    }
    const int n = chars();
    std::size_t o = 0;
    for (int i = 0; i < n - (last_visible ? 1 : 0) && o + 4 < cap; ++i)
    {
        // U+2022 bullet
        out[o++] = static_cast<char>(0xe2);
        out[o++] = static_cast<char>(0x80);
        out[o++] = static_cast<char>(0xa2);
    }
    out[o] = 0;
    if (last_visible && n)
    {
        const std::size_t len = std::strlen(text_);
        std::size_t at = len - 1;
        while (at && (static_cast<unsigned char>(text_[at]) & 0xc0) == 0x80)
            --at;
        std::snprintf(out + o, cap - o, "%s", text_ + at);
    }
}
} // namespace play::textkb
