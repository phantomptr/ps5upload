// PS5Upload Play - the Play Rooms responsibility notice (the design's 7B): accepted once on this
// PS5 (Settings keeps the version), before chat, voice, a public room or anything from the Rooms
// list. A private room with chat and voice off never asks. "Not now" keeps those features off.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once

namespace play::notice
{
// Bump when the text changes in substance: everyone is asked again.
constexpr int kVersion = 1;

enum class Need
{
    chat,         // turning chat on, sending a message
    voice,        // turning the mic on
    public_room,  // making a room Public, or joining from the public list
    rooms_list,   // anything else from Rooms (Details, a friend's room)
    private_play, // a private room, chat and voice off: never asks
};

inline bool accepted(int version_accepted) noexcept
{
    return version_accepted >= kVersion;
}
inline bool needed(Need what, int version_accepted) noexcept
{
    return what != Need::private_play && !accepted(version_accepted);
}
} // namespace play::notice
