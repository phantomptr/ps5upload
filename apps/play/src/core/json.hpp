// PS5Upload Play - a small JSON reader and writer (no heap: tokens live in a caller's array).
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::json
{
enum class Type : std::uint8_t
{
    none,
    object,
    array,
    string,
    number,
    boolean,
    null,
};

// One value. Objects and arrays list their children as the tokens after them: `count` is the
// number of direct children (an object's are key, value, key, value...), `end` the index of the
// first token after the whole value.
struct Token
{
    Type type = Type::none;
    std::uint32_t start = 0, len = 0; // the value's text (a string without its quotes)
    std::uint32_t count = 0;
    std::uint32_t end = 0;
};

// A parsed document over `text` (not copied: it must outlive the Doc).
class Doc
{
  public:
    Doc(Token *tokens, std::uint32_t capacity) noexcept : tok_{tokens}, cap_{capacity}
    {
    }
    // False on bad JSON, too deep, or more values than tokens.
    bool parse(const char *text, std::size_t len) noexcept;
    std::uint32_t size() const noexcept
    {
        return n_;
    }
    const Token &at(std::uint32_t i) const noexcept
    {
        return tok_[i];
    }

    // The member `key` of the object at `obj` (index of its value), or -1.
    int get(int obj, const char *key) const noexcept;
    // A path of keys from the root: get_path({"together", "invite", "code"}).
    int path(int obj, const char *const *keys, int n) const noexcept;
    // The i-th element of the array at `arr`, or -1.
    int item(int arr, std::uint32_t i) const noexcept;
    std::uint32_t count(int i) const noexcept
    {
        return i >= 0 ? tok_[i].count : 0;
    }

    Type type(int i) const noexcept
    {
        return i >= 0 ? tok_[i].type : Type::none;
    }
    bool is_null(int i) const noexcept
    {
        return i < 0 || tok_[i].type == Type::null;
    }
    // The string's text unescaped into out (always terminated); false when not a string.
    bool str(int i, char *out, std::size_t cap) const noexcept;
    // True when the token is a string equal to s.
    bool str_is(int i, const char *s) const noexcept;
    double num(int i, double fallback = 0) const noexcept;
    long long integer(int i, long long fallback = 0) const noexcept
    {
        return static_cast<long long>(num(i, static_cast<double>(fallback)));
    }
    bool boolean(int i, bool fallback = false) const noexcept;
    // Raw text of any value (as it was in the document).
    const char *raw(int i, std::size_t *len) const noexcept;

  private:
    Token *tok_;
    std::uint32_t cap_;
    std::uint32_t n_ = 0;
    const char *text_ = nullptr;
    std::size_t len_ = 0;
};

// Writing: appends into a fixed buffer; `ok()` turns false if anything did not fit.
class Writer
{
  public:
    Writer(char *buf, std::size_t cap) noexcept : buf_{buf}, cap_{cap}
    {
        if (cap_)
            buf_[0] = 0;
    }
    Writer &raw(const char *s) noexcept;
    Writer &str(const char *s) noexcept; // a quoted, escaped string
    Writer &num(long long v) noexcept;
    Writer &num(double v, int decimals) noexcept;
    Writer &boolean(bool v) noexcept
    {
        return raw(v ? "true" : "false");
    }
    // `"key":` with a comma first when needed (the writer tracks the last char).
    Writer &key(const char *k) noexcept;
    Writer &open_obj() noexcept;
    Writer &close_obj() noexcept;
    bool ok() const noexcept
    {
        return ok_;
    }
    const char *c_str() const noexcept
    {
        return buf_;
    }
    std::size_t len() const noexcept
    {
        return n_;
    }

  private:
    void put(char c) noexcept;
    char *buf_;
    std::size_t cap_, n_ = 0;
    bool ok_ = true;
};

// Decodes one UTF-8 code point at s (advances *i); returns 0xFFFD for broken bytes.
std::uint32_t utf8_next(const char *s, std::size_t len, std::size_t *i) noexcept;
} // namespace play::json
