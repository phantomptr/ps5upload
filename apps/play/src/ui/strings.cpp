// PS5Upload Play - the strings table.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "strings.hpp"

namespace play::ui
{
namespace
{
const char *const kEnglish[] = {
#define PLAY_STR_TEXT(id, text) text,
    PLAY_STRINGS(PLAY_STR_TEXT)
#undef PLAY_STR_TEXT
};
static_assert(sizeof kEnglish / sizeof kEnglish[0] == static_cast<int>(S::count_));
} // namespace

const char *tr(S id) noexcept
{
    const int i = static_cast<int>(id);
    return i >= 0 && i < static_cast<int>(S::count_) ? kEnglish[i] : "";
}
} // namespace play::ui
