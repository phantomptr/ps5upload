// PS5Upload Play - the sound's jitter buffer and the PCM ring that re-chunks it.
// The engine sends each 10 ms Opus frame once with its 16-bit index; a frame recovered from a
// later packet's redundancy can arrive after newer ones (rp_stream/audio.rs). The buffer puts
// them back in order, conceals a frame that never came, and keeps a few frames of headroom.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::audio
{
inline bool seq_after(std::uint16_t a, std::uint16_t b) noexcept
{
    return a != b && static_cast<std::int16_t>(static_cast<std::uint16_t>(a - b)) > 0;
}

class Jitter
{
  public:
    static constexpr int kSlots = 64;
    static constexpr std::size_t kMaxPacket = 1276;

    struct Stats
    {
        std::uint64_t played = 0, concealed = 0, recovered = 0, late = 0, dropped = 0, underruns = 0;
    };

    // How many frames to hold before playing (and after running dry), and the most it keeps.
    void set_depth(int target, int most) noexcept
    {
        target_ = target;
        most_ = most;
    }
    void reset() noexcept;
    void insert(std::uint16_t index, const std::uint8_t *data, std::size_t len) noexcept;

    enum class Act
    {
        silence, // nothing to play yet (or ran dry): 10 ms of silence
        play,    // decode `data`
        fec,     // decode the next frame's redundancy (`data`, decode_fec=1) for the missing one
        conceal, // the frame is missing: packet loss concealment
    };
    // The next 10 ms. `data`/`len` point into the buffer until the next call.
    Act next(const std::uint8_t **data, std::size_t *len) noexcept;

    int buffered() const noexcept; // frames from the next one to the newest held
    const Stats &stats() const noexcept
    {
        return stats_;
    }

  private:
    struct Slot
    {
        bool used = false;
        std::uint16_t index = 0;
        std::uint16_t len = 0;
        std::uint8_t data[kMaxPacket];
    };
    Slot slots_[kSlots];
    bool have_next_ = false, primed_ = false, have_newest_ = false;
    std::uint16_t next_ = 0, newest_ = 0;
    int target_ = 4, most_ = 14;
    Stats stats_;
    void drop_head() noexcept;
};

// Interleaved stereo s16 frames in, fixed-size blocks out.
class PcmRing
{
  public:
    static constexpr std::size_t kFrames = 8192;
    std::size_t frames() const noexcept
    {
        return count_;
    }
    std::size_t space() const noexcept
    {
        return kFrames - count_;
    }
    void push(const std::int16_t *stereo, std::size_t frames) noexcept;
    // Takes `frames` frames into out; false (nothing taken) when there are fewer.
    bool pop(std::int16_t *out, std::size_t frames) noexcept;
    void clear() noexcept
    {
        head_ = count_ = 0;
    }

  private:
    std::int16_t buf_[kFrames * 2];
    std::size_t head_ = 0, count_ = 0;
};
} // namespace play::audio
