// PS5Upload Play - the current language's words and pre-rendered runs.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "i18n.hpp"

#include "strings.hpp"

#include <cstdlib>
#include <cstring>

namespace play::ui::i18n
{
namespace
{
const char *const kCodes[] = {"en", "ar", "bn", "de", "es", "fa", "fr", "hi", "hu", "id", "it",
                              "ja", "ko", "pl", "pt-BR", "ru", "th", "tr", "vi", "zh-CN", "zh-TW"};
constexpr int kCount = sizeof kCodes / sizeof kCodes[0];

struct File
{
    const std::uint8_t *data = nullptr;
    std::size_t len = 0;
    std::uint32_t strings = 0, runs = 0, flags = 0, sizes = 0;
    const std::uint8_t *string_table = nullptr, *run_table = nullptr, *text = nullptr, *pixels = nullptr;
    char *copies = nullptr;           // every string, NUL-terminated
    const char **index = nullptr;     // per string, into copies (null: English)
};
File lang, names;

std::uint32_t rd32(const std::uint8_t *p) noexcept
{
    return static_cast<std::uint32_t>(p[0]) | p[1] << 8 | p[2] << 16 | static_cast<std::uint32_t>(p[3]) << 24;
}
std::uint16_t rd16(const std::uint8_t *p) noexcept
{
    return static_cast<std::uint16_t>(p[0] | p[1] << 8);
}

void drop(File &f) noexcept
{
    std::free(f.copies);
    std::free(f.index);
    f = File{};
}

bool open(File &f, const std::uint8_t *d, std::size_t len) noexcept
{
    drop(f);
    if (!d || len < 20 || std::memcmp(d, "PPL1", 4) != 0)
        return false;
    File x;
    x.data = d;
    x.len = len;
    x.strings = rd32(d + 4);
    x.runs = rd32(d + 8);
    x.flags = rd32(d + 12);
    x.sizes = rd32(d + 16);
    if (x.sizes != 3 || x.strings > 100000 || x.runs > 0xFFFF)
        return false;
    std::size_t at = 20 + 4 * x.sizes;
    x.string_table = d + at;
    at += 8u * x.strings;
    x.run_table = d + at;
    at += 16u * x.runs * x.sizes;
    if (at > len)
        return false;
    x.text = d + at;
    std::size_t text_len = 0;
    for (std::uint32_t i = 0; i < x.strings; ++i)
    {
        const std::uint32_t off = rd32(x.string_table + 8 * i), n = rd32(x.string_table + 8 * i + 4);
        if (off + n > text_len)
            text_len = off + n;
    }
    if (at + text_len > len)
        return false;
    x.pixels = x.text + text_len;
    x.copies = static_cast<char *>(std::malloc(text_len + x.strings + 1));
    x.index = static_cast<const char **>(std::calloc(x.strings + 1, sizeof(const char *)));
    if (!x.copies || !x.index)
    {
        std::free(x.copies);
        std::free(x.index);
        return false;
    }
    std::size_t o = 0;
    for (std::uint32_t i = 0; i < x.strings; ++i)
    {
        const std::uint32_t off = rd32(x.string_table + 8 * i), n = rd32(x.string_table + 8 * i + 4);
        if (!n)
            continue;
        std::memcpy(x.copies + o, x.text + off, n);
        x.copies[o + n] = 0;
        x.index[i] = x.copies + o;
        o += n + 1;
    }
    f = x;
    return true;
}

int hexval(char c) noexcept
{
    return c >= '0' && c <= '9' ? c - '0' : c >= 'a' && c <= 'f' ? c - 'a' + 10 : c >= 'A' && c <= 'F' ? c - 'A' + 10 : -1;
}
} // namespace

int count() noexcept
{
    return kCount;
}

const char *code(int language) noexcept
{
    return language >= 0 && language < kCount ? kCodes[language] : "en";
}

int find(const char *c) noexcept
{
    for (int i = 0; i < kCount; ++i)
        if (c && !std::strcmp(c, kCodes[i]))
            return i;
    return -1;
}

int from_system_id(int id) noexcept
{
    // SCE_SYSTEM_SERVICE_PARAM_ID_LANG values (as PS5Term maps them); regional variants share
    // one translation. The PS5 has no Hindi, Bengali or Persian system language: those are
    // picked in Settings.
    const char *want;
    switch (id)
    {
    case 0: want = "ja"; break;
    case 2: case 22: want = "fr"; break;
    case 3: case 20: want = "es"; break;
    case 4: want = "de"; break;
    case 5: want = "it"; break;
    case 7: case 17: want = "pt-BR"; break;
    case 8: want = "ru"; break;
    case 9: want = "ko"; break;
    case 10: want = "zh-TW"; break;
    case 11: want = "zh-CN"; break;
    case 16: want = "pl"; break;
    case 19: want = "tr"; break;
    case 21: want = "ar"; break;
    case 24: want = "hu"; break;
    case 27: want = "th"; break;
    case 28: want = "vi"; break;
    case 29: want = "id"; break;
    default: want = "en"; break;
    }
    const int i = find(want);
    return i < 0 ? 0 : i;
}

bool load(const std::uint8_t *data, std::size_t len) noexcept
{
    return open(lang, data, len);
}

void use_english() noexcept
{
    drop(lang);
}

bool load_names(const std::uint8_t *data, std::size_t len) noexcept
{
    return open(names, data, len);
}

bool right_to_left() noexcept
{
    return lang.data && (lang.flags & 1);
}

const char *text(int index) noexcept
{
    if (!lang.data || index < 0 || static_cast<std::uint32_t>(index) >= lang.strings)
        return nullptr;
    return lang.index[index];
}

const char *name(int language) noexcept
{
    if (names.data && language >= 0 && static_cast<std::uint32_t>(language) < names.strings && names.index[language])
        return names.index[language];
    return code(language);
}

bool run(int table, int id, int size, Run &out) noexcept
{
    const File &f = table ? names : lang;
    if (!f.data || id < 0 || static_cast<std::uint32_t>(id) >= f.runs || size < 0 || static_cast<std::uint32_t>(size) >= f.sizes)
        return false;
    const std::uint8_t *e = f.run_table + 16u * (static_cast<std::uint32_t>(id) * f.sizes + static_cast<std::uint32_t>(size));
    out.w = rd16(e);
    out.h = rd16(e + 2);
    out.base = rd16(e + 4);
    const std::uint32_t off = rd32(e + 8), n = rd32(e + 12);
    if (f.pixels + off + n > f.data + f.len)
        return false;
    out.rle = f.pixels + off;
    out.len = n;
    return true;
}

bool run_ref(const char *s, std::size_t n, int *table, int *id) noexcept
{
    // U+E000 = EE 80 80, U+E002 = EE 80 82
    if (n < kRunRef || static_cast<unsigned char>(s[0]) != 0xEE || static_cast<unsigned char>(s[1]) != 0x80)
        return false;
    const unsigned char c = static_cast<unsigned char>(s[2]);
    if (c != 0x80 && c != 0x82)
        return false;
    int v = 0;
    for (int k = 0; k < 4; ++k)
    {
        const int h = hexval(s[3 + k]);
        if (h < 0)
            return false;
        v = v << 4 | h;
    }
    *table = c == 0x82 ? 1 : 0;
    *id = v;
    return true;
}
} // namespace play::ui::i18n
