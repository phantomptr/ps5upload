// PS5Upload Play - held button chords while playing on a friend's console.
// The PS button stays with this console, so the host's PS button is a held chord:
// Options + touchpad for 1 s. Leaving is L1 + R1 + Options + touchpad for 1 s.
// While Options + touchpad are both down they are kept from the host (the chord's own buttons).
// Room voice (the Play Rooms design's 7A.3): with push-to-talk on Create (set_ptt_on_create),
// Create never reaches the game; holding it is talking (`talk`).
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "proto.hpp"

#include <cstdint>

namespace play
{
class Chords
{
  public:
    static constexpr std::uint64_t kHoldUs = 1000000;
    static constexpr std::uint64_t kPsPressUs = 250000;
    static constexpr std::uint32_t kBase = proto::pad::kOptions | proto::pad::kTouchpad;
    static constexpr std::uint32_t kLeave = kBase | proto::pad::kL1 | proto::pad::kR1;

    struct Out
    {
        std::uint32_t buttons = 0; // what goes to the host
        bool leave = false;        // the leave chord completed (once per hold)
        bool ps_sent = false;      // the PS chord completed (once per hold)
        float ps_progress = 0;     // 0-1 while the PS chord is held (for the on-screen ring)
        float leave_progress = 0;  // 0-1 while the leave chord is held
        bool talk = false;         // Create held while it is push-to-talk
        bool room_menu = false;    // OPTIONS + TOUCHPAD held, then △ pressed: the Room menu (once)
    };

    // Create is push-to-talk (the guest's PS & Create are blocked, or the person is from Rooms).
    void set_ptt_on_create(bool on) noexcept
    {
        ptt_create_ = on;
    }
    bool ptt_on_create() const noexcept
    {
        return ptt_create_;
    }

    Out update(std::uint32_t raw, std::uint64_t now_us) noexcept
    {
        Out o;
        const bool base = (raw & kBase) == kBase;
        const bool all = (raw & kLeave) == kLeave;
        if (base && !base_at_)
            base_at_ = now_us ? now_us : 1;
        if (!base)
        {
            base_at_ = 0;
            ps_done_ = false;
        }
        // △ while OPTIONS + TOUCHPAD are held: the Room menu, never the host's PS button
        const bool tri = (raw & proto::pad::kTriangle) != 0;
        if (base && tri && !tri_was_ && !all)
        {
            o.room_menu = true;
            ps_done_ = true;
        }
        tri_was_ = tri;
        if (all && !leave_at_)
            leave_at_ = now_us ? now_us : 1;
        if (!all)
        {
            leave_at_ = 0;
            leave_done_ = false;
        }
        if (all)
        {
            const std::uint64_t held = now_us - leave_at_;
            o.leave_progress = held >= kHoldUs ? 1.0f : static_cast<float>(held) / kHoldUs;
            if (held >= kHoldUs && !leave_done_)
            {
                leave_done_ = true;
                o.leave = true;
            }
        }
        else if (base && !ps_done_)
        {
            const std::uint64_t held = now_us - base_at_;
            o.ps_progress = held >= kHoldUs ? 1.0f : static_cast<float>(held) / kHoldUs;
            if (held >= kHoldUs && !ps_done_)
            {
                ps_done_ = true;
                o.ps_sent = true;
                ps_until_ = now_us + kPsPressUs;
            }
        }
        // Options + touchpad down together belong to the chord, not to the game.
        o.buttons = base || hold_back_ ? raw & ~(kBase | proto::pad::kTriangle) : raw;
        // Keep them back until both are up again (letting go of one first is not a press).
        hold_back_ = (base || hold_back_) && (raw & kBase) != 0;
        if (ps_until_ && now_us < ps_until_)
            o.buttons |= proto::pad::kPs;
        else
            ps_until_ = 0;
        if (ptt_create_)
        {
            o.talk = (raw & proto::pad::kCreate) != 0;
            o.buttons &= ~proto::pad::kCreate;
        }
        return o;
    }

  private:
    std::uint64_t base_at_ = 0, leave_at_ = 0, ps_until_ = 0;
    bool ps_done_ = false, leave_done_ = false, hold_back_ = false, ptt_create_ = false, tri_was_ = false;
};
} // namespace play
