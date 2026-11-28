// PS5Upload Play - "Play again": the hosts this PS5 played with, each with the friend pass their
// engine gave (docs/EASY_JOIN.md). Kept in friends.txt beside settings.txt, one line each.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play
{
struct Friend
{
    char addr[64] = {};   // the engine's address (a Tailscale one is reached through the helper)
    std::uint16_t port = 0;
    bool tailnet = false;
    char path[80] = {};   // the guest socket's path ("" = /api/join/ws)
    char name[48] = {};   // the host's computer, as shown
    char pass[48] = {};   // the friend pass (asks again; the host still decides)
    std::uint32_t seen = 0;
};

struct Friends
{
    static constexpr int kMax = 8;
    Friend list[kMax];
    std::uint32_t clock = 0;

    // Keeps (or refreshes) the pass for addr:port as the most recent; the oldest makes room.
    Friend *remember(const char *addr, std::uint16_t port, bool tailnet, const char *path, const char *name,
                     const char *pass) noexcept;
    Friend *find(const char *addr, std::uint16_t port) noexcept;
    void forget(const Friend *f) noexcept;
    void forget_all() noexcept;
    // Most recent first; returns how many.
    int recent(const Friend **out, int cap) const noexcept;

    std::size_t write(char *out, std::size_t cap) const noexcept;
    void parse(const char *text, std::size_t len) noexcept;
};
} // namespace play
