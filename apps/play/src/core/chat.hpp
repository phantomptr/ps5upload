// PS5Upload Play - room chat (the Play Rooms design's 7A.4): quick messages sent as ids and shown
// in each person's own language, short typed text, the lines kept in memory only (nothing is
// stored), the fading overlay's recent lines and the unread count. Pure logic.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::chat
{
constexpr int kQuick = 16;       // ids 1-16 (ui/strings.hpp quick_1 .. quick_16)
constexpr int kTextMax = 140;    // characters
constexpr std::uint64_t kFadeUs = 6000000; // the overlay's lines fade after 6 s

// Typed text made safe to send and to show: control and format characters out, at most
// kTextMax characters, spaces collapsed; in a Public room anything that looks like an invite
// code is taken out. Returns its length (0: nothing to send).
std::size_t clean_text(const char *in, char *out, std::size_t cap, bool strip_codes) noexcept;
// For showing: a link is never a link here, and is spelled with "·" for its dots so it reads as
// plain text (www·example·com). `hide_rude` masks the local list of rude words.
std::size_t for_display(const char *in, char *out, std::size_t cap, bool hide_rude) noexcept;
bool looks_like_link(const char *word, std::size_t len) noexcept;

// {"chat":{"quick":n}} / {"chat":{"text":"..."}}: what this app sends on the guest socket.
std::size_t quick_message(char *out, std::size_t cap, int id) noexcept;
std::size_t text_message(char *out, std::size_t cap, const char *text) noexcept;

// A chat line from the engine: {"chat": {"from", "quick"} | {"from", "text"}} and its notes
// ({"chat": {"error": "slow"|"off"|"muted"}}).
struct Incoming
{
    char from[40] = {};
    int quick = 0;      // 1-16, 0 for text
    char text[4 * kTextMax + 1] = {};
    char error[12] = {}; // "slow", "off", "muted"
    bool system = false; // the host engine's own line ("Mia flagged Lee" goes to the host only)
};
bool parse(const char *json_text, std::size_t len, Incoming &in) noexcept;

// What this person may send: 1 message every 2 s and 10 a minute (the engine drops the rest
// with "Slow down"; the app says it before sending).
class Rate
{
  public:
    bool take(std::uint64_t now_us) noexcept;

  private:
    std::uint64_t times_[10] = {};
    int at_ = 0;
    std::uint64_t last_ = 0;
};

// The chat as this app holds it: the last 50 lines, in memory only.
struct Line
{
    char from[40] = {};
    int quick = 0;
    char text[4 * kTextMax + 1] = {};
    int times = 1; // the same line again, folded ("×3")
    std::uint64_t at_us = 0;
    bool mine = false;
};

class Log
{
  public:
    static constexpr int kLines = 50;
    void clear() noexcept;
    // A line in (the same as the last one from the same person within 30 s is folded into it).
    void add(const char *from, int quick, const char *text, std::uint64_t now_us, bool mine) noexcept;
    int size() const noexcept
    {
        return n_;
    }
    // i = 0 is the oldest kept.
    const Line &at(int i) const noexcept;
    int unread() const noexcept
    {
        return unread_;
    }
    void read_all() noexcept
    {
        unread_ = 0;
    }
    // The lines for the overlay: at most `cap` of the newest, each younger than kFadeUs; newest last.
    int recent(const Line **out, int cap, std::uint64_t now_us) const noexcept;

  private:
    Line lines_[kLines];
    int head_ = 0, n_ = 0, unread_ = 0;
};
} // namespace play::chat
