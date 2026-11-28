// PS5Upload Play - the sound's jitter buffer and PCM ring.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "jitter.hpp"

#include <cstring>

namespace play::audio
{
void Jitter::reset() noexcept
{
    for (auto &s : slots_)
        s.used = false;
    have_next_ = primed_ = have_newest_ = false;
}

int Jitter::buffered() const noexcept
{
    if (!have_next_ || !have_newest_ || seq_after(next_, newest_))
        return 0;
    return static_cast<std::uint16_t>(newest_ - next_) + 1;
}

void Jitter::insert(std::uint16_t index, const std::uint8_t *data, std::size_t len) noexcept
{
    if (len == 0 || len > kMaxPacket)
        return;
    if (!have_next_)
    {
        next_ = index;
        have_next_ = true;
    }
    if (seq_after(next_, index))
    {
        ++stats_.late; // its time has passed
        return;
    }
    if (static_cast<std::uint16_t>(index - next_) >= kSlots)
    {
        // Far ahead (a long stall, or the stream restarted): start over from here.
        reset();
        next_ = index;
        have_next_ = true;
    }
    Slot &s = slots_[index % kSlots];
    if (s.used && s.index == index)
        return; // a copy
    s.used = true;
    s.index = index;
    s.len = static_cast<std::uint16_t>(len);
    std::memcpy(s.data, data, len);
    if (!have_newest_ || seq_after(index, newest_))
    {
        newest_ = index;
        have_newest_ = true;
    }
}

void Jitter::drop_head() noexcept
{
    Slot &s = slots_[next_ % kSlots];
    if (s.used && s.index == next_)
        s.used = false;
    ++next_;
}

Jitter::Act Jitter::next(const std::uint8_t **data, std::size_t *len) noexcept
{
    *data = nullptr;
    *len = 0;
    const int depth = buffered();
    if (!primed_)
    {
        if (depth < target_)
            return Act::silence;
        primed_ = true;
    }
    if (depth <= 0)
    {
        // ran dry: wait for the buffer to fill again
        primed_ = false;
        ++stats_.underruns;
        return Act::silence;
    }
    // Too much held (the sender ran ahead, or a burst after a stall): catch up.
    while (buffered() > most_)
    {
        drop_head();
        ++stats_.dropped;
    }
    Slot &s = slots_[next_ % kSlots];
    if (s.used && s.index == next_)
    {
        *data = s.data;
        *len = s.len;
        s.used = false;
        ++next_;
        ++stats_.played;
        return Act::play;
    }
    // Missing, with newer frames held: use the next frame's redundancy if it is here.
    const std::uint16_t after = static_cast<std::uint16_t>(next_ + 1);
    Slot &n = slots_[after % kSlots];
    ++next_;
    if (n.used && n.index == after)
    {
        *data = n.data;
        *len = n.len;
        ++stats_.recovered;
        return Act::fec;
    }
    ++stats_.concealed;
    return Act::conceal;
}

void PcmRing::push(const std::int16_t *stereo, std::size_t frames) noexcept
{
    if (frames > space())
    {
        // keep the newest: drop the oldest
        const std::size_t drop = frames - space();
        head_ = (head_ + drop) % kFrames;
        count_ -= drop;
    }
    std::size_t tail = (head_ + count_) % kFrames;
    for (std::size_t i = 0; i < frames; ++i)
    {
        buf_[tail * 2] = stereo[i * 2];
        buf_[tail * 2 + 1] = stereo[i * 2 + 1];
        tail = (tail + 1) % kFrames;
    }
    count_ += frames;
}

bool PcmRing::pop(std::int16_t *out, std::size_t frames) noexcept
{
    if (frames > count_)
        return false;
    for (std::size_t i = 0; i < frames; ++i)
    {
        out[i * 2] = buf_[head_ * 2];
        out[i * 2 + 1] = buf_[head_ * 2 + 1];
        head_ = (head_ + 1) % kFrames;
    }
    count_ -= frames;
    return true;
}
} // namespace play::audio
