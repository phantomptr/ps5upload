// PS5Upload Play - what the app keeps between runs (a small key=value text file outside the app
// folder, so an update in place keeps it).
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play
{
struct Host
{
    char addr[64] = {}; // IPv4 address or name
    std::uint16_t port = 0;
    char name[65] = {};
    char token[80] = {};  // the app channel's pairing token for this computer ("" = not paired)
    std::uint32_t seen = 0; // order: higher = used more recently
};

struct Settings
{
    static constexpr int kHosts = 8;
    bool music = true;
    int music_volume = 25; // percent of full: low by default
    bool sounds = true;
    bool hints = true; // the controls hint while playing
    char language[8] = {}; // "" = the PS5's own; else a code (ui::i18n)
    Host hosts[kHosts];
    std::uint32_t clock = 0; // the next `seen`

    // The remembered computer at addr:port, or null.
    Host *find(const char *addr, std::uint16_t port) noexcept;
    // Remembers addr:port (as the most recent), keeping the oldest out when full.
    Host *remember(const char *addr, std::uint16_t port, const char *name) noexcept;
    void forget(const Host *h) noexcept;
    // The remembered computers, most recent first; returns how many.
    int recent(const Host **out, int cap) const noexcept;

    // Text form. parse() keeps defaults for anything missing or unreadable.
    std::size_t write(char *out, std::size_t cap) const noexcept;
    void parse(const char *text, std::size_t len) noexcept;
};

// "192.168.1.5:9113" or "192.168.1.5" (default port) into addr and port; false when unusable.
bool split_address(const char *in, char *addr, std::size_t cap, std::uint16_t *port,
                   std::uint16_t default_port) noexcept;
} // namespace play
