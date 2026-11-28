// PS5Upload Play - the on-screen keyboard for short text (a room password, your name, a chat line,
// a game to look for): 10 keys a row, letters / capitals / digits and symbols, controller-first.
// ✕ types the key, □ deletes, △ is a space, L1 switches letters and symbols, R1 capitals, R2 or
// OPTIONS is Done. The cursor wraps; Done is also the last key of the bottom row. Pure logic.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::textkb
{
constexpr int kCols = 10, kRows = 4; // the 4th row: Shift, Symbols, Space (4 keys wide), Delete, Done
enum Special : int
{
    k_char = 0,
    k_shift,
    k_symbols,
    k_space,
    k_delete,
    k_done,
};

class Keyboard
{
  public:
    void reset(const char *text, int max_chars, bool secret) noexcept;
    // Moves: dx, dy in -1/0/1.
    void move(int dx, int dy) noexcept;
    // ✕ on the focused key; returns true when it was Done.
    bool press() noexcept;
    void type(const char *utf8) noexcept;
    void del() noexcept;
    void space() noexcept
    {
        type(" ");
    }
    void toggle_shift() noexcept
    {
        shift_ = !shift_;
    }
    void toggle_symbols() noexcept
    {
        symbols_ = !symbols_;
    }

    const char *text() const noexcept
    {
        return text_;
    }
    int chars() const noexcept;
    int row() const noexcept
    {
        return row_;
    }
    int col() const noexcept
    {
        return col_;
    }
    bool shift() const noexcept
    {
        return shift_;
    }
    bool symbols() const noexcept
    {
        return symbols_;
    }
    bool secret() const noexcept
    {
        return secret_;
    }
    // The key at a place: its label (a character, or "" for a special key) and kind.
    Special key(int row, int col, const char **label) const noexcept;
    // The text as shown: dots for a secret (the last character typed stays visible 1 s: shown()).
    void shown(char *out, std::size_t cap, bool last_visible) const noexcept;

  private:
    char text_[4 * 64 + 1] = {};
    int max_ = 64;
    int row_ = 0, col_ = 0;
    bool shift_ = false, symbols_ = false, secret_ = false;
};
} // namespace play::textkb
