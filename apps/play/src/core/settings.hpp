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
    // The engine's id (its discovery answer's `id`, the helper mesh's `id`): the pairing belongs
    // to the computer, never to an address (integration design P0-1). "" = not known yet.
    char id[72] = {};
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
    // Play Rooms (docs/ROOMS.md)
    int notice = 0;          // the responsibility notice's version accepted (0 = never)
    bool welcome_done = false; // the first-run guide was seen
    bool rooms_intro = false;  // "Rooms are open games you can ask to join..." was seen
    bool public_ok = false;    // the first-time "Make public" confirm was said yes to
    bool talk_open = false;  // open mic instead of push-to-talk (only where the room allows it)
    int duck = 1;            // the game's sound while others talk: 0 off, 1 a little, 2 more
    bool show_chat = true;   // chat lines over the game
    int hide_rude = -1;      // -1: on in Public rooms only, 0 off, 1 on
    char sort[12] = {};      // the Join list's sort ("" = best)
    bool f_space = false, f_nopw = false, f_myver = true, f_near = false;
    char f_lang[8] = {};

    // The remembered computer at addr:port, or null.
    Host *find(const char *addr, std::uint16_t port) noexcept;
    // The remembered computer with this engine id, or null.
    Host *find_id(const char *id) noexcept;
    // Remembers addr:port (as the most recent), keeping the oldest out when full.
    Host *remember(const char *addr, std::uint16_t port, const char *name) noexcept;
    // Remembers an engine by its id: the row with that id follows it to a new address (no
    // surprise re-pair); a row at the same address with another id stays that other computer's.
    // Without an id: as remember().
    Host *remember_id(const char *id, const char *addr, std::uint16_t port, const char *name) noexcept;
    void forget(const Host *h) noexcept;
    // A row for a new computer (the oldest makes room), cleared.
    Host *fresh() noexcept;
    // The remembered computers, most recent first; returns how many.
    int recent(const Host **out, int cap) const noexcept;

    // Text form. parse() keeps defaults for anything missing or unreadable.
    std::size_t write(char *out, std::size_t cap) const noexcept;
    void parse(const char *text, std::size_t len) noexcept;
};

// "192.168.1.5:19200" or "192.168.1.5" (default port) into addr and port; false when unusable.
bool split_address(const char *in, char *addr, std::size_t cap, std::uint16_t *port,
                   std::uint16_t default_port) noexcept;
} // namespace play
