// PS5Upload Play - the screen: three tiled RGBA framebuffers, the UI copied in, decoded
// pictures converted in (NV12 -> RGBA on worker threads) with the overlay blended on top.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::ps5::display
{
bool open() noexcept;
// Shows a 1920x1080 linear RGBA picture.
void present_ui(const std::uint32_t *ui) noexcept;
// Shows one decoded NV12 picture (any size up to 1920x1088, scaled to the screen).
void present_nv12(const std::uint8_t *y, std::size_t pitch, unsigned w, unsigned h, unsigned luma_rows) noexcept;
// The overlay bands (copied): rows [0, top) and [1080 - bottom, 1080) of `layer`.
void set_overlay(const std::uint32_t *layer, int top, int bottom) noexcept;
// How long ago the last decoded picture went to the screen (µs; huge when never).
std::uint64_t since_video_us() noexcept;
struct Stats
{
    unsigned flips = 0, waits = 0;
    float convert_ms = 0; // average of the last pictures
};
Stats stats() noexcept;
} // namespace play::ps5::display
