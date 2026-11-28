// PS5Upload Play - "Back to game": the game to go back to after the host restarts or this app was
// closed (resume.txt, kept 5 minutes), with the way that game was actually played over, so going
// back dials the same engine the same way (13.0.0 audit: a game played through this home's
// PS5Upload, the bridge, went back to the wrong engine).
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "../net/route.hpp"

#include <cstddef>
#include <cstdint>

namespace play::resume
{
struct Record
{
    char addr[64] = {}; // the join's start address (start())
    std::uint16_t port = 0;
    char token[80] = {};
    char path[80] = {};
    char name[65] = {};
    std::uint64_t at = 0; // unix seconds when written
    // The way in use when it was written (has_way false: an older file, or no ways were set).
    bool has_way = false;
    route::Candidate way;
};

// One line: addr|port|token|path|name|at, then the way: |kind|way|addr|port|path|room|fp|lobby.
// 0 when it does not fit or a field holds a '|' or a line break.
std::size_t write(const Record &r, char *out, std::size_t cap) noexcept;
// A line write() made (an older file without the way too). False when it is not one.
bool parse(const char *text, Record &out) noexcept;
} // namespace play::resume
