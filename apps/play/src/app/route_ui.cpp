// PS5Upload Play - the way to the host and its tier, in words (route_ui.hpp).
// SPDX-License-Identifier: GPL-3.0-or-later
#include "route_ui.hpp"

#include "../net/route.hpp"
#include "../ui/strings.hpp"

#include <cstdio>
#include <cstring>

namespace play::route_ui
{
namespace
{
using ui::S;
using ui::tr;
namespace col = ui::color;
constexpr std::uint64_t kShowUs = 6000000;
} // namespace

const char *way_text(int kind, bool bridged) noexcept
{
    switch (static_cast<route::Kind>(kind))
    {
    case route::Kind::lan:
        return tr(S::way_lan);
    case route::Kind::tailscale:
        return tr(S::way_tailscale);
    case route::Kind::internet:
        return tr(bridged ? S::way_bridge : S::way_internet);
    }
    return "";
}

const char *tier_text(int tier) noexcept
{
    switch (static_cast<route::Tier>(tier))
    {
    case route::Tier::excellent:
        return tr(S::tier_excellent);
    case route::Tier::good:
        return tr(S::tier_good);
    case route::Tier::playable:
        return tr(S::tier_playable);
    case route::Tier::poor:
        return tr(S::tier_poor);
    case route::Tier::down:
        return tr(S::tier_down);
    }
    return "";
}

const char *way_word_text(const char *w) noexcept
{
    if (!w || !w[0])
        return "";
    for (int k = 0; k <= static_cast<int>(route::Kind::internet); ++k)
        if (!std::strcmp(w, route::kind_word(static_cast<route::Kind>(k))))
            return way_text(k, false);
    return "";
}

const char *tier_word_text(const char *w) noexcept
{
    if (!w || !w[0])
        return "";
    for (int t = 0; t <= static_cast<int>(route::Tier::excellent); ++t)
        if (!std::strcmp(w, route::tier_word(static_cast<route::Tier>(t))))
            return tier_text(t);
    return "";
}

void guest_line(const char *way, const char *tier, char *out, std::size_t cap) noexcept
{
    const char *a = way_word_text(way), *b = tier_word_text(tier);
    if (a[0] && b[0])
        std::snprintf(out, cap, "%s · %s", a, b);
    else
        std::snprintf(out, cap, "%s", a[0] ? a : b);
}

bool draw_chip(ui::Canvas &l, const GuestView &v, std::uint64_t now, std::uint64_t joined_at) noexcept
{
    if (v.route < 0 || v.phase != GuestView::joined)
        return false;
    const auto tier = static_cast<route::Tier>(v.tier);
    const bool bad = tier <= route::Tier::poor;
    const bool fresh = (joined_at && now - joined_at < kShowUs) || (v.route_changed_us && now - v.route_changed_us < kShowUs);
    if (!bad && !fresh)
        return false;
    char text[160];
    const char *way = way_text(v.route, v.bridged);
    if (v.route_changed_us && now - v.route_changed_us < kShowUs)
        std::snprintf(text, sizeof text, tr(S::way_changed), way);
    else if (bad && !fresh)
        std::snprintf(text, sizeof text, "%s", tr(S::way_poor));
    else if (v.route_rtt_ms >= 0)
        std::snprintf(text, sizeof text, tr(S::way_chip), way, tier_text(v.tier), v.route_rtt_ms);
    else
        std::snprintf(text, sizeof text, "%s · %s", way, tier_text(v.tier));
    const int h = 52;
    const int w = ui::Canvas::text_width(ui::Font::small, text) + 72;
    const int x = ui::kW - w - 40, y = 34;
    const std::uint32_t dot = tier >= route::Tier::good ? col::good : tier == route::Tier::playable ? col::warn : col::bad;
    for (int yy = y; yy < y + h; ++yy)
        for (int xx = x < 0 ? 0 : x; xx < x + w && xx < ui::kW; ++xx)
            l.pixels()[yy * ui::kW + xx] = (col::panel & 0xffffffu) | (215u << 24);
    // the tier's colour as a dot before the words
    for (int yy = y + h / 2 - 8; yy < y + h / 2 + 8; ++yy)
        for (int xx = x + 22; xx < x + 38; ++xx)
        {
            const int dx = xx - (x + 30), dy = yy - (y + h / 2);
            if (dx * dx + dy * dy <= 64)
                l.pixels()[yy * ui::kW + xx] = (dot & 0xffffffu) | (255u << 24);
        }
    l.text(ui::Font::small, x + 50, y + (h - ui::font_height(ui::Font::small)) / 2, text, col::text);
    return true;
}
} // namespace play::route_ui
