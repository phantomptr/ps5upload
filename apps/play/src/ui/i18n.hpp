// PS5Upload Play - the current language's words, and the pictures of the words the UI font has
// no letters for (tools/i18n.py makes both into assets/i18n/<code>.bin).
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::ui::i18n
{
// PS5Upload's 21 languages, in tools/i18n.py's order; 0 is English.
int count() noexcept;
const char *code(int language) noexcept;
int find(const char *code) noexcept; // -1 when not one of them
// The PS5's system language id (SCE_SYSTEM_SERVICE_PARAM_ID_LANG) as one of ours, or English.
int from_system_id(int id) noexcept;

// Uses a language file (the bytes must stay alive); false (English) when it can't be read.
bool load(const std::uint8_t *data, std::size_t len) noexcept;
void use_english() noexcept;
// How many strings the loaded language file has (0: English).
std::uint32_t strings() noexcept;
bool load_names(const std::uint8_t *data, std::size_t len) noexcept;
bool right_to_left() noexcept;

// The translation of string `index` (strings.hpp order), or null (English).
const char *text(int index) noexcept;
// A language's own name (names.bin), or its code.
const char *name(int language) noexcept;

// A pre-rendered run: table 0 = the language's, 1 = the names'. size: 0 small, 1 body, 2 title.
struct Run
{
    int w = 0, h = 0, base = 0;
    const std::uint8_t *rle = nullptr; // level << 4 | (repeat - 1), row by row
    std::size_t len = 0;
};
bool run(int table, int id, int size, Run &out) noexcept;
// U+E000 (the language's runs) and U+E002 (the names') start a run reference: 3 bytes of
// UTF-8 then 4 hex digits.
constexpr std::size_t kRunRef = 7;
// At s (length left n): a run reference? Gives its table and id.
bool run_ref(const char *s, std::size_t n, int *table, int *id) noexcept;
} // namespace play::ui::i18n
