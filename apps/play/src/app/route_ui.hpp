// PS5Upload Play - the way to the host and its tier, in words, while playing and on the host's
// guest list (docs/ROUTES.md).
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "../ui/canvas.hpp"
#include "guest.hpp"

#include <cstddef>
#include <cstdint>

namespace play::route_ui
{
// "Home network", "Tailscale", "Internet", "Internet, through your PS5Upload" (route::Kind).
const char *way_text(int kind, bool bridged) noexcept;
// "Excellent" … "Poor", "No answer" (route::Tier).
const char *tier_text(int tier) noexcept;
// The same from the engine's words ("lan", "good"…); "" for none or a word it does not know.
const char *way_word_text(const char *word) noexcept;
const char *tier_word_text(const char *word) noexcept;
// The host's guest list: "Home network · Good" (or just one of them), into out.
void guest_line(const char *way, const char *tier, char *out, std::size_t cap) noexcept;

// While playing, top right over the picture: the way, its tier and round trip for a few seconds
// after joining and after each change of way, and all the time while it is Poor or worse. Draws
// into the overlay layer's top band (straight alpha); true when it drew.
bool draw_chip(ui::Canvas &layer, const GuestView &v, std::uint64_t now, std::uint64_t joined_at) noexcept;
} // namespace play::route_ui
