// PS5Upload Play - room chat.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "chat.hpp"

#include "json.hpp"
#include "proto.hpp"

#include <cstdio>
#include <cstring>
#include <pthread.h>

namespace play::chat
{
namespace
{
bool format_char(std::uint32_t c) noexcept
{
    return c < 0x20 || c == 0x7f || (c >= 0x80 && c < 0xa0) || (c >= 0x200b && c <= 0x200f) ||
           (c >= 0x202a && c <= 0x202e) || (c >= 0x2060 && c <= 0x206f) || c == 0xfeff || c == 0xfffd;
}

bool code_char(char c) noexcept
{
    for (const char *a = proto::kCodeAlphabet; *a; ++a)
        if (*a == c)
            return true;
    return false;
}

// "ABC-DEF", "ABCDEF" or "abc def" as a whole word: an invite code (taken out in Public).
bool is_code(const char *w, std::size_t n) noexcept
{
    char c[8];
    std::size_t k = 0;
    for (std::size_t i = 0; i < n; ++i)
    {
        char ch = w[i];
        if (ch == '-')
            continue;
        if (ch >= 'a' && ch <= 'z')
            ch = static_cast<char>(ch - 'a' + 'A');
        if (k >= 6 || !code_char(ch))
            return false;
        c[k++] = ch;
    }
    if (k != 6)
        return false;
    // a plain word in capitals ("HELLOO") is not a code: a code has a digit or a dash
    bool digit = false;
    for (std::size_t i = 0; i < 6; ++i)
        digit = digit || (c[i] >= '2' && c[i] <= '9');
    return digit || std::memchr(w, '-', n) != nullptr;
}

char lower(char c) noexcept
{
    return c >= 'A' && c <= 'Z' ? static_cast<char>(c - 'A' + 'a') : c;
}

// A small local list (the rooms' word filter is the engine's and the hub's; this only masks on
// this screen, on by default in Public).
const char *const kRude[] = {"fuck", "shit", "bitch", "cunt", "asshole", "dick", "bastard", "slut", "whore",
                             "fag", "nigger", "retard", "merde", "puta", "scheisse", "blyat", "cazzo"};

bool rude(const char *w, std::size_t n) noexcept
{
    for (const char *r : kRude)
    {
        const std::size_t rl = std::strlen(r);
        for (std::size_t i = 0; i + rl <= n; ++i)
        {
            std::size_t k = 0;
            while (k < rl && lower(w[i + k]) == r[k])
                ++k;
            if (k == rl)
                return true;
        }
    }
    return false;
}

bool word_char(char c) noexcept
{
    return c != ' ';
}
} // namespace

bool looks_like_link(const char *w, std::size_t n) noexcept
{
    // "://", "www.", or letters.letters with a 2-6 letter end ("example.com", "bit.ly/x")
    for (std::size_t i = 0; i + 3 <= n; ++i)
        if (!std::memcmp(w + i, "://", 3))
            return true;
    if (n >= 4 && lower(w[0]) == 'w' && lower(w[1]) == 'w' && lower(w[2]) == 'w' && w[3] == '.')
        return true;
    for (std::size_t i = 1; i + 2 < n; ++i)
    {
        if (w[i] != '.')
            continue;
        const auto alpha = [](char c) { return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z'); };
        if (!alpha(w[i - 1]) && !(w[i - 1] >= '0' && w[i - 1] <= '9'))
            continue;
        std::size_t k = i + 1;
        while (k < n && alpha(w[k]))
            ++k;
        const std::size_t tld = k - (i + 1);
        if (tld >= 2 && tld <= 6 && (k == n || w[k] == '/' || w[k] == ':' || w[k] == '.'))
            return true;
    }
    return false;
}

std::size_t clean_text(const char *in, char *out, std::size_t cap, bool strip_codes) noexcept
{
    if (!cap)
        return 0;
    out[0] = 0;
    if (!in)
        return 0;
    const std::size_t len = std::strlen(in);
    // First: the characters, spaces collapsed, at most kTextMax.
    char tmp[4 * kTextMax + 8];
    std::size_t t = 0;
    int chars = 0;
    bool space = false;
    std::size_t i = 0;
    while (i < len && chars < kTextMax)
    {
        const std::size_t at = i;
        const std::uint32_t c = json::utf8_next(in, len, &i);
        if (c == ' ' || c == '\t' || c == '\n' || c == '\r')
        {
            space = t > 0;
            continue;
        }
        if (format_char(c))
            continue;
        const std::size_t n = i - at;
        if (t + n + 2 > sizeof tmp)
            break;
        if (space)
        {
            tmp[t++] = ' ';
            ++chars;
            space = false;
            if (chars >= kTextMax)
                break;
        }
        std::memcpy(tmp + t, in + at, n);
        t += n;
        ++chars;
    }
    tmp[t] = 0;
    // Then: whole words that are invite codes go (Public).
    std::size_t o = 0;
    for (std::size_t w = 0; w < t;)
    {
        std::size_t e = w;
        while (e < t && word_char(tmp[e]))
            ++e;
        const bool drop = strip_codes && is_code(tmp + w, e - w);
        if (!drop && e > w)
        {
            if (o && o + 1 < cap)
                out[o++] = ' ';
            const std::size_t n = e - w < cap - 1 - o ? e - w : cap - 1 - o;
            std::memcpy(out + o, tmp + w, n);
            o += n;
        }
        w = e + 1;
    }
    out[o] = 0;
    return o;
}

std::size_t for_display(const char *in, char *out, std::size_t cap, bool hide_rude) noexcept
{
    if (!cap)
        return 0;
    std::size_t o = 0;
    const std::size_t len = in ? std::strlen(in) : 0;
    for (std::size_t w = 0; w < len && o + 1 < cap;)
    {
        std::size_t e = w;
        while (e < len && in[e] != ' ')
            ++e;
        const bool link = looks_like_link(in + w, e - w);
        const bool mask = hide_rude && rude(in + w, e - w);
        for (std::size_t k = w; k < e && o + 3 < cap; ++k)
        {
            if (mask)
                out[o++] = '*';
            else if (link && in[k] == '.')
            {
                out[o++] = static_cast<char>(0xc2); // U+00B7 middle dot
                out[o++] = static_cast<char>(0xb7);
            }
            else
                out[o++] = in[k];
            if (mask)
            {
                // one star per character, not per byte
                while (k + 1 < e && (static_cast<unsigned char>(in[k + 1]) & 0xc0) == 0x80)
                    ++k;
            }
        }
        if (e < len && o + 1 < cap)
            out[o++] = ' ';
        w = e + 1;
    }
    out[o] = 0;
    return o;
}

std::size_t quick_message(char *out, std::size_t cap, int id) noexcept
{
    if (id < 1 || id > kQuick)
        return 0;
    const int n = std::snprintf(out, cap, "{\"chat\":{\"quick\":%d}}", id);
    return n > 0 && static_cast<std::size_t>(n) < cap ? static_cast<std::size_t>(n) : 0;
}

std::size_t text_message(char *out, std::size_t cap, const char *text) noexcept
{
    if (!text || !*text)
        return 0;
    json::Writer w(out, cap);
    w.raw("{\"chat\":{\"text\":").str(text).raw("}}");
    return w.ok() ? w.len() : 0;
}

bool parse(const char *t, std::size_t len, Incoming &in) noexcept
{
    in = Incoming{};
    json::Token toks[24];
    json::Doc d(toks, 24);
    if (!d.parse(t, len) || d.type(0) != json::Type::object)
        return false;
    const int c = d.get(0, "chat");
    if (d.type(c) != json::Type::object)
        return false;
    if (!d.str(d.get(c, "error"), in.error, sizeof in.error))
        in.error[0] = 0;
    if (!d.str(d.get(c, "from"), in.from, sizeof in.from))
        in.from[0] = 0;
    in.system = d.boolean(d.get(c, "system"));
    if (d.type(d.get(c, "id")) == json::Type::number)
        in.id = d.integer(d.get(c, "id"));
    if (d.type(d.get(c, "sid")) == json::Type::number)
        in.sid = static_cast<int>(d.integer(d.get(c, "sid")));
    if (d.type(d.get(c, "x")) == json::Type::number)
        in.x = static_cast<int>(d.integer(d.get(c, "x")));
    const int q = d.get(c, "quick");
    if (d.type(q) == json::Type::number)
    {
        const long long v = d.integer(q);
        in.quick = v >= 1 && v <= kQuick ? static_cast<int>(v) : 0;
        if (!in.quick)
            return false;
    }
    else
    {
        char raw[4 * kTextMax + 64];
        if (d.str(d.get(c, "text"), raw, sizeof raw))
            clean_text(raw, in.text, sizeof in.text, false);
    }
    return in.error[0] || in.quick || in.text[0];
}

bool Rate::take(std::uint64_t now) noexcept
{
    if (last_ && now - last_ < 2000000)
        return false;
    // the oldest of the last ten within a minute: too many
    const std::uint64_t oldest = times_[at_];
    if (oldest && now - oldest < 60000000)
        return false;
    times_[at_] = now ? now : 1;
    at_ = (at_ + 1) % 10;
    last_ = now ? now : 1;
    return true;
}

void Log::add_line(const Incoming &in, std::uint64_t now, bool mine) noexcept
{
    if (in.id >= 0)
        for (int i = 0; i < n_; ++i)
        {
            Line &l = lines_[(head_ + i) % kLines];
            if (l.id == in.id)
            {
                l.times = in.x > 1 ? in.x : l.times;
                l.at_us = now;
                if (!mine)
                    ++unread_;
                return;
            }
        }
    Line *l;
    if (n_ < kLines)
        l = &lines_[(head_ + n_++) % kLines];
    else
    {
        l = &lines_[head_];
        head_ = (head_ + 1) % kLines;
    }
    *l = Line{};
    std::snprintf(l->from, sizeof l->from, "%s", in.from);
    l->quick = in.quick;
    std::snprintf(l->text, sizeof l->text, "%s", in.text);
    l->at_us = now;
    l->mine = mine;
    l->id = in.id;
    l->times = in.x > 1 ? in.x : 1;
    if (!mine)
        ++unread_;
}

int parse_history(const char *t, std::size_t len, Log &log, std::uint64_t now) noexcept
{
    // One reader at a time: every guest socket (one per player on this PS5) has its own thread.
    static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
    struct Hold
    {
        pthread_mutex_t &m;
        explicit Hold(pthread_mutex_t &x) noexcept : m(x)
        {
            pthread_mutex_lock(&m);
        }
        ~Hold()
        {
            pthread_mutex_unlock(&m);
        }
    } hold(mu);
    static json::Token toks[600];
    json::Doc d(toks, 600);
    if (!d.parse(t, len) || d.type(0) != json::Type::object)
        return 0;
    const int h = d.get(0, "chat_history");
    int n = 0;
    for (std::uint32_t i = 0; i < d.count(h); ++i)
    {
        const int c = d.item(h, i);
        Incoming in;
        if (!d.str(d.get(c, "from"), in.from, sizeof in.from))
            in.from[0] = 0;
        if (d.type(d.get(c, "id")) == json::Type::number)
            in.id = d.integer(d.get(c, "id"));
        const int q = d.get(c, "quick");
        if (d.type(q) == json::Type::number)
            in.quick = d.integer(q) >= 1 && d.integer(q) <= kQuick ? static_cast<int>(d.integer(q)) : 0;
        else
        {
            char raw[4 * kTextMax + 64];
            if (d.str(d.get(c, "text"), raw, sizeof raw))
                clean_text(raw, in.text, sizeof in.text, false);
        }
        if (!in.quick && !in.text[0])
            continue;
        // old lines: no fading toasts for them
        log.add_line(in, now > kFadeUs ? now - kFadeUs : 0, false);
        ++n;
    }
    log.read_all();
    return n;
}

void Log::clear() noexcept
{
    head_ = n_ = unread_ = 0;
}

const Line &Log::at(int i) const noexcept
{
    return lines_[(head_ + i) % kLines];
}

Line &Log::at(int i) noexcept
{
    return lines_[(head_ + i) % kLines];
}

void Log::add(const char *from, int quick, const char *text, std::uint64_t now, bool mine) noexcept
{
    if (n_)
    {
        Line &last = lines_[(head_ + n_ - 1) % kLines];
        if (!std::strcmp(last.from, from ? from : "") && last.quick == quick &&
            !std::strcmp(last.text, text ? text : "") && now - last.at_us < 30000000)
        {
            ++last.times;
            last.at_us = now;
            if (!mine)
                ++unread_;
            return;
        }
    }
    Line *l;
    if (n_ < kLines)
        l = &lines_[(head_ + n_++) % kLines];
    else
    {
        l = &lines_[head_];
        head_ = (head_ + 1) % kLines;
    }
    *l = Line{};
    std::snprintf(l->from, sizeof l->from, "%s", from ? from : "");
    l->quick = quick;
    std::snprintf(l->text, sizeof l->text, "%s", text ? text : "");
    l->at_us = now;
    l->mine = mine;
    if (!mine)
        ++unread_;
}

int Log::recent(const Line **out, int cap, std::uint64_t now) const noexcept
{
    int k = 0;
    for (int i = n_ - 1; i >= 0 && k < cap; --i)
    {
        const Line &l = at(i);
        if (now - l.at_us >= kFadeUs)
            break;
        out[k++] = &l;
    }
    // newest last
    for (int a = 0, b = k - 1; a < b; ++a, --b)
    {
        const Line *x = out[a];
        out[a] = out[b];
        out[b] = x;
    }
    return k;
}
} // namespace play::chat
