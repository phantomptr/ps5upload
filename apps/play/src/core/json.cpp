// PS5Upload Play - a small JSON reader and writer.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "json.hpp"

#include <cstdio>
#include <cstring>

namespace play::json
{
namespace
{
constexpr int kMaxDepth = 32;

struct Parser
{
    const char *s;
    std::size_t len, i = 0;
    Token *tok;
    std::uint32_t cap, n = 0;

    void ws() noexcept
    {
        while (i < len && (s[i] == ' ' || s[i] == '\t' || s[i] == '\n' || s[i] == '\r'))
            ++i;
    }
    int add(Type t) noexcept
    {
        if (n >= cap)
            return -1;
        tok[n] = {};
        tok[n].type = t;
        tok[n].start = static_cast<std::uint32_t>(i);
        return static_cast<int>(n++);
    }
    bool lit(const char *w) noexcept
    {
        const std::size_t l = std::strlen(w);
        if (len - i < l || std::memcmp(s + i, w, l) != 0)
            return false;
        i += l;
        return true;
    }
    bool string() noexcept
    {
        // at the opening quote
        ++i;
        const int t = add(Type::string);
        if (t < 0)
            return false;
        tok[t].start = static_cast<std::uint32_t>(i);
        while (i < len)
        {
            const char c = s[i];
            if (c == '"')
            {
                tok[t].len = static_cast<std::uint32_t>(i - tok[t].start);
                tok[t].end = n;
                ++i;
                return true;
            }
            if (static_cast<unsigned char>(c) < 0x20)
                return false;
            if (c == '\\')
            {
                if (++i >= len)
                    return false;
                const char e = s[i];
                if (e == 'u')
                {
                    for (int k = 1; k <= 4; ++k)
                    {
                        if (i + k >= len)
                            return false;
                        const char h = s[i + k];
                        if (!((h >= '0' && h <= '9') || (h >= 'a' && h <= 'f') || (h >= 'A' && h <= 'F')))
                            return false;
                    }
                    i += 4;
                }
                else if (!std::strchr("\"\\/bfnrt", e))
                    return false;
            }
            ++i;
        }
        return false;
    }
    bool number() noexcept
    {
        const int t = add(Type::number);
        if (t < 0)
            return false;
        const std::size_t b = i;
        if (i < len && s[i] == '-')
            ++i;
        bool digits = false;
        while (i < len && s[i] >= '0' && s[i] <= '9')
            ++i, digits = true;
        if (i < len && s[i] == '.')
        {
            ++i;
            while (i < len && s[i] >= '0' && s[i] <= '9')
                ++i, digits = true;
        }
        if (i < len && (s[i] == 'e' || s[i] == 'E'))
        {
            ++i;
            if (i < len && (s[i] == '+' || s[i] == '-'))
                ++i;
            while (i < len && s[i] >= '0' && s[i] <= '9')
                ++i;
        }
        tok[t].len = static_cast<std::uint32_t>(i - b);
        tok[t].end = n;
        return digits;
    }
    bool value(int depth) noexcept
    {
        if (depth > kMaxDepth)
            return false;
        ws();
        if (i >= len)
            return false;
        const char c = s[i];
        if (c == '{' || c == '[')
        {
            const bool obj = c == '{';
            const int t = add(obj ? Type::object : Type::array);
            if (t < 0)
                return false;
            ++i;
            ws();
            if (i < len && s[i] == (obj ? '}' : ']'))
            {
                ++i;
                tok[t].len = static_cast<std::uint32_t>(i - tok[t].start);
                tok[t].end = n;
                return true;
            }
            for (;;)
            {
                ws();
                if (obj)
                {
                    if (i >= len || s[i] != '"' || !string())
                        return false;
                    ws();
                    if (i >= len || s[i] != ':')
                        return false;
                    ++i;
                    tok[t].count += 1;
                }
                if (!value(depth + 1))
                    return false;
                tok[t].count += 1;
                ws();
                if (i < len && s[i] == ',')
                {
                    ++i;
                    continue;
                }
                if (i < len && s[i] == (obj ? '}' : ']'))
                {
                    ++i;
                    tok[t].len = static_cast<std::uint32_t>(i - tok[t].start);
                    tok[t].end = n;
                    return true;
                }
                return false;
            }
        }
        if (c == '"')
            return string();
        if (c == 't' || c == 'f')
        {
            const int t = add(Type::boolean);
            if (t < 0 || !(lit("true") || lit("false")))
                return false;
            tok[t].len = static_cast<std::uint32_t>(i - tok[t].start);
            tok[t].end = n;
            return true;
        }
        if (c == 'n')
        {
            const int t = add(Type::null);
            if (t < 0 || !lit("null"))
                return false;
            tok[t].len = 4;
            tok[t].end = n;
            return true;
        }
        if (c == '-' || (c >= '0' && c <= '9'))
            return number();
        return false;
    }
};

int hexval(char h) noexcept
{
    return h <= '9' ? h - '0' : (h | 0x20) - 'a' + 10;
}

std::size_t put_utf8(char *o, std::uint32_t cp) noexcept
{
    if (cp < 0x80)
    {
        o[0] = static_cast<char>(cp);
        return 1;
    }
    if (cp < 0x800)
    {
        o[0] = static_cast<char>(0xC0 | (cp >> 6));
        o[1] = static_cast<char>(0x80 | (cp & 0x3F));
        return 2;
    }
    if (cp < 0x10000)
    {
        o[0] = static_cast<char>(0xE0 | (cp >> 12));
        o[1] = static_cast<char>(0x80 | ((cp >> 6) & 0x3F));
        o[2] = static_cast<char>(0x80 | (cp & 0x3F));
        return 3;
    }
    o[0] = static_cast<char>(0xF0 | (cp >> 18));
    o[1] = static_cast<char>(0x80 | ((cp >> 12) & 0x3F));
    o[2] = static_cast<char>(0x80 | ((cp >> 6) & 0x3F));
    o[3] = static_cast<char>(0x80 | (cp & 0x3F));
    return 4;
}
} // namespace

bool Doc::parse(const char *text, std::size_t len) noexcept
{
    text_ = text;
    len_ = len;
    Parser p{text, len, 0, tok_, cap_};
    n_ = 0;
    if (!p.value(0))
        return false;
    p.ws();
    if (p.i != len)
        return false;
    n_ = p.n;
    return true;
}

int Doc::get(int obj, const char *key) const noexcept
{
    if (obj < 0 || static_cast<std::uint32_t>(obj) >= n_ || tok_[obj].type != Type::object)
        return -1;
    std::uint32_t at = static_cast<std::uint32_t>(obj) + 1;
    // An object's count is two per member (its key and its value).
    for (std::uint32_t k = 0; k < tok_[obj].count && at < n_; k += 2)
    {
        const std::uint32_t v = at + 1;
        if (v >= n_)
            return -1;
        if (str_is(static_cast<int>(at), key))
            return static_cast<int>(v);
        at = tok_[v].end;
    }
    return -1;
}

int Doc::path(int obj, const char *const *keys, int n) const noexcept
{
    for (int k = 0; k < n && obj >= 0; ++k)
        obj = get(obj, keys[k]);
    return obj;
}

int Doc::item(int arr, std::uint32_t i) const noexcept
{
    if (arr < 0 || tok_[arr].type != Type::array || i >= tok_[arr].count)
        return -1;
    std::uint32_t at = static_cast<std::uint32_t>(arr) + 1;
    for (std::uint32_t k = 0; k < i; ++k)
        at = tok_[at].end;
    return static_cast<int>(at);
}

bool Doc::str(int i, char *out, std::size_t cap) const noexcept
{
    if (!cap)
        return false;
    out[0] = 0;
    if (i < 0 || tok_[i].type != Type::string)
        return false;
    const char *s = text_ + tok_[i].start;
    const std::size_t l = tok_[i].len;
    std::size_t o = 0;
    for (std::size_t k = 0; k < l;)
    {
        char buf[4];
        std::size_t w = 1;
        if (s[k] == '\\' && k + 1 < l)
        {
            const char e = s[k + 1];
            k += 2;
            switch (e)
            {
            case 'b': buf[0] = '\b'; break;
            case 'f': buf[0] = '\f'; break;
            case 'n': buf[0] = '\n'; break;
            case 'r': buf[0] = '\r'; break;
            case 't': buf[0] = '\t'; break;
            case 'u':
            {
                std::uint32_t cp = 0;
                for (int h = 0; h < 4 && k < l; ++h, ++k)
                    cp = cp << 4 | static_cast<std::uint32_t>(hexval(s[k]));
                // a surrogate pair
                if (cp >= 0xD800 && cp < 0xDC00 && k + 6 <= l && s[k] == '\\' && s[k + 1] == 'u')
                {
                    std::uint32_t lo = 0;
                    for (int h = 0; h < 4; ++h)
                        lo = lo << 4 | static_cast<std::uint32_t>(hexval(s[k + 2 + h]));
                    if (lo >= 0xDC00 && lo < 0xE000)
                    {
                        cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                        k += 6;
                    }
                }
                if (cp >= 0xD800 && cp < 0xE000)
                    cp = 0xFFFD;
                w = put_utf8(buf, cp);
                break;
            }
            default: buf[0] = e; break;
            }
        }
        else
            buf[0] = s[k++];
        if (o + w >= cap)
            break;
        std::memcpy(out + o, buf, w);
        o += w;
    }
    out[o] = 0;
    return true;
}

bool Doc::str_is(int i, const char *s) const noexcept
{
    if (i < 0 || tok_[i].type != Type::string)
        return false;
    // Keys and words compared here never carry escapes.
    const std::size_t l = std::strlen(s);
    return tok_[i].len == l && std::memcmp(text_ + tok_[i].start, s, l) == 0;
}

double Doc::num(int i, double fallback) const noexcept
{
    if (i < 0 || tok_[i].type != Type::number)
        return fallback;
    const char *s = text_ + tok_[i].start;
    const char *e = s + tok_[i].len;
    bool neg = false;
    if (s < e && *s == '-')
        neg = true, ++s;
    double v = 0;
    while (s < e && *s >= '0' && *s <= '9')
        v = v * 10 + (*s++ - '0');
    if (s < e && *s == '.')
    {
        ++s;
        double f = 0.1;
        while (s < e && *s >= '0' && *s <= '9')
            v += (*s++ - '0') * f, f *= 0.1;
    }
    if (s < e && (*s == 'e' || *s == 'E'))
    {
        ++s;
        bool eneg = false;
        if (s < e && (*s == '+' || *s == '-'))
            eneg = *s++ == '-';
        int ex = 0;
        while (s < e && *s >= '0' && *s <= '9' && ex < 400)
            ex = ex * 10 + (*s++ - '0');
        while (ex-- > 0)
            v = eneg ? v / 10 : v * 10;
    }
    return neg ? -v : v;
}

bool Doc::boolean(int i, bool fallback) const noexcept
{
    if (i < 0 || tok_[i].type != Type::boolean)
        return fallback;
    return text_[tok_[i].start] == 't';
}

const char *Doc::raw(int i, std::size_t *len) const noexcept
{
    if (i < 0)
    {
        *len = 0;
        return "";
    }
    if (tok_[i].type == Type::string)
    {
        *len = tok_[i].len + 2;
        return text_ + tok_[i].start - 1;
    }
    *len = tok_[i].len;
    return text_ + tok_[i].start;
}

// ---- writer ---------------------------------------------------------------------------------
void Writer::put(char c) noexcept
{
    if (n_ + 1 >= cap_)
    {
        ok_ = false;
        return;
    }
    buf_[n_++] = c;
    buf_[n_] = 0;
}

Writer &Writer::raw(const char *s) noexcept
{
    while (*s)
        put(*s++);
    return *this;
}

Writer &Writer::str(const char *s) noexcept
{
    put('"');
    for (; *s; ++s)
    {
        const unsigned char c = static_cast<unsigned char>(*s);
        if (c == '"' || c == '\\')
        {
            put('\\');
            put(static_cast<char>(c));
        }
        else if (c < 0x20)
        {
            char u[8];
            std::snprintf(u, sizeof u, "\\u%04x", c);
            raw(u);
        }
        else
            put(static_cast<char>(c));
    }
    put('"');
    return *this;
}

Writer &Writer::num(long long v) noexcept
{
    char b[32];
    std::snprintf(b, sizeof b, "%lld", v);
    return raw(b);
}

Writer &Writer::num(double v, int decimals) noexcept
{
    char b[48];
    std::snprintf(b, sizeof b, "%.*f", decimals, v);
    return raw(b);
}

Writer &Writer::key(const char *k) noexcept
{
    if (n_ && buf_[n_ - 1] != '{' && buf_[n_ - 1] != '[')
        put(',');
    str(k);
    put(':');
    return *this;
}

Writer &Writer::open_obj() noexcept
{
    put('{');
    return *this;
}

Writer &Writer::close_obj() noexcept
{
    put('}');
    return *this;
}

std::uint32_t utf8_next(const char *s, std::size_t len, std::size_t *i) noexcept
{
    const auto b = [&](std::size_t k) { return static_cast<unsigned char>(s[k]); };
    const std::size_t at = *i;
    const unsigned c = b(at);
    if (c < 0x80)
    {
        *i = at + 1;
        return c;
    }
    int n = (c & 0xE0) == 0xC0 ? 1 : (c & 0xF0) == 0xE0 ? 2 : (c & 0xF8) == 0xF0 ? 3 : -1;
    if (n < 0)
    {
        *i = at + 1;
        return 0xFFFD;
    }
    std::uint32_t cp = c & (0x3F >> n);
    for (int k = 1; k <= n; ++k)
    {
        if (at + k >= len || (b(at + k) & 0xC0) != 0x80)
        {
            *i = at + 1;
            return 0xFFFD;
        }
        cp = cp << 6 | (b(at + k) & 0x3F);
    }
    *i = at + n + 1;
    return cp;
}
} // namespace play::json
