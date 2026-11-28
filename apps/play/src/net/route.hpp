// PS5Upload Play - choosing the way to the host's engine (docs/ROUTES.md): the tiers, a meter per
// way, the choice at connect and the switch rules during play, and the budget for checking the
// backups. Pure logic, no sockets and no clock of its own (every call takes `now` in µs), so the
// host tests drive it with any timeline.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::route
{
// What the way is, shortest first. The words go into the stats the host sees.
enum class Kind : std::uint8_t
{
    lan,       // the host's engine on this console's home network
    tailscale, // over Tailscale (the helper's forward on this console, or a 100.x address directly)
    internet,  // straight across the internet: through this home's PS5Upload (the bridge)
    // No relay: the hub only introduces the two sides (docs/ROUTES.md, "Why no relay").
};
const char *kind_word(Kind k) noexcept;

// How this app dials it.
enum class Way : std::uint8_t
{
    direct,  // addr:port as it is
    helper,  // the helper's Tailscale forward on 127.0.0.1 (addr = the engine's tailnet address)
    bridge,  // this home's PS5Upload engine (addr:port) joins the host's internet room for us
};

// Better is larger. `down`: no answer.
enum class Tier : std::uint8_t
{
    down,
    poor,
    playable,
    good,
    excellent,
};
const char *tier_word(Tier t) noexcept;

// The lines between the tiers (docs/ROUTES.md has where each number comes from). A reading must be
// inside every line of a tier to be in it; anything past Playable's lines is Poor.
struct Limits
{
    int rtt_ms;        // round trip
    int jitter_ms;     // how much the round trip (or the pictures' arrival) moves
    int loss_permille; // checks or pictures lost, per thousand
    int headroom_pct;  // what the way carries over what the stream needs, percent
};
// [0] Excellent, [1] Good, [2] Playable.
extern const Limits kLimits[3];

// What a meter says now. -1: not known (a part not known never lowers the tier).
struct Reading
{
    int rtt_ms = -1;
    int jitter_ms = -1;
    int loss_permille = -1;
    int headroom_pct = -1;
    // The way fell behind lately (the engine skipped pictures to the next keyframe for us): once
    // lowers the tier to Playable at best, three times or more to Poor.
    int behind = 0;
    bool down = false; // no answer for a while, or the connection is gone
    int samples = 0;   // answers so far: none yet is down
};
Tier classify(const Reading &r) noexcept;

// The round trips, their spread and the losses of one way, from checks (ping / answer) and, on the
// way in use, the pictures' arrival.
class Meter
{
  public:
    static constexpr std::uint64_t kAnswerWithinUs = 1000000; // a check unanswered this long is lost
    static constexpr std::uint64_t kDownAfterUs = 3000000;    // nothing at all this long: down
    static constexpr int kWindow = 32;                       // checks the loss counts over
    static constexpr std::uint64_t kBehindForUs = 10000000;  // a fall-behind counts this long

    void reset() noexcept;
    // A check went out; answered() with the same `id` closes it (unanswered ones are lost after
    // kAnswerWithinUs; a later answer to a lost one is ignored).
    void sent(std::uint32_t id, std::uint64_t now) noexcept;
    void answered(std::uint32_t id, std::uint64_t now) noexcept;
    // A round trip measured some other way (the guest socket's own ping, a connect time).
    void rtt(std::uint64_t rtt_us, std::uint64_t now) noexcept;
    // A picture arrived (the engine's time it was whole, ours now): the arrival jitter
    // (RFC 3550's interarrival estimate).
    void picture(std::uint64_t engine_us, std::uint64_t now) noexcept;
    // Anything came in (the connection is alive).
    void heard(std::uint64_t now) noexcept
    {
        last_heard_ = now;
    }
    // The engine skipped pictures for us (its status' `skipped` went up).
    void behind(std::uint64_t now) noexcept;
    // A short burst at connect: `bytes` in `us` over a way with round trip `rtt_us` (a burst
    // shorter than 4 round trips says nothing, and one under the stream's rate in fewer than 8
    // neither: TCP's slow start would make it look slow). Set need_kbps() first.
    void burst(std::uint64_t bytes, std::uint64_t us, std::uint64_t rtt_us) noexcept;
    // What the stream needs, kbit/s (0: not known: no headroom part).
    void need_kbps(unsigned kbps) noexcept
    {
        need_kbps_ = kbps;
    }
    void lost_connection(std::uint64_t now) noexcept;

    Reading reading(std::uint64_t now) const noexcept;
    Tier tier(std::uint64_t now) const noexcept
    {
        return classify(reading(now));
    }
    unsigned capacity_kbps() const noexcept
    {
        return cap_kbps_;
    }

  private:
    void outcome(bool lost) noexcept;
    void sweep(std::uint64_t now) noexcept;

    struct Pending
    {
        std::uint32_t id;
        std::uint64_t at;
    };
    static constexpr int kPending = 8;
    Pending pending_[kPending] = {};
    int n_pending_ = 0;
    std::uint64_t srtt_us_ = 0;   // smoothed round trip (RFC 6298, gain 1/8)
    std::uint64_t rtt_jit_us_ = 0; // mean change between round trips (gain 1/4)
    std::uint64_t last_rtt_us_ = 0;
    std::uint64_t pic_jit_us_ = 0; // RFC 3550 J (gain 1/16)
    std::int64_t last_transit_ = 0;
    bool have_transit_ = false;
    std::uint64_t outcomes_ = 0; // bit i: check i lost (newest in bit 0)
    int n_outcomes_ = 0;
    int samples_ = 0;
    std::uint64_t last_heard_ = 0, last_sent_ = 0;
    std::uint64_t behind_at_[4] = {};
    unsigned cap_kbps_ = 0, need_kbps_ = 0;
    bool gone_ = false;
};

// One way to the engine.
struct Candidate
{
    Kind kind = Kind::lan;
    Way way = Way::direct;
    char addr[64] = {}; // dialled (Way::direct, bridge) or the engine's tailnet address (helper)
    std::uint16_t port = 0;
    // The guest socket's path when the engine is served under a prefix (an invite link's
    // `/prefix/join`): "" = /api/join/ws. Its checks socket is beside it (`…/api/join/probe`).
    char path[80] = {};
    // An internet invite this way joins (Way::bridge): the room and the engine's certificate.
    char room[24] = {};
    char fp[100] = {};
};

// Which way is used and when to change it. Holds a meter per way; the caller feeds the meters
// and asks decide() now and then (every second is plenty).
class Selector
{
  public:
    static constexpr int kMax = 6;
    // No change by choice this soon after the last (or the start).
    static constexpr std::uint64_t kDwellUs = 20000000;
    // A backup better by a tier for this long replaces a way that still works...
    static constexpr std::uint64_t kUpgradeHoldUs = 10000000;
    // ...and this long when the way in use is Poor.
    static constexpr std::uint64_t kRescueHoldUs = 3000000;
    // Going back to a way left less than this ago takes twice the hold.
    static constexpr std::uint64_t kLeftRecentlyUs = 120000000;
    // Changes by choice at most this many in kBudgetWindowUs (a lost way may always be left).
    static constexpr int kBudget = 3;
    static constexpr std::uint64_t kBudgetWindowUs = 600000000;

    void clear() noexcept;
    int add(const Candidate &c) noexcept; // its index, or -1 when full
    int size() const noexcept
    {
        return n_;
    }
    const Candidate &at(int i) const noexcept
    {
        return c_[i];
    }
    Meter &meter(int i) noexcept
    {
        return m_[i];
    }
    const Meter &meter(int i) const noexcept
    {
        return m_[i];
    }

    // The best way now: the highest tier, then the shortest kind, then the lowest round trip;
    // -1 when every way is down. `skip`: one to leave out (-1: none).
    int best(std::uint64_t now, int skip = -1) const noexcept;
    // Better: a is preferred over b at the connect (the order best() uses).
    bool better(int a, int b, std::uint64_t now) const noexcept;

    int active() const noexcept
    {
        return active_;
    }
    // The way now in use (at connect, or after a switch the caller made).
    void use(int i, std::uint64_t now, const char *why) noexcept;

    struct Decision
    {
        int to = -1;               // the way to change to, or -1: stay
        const char *why = "";      // "lost", "rescue", "upgrade"
    };
    // Call often; it keeps its own timers. A way it names is not used until the caller says use().
    Decision decide(std::uint64_t now) noexcept;

    // The last changes, newest last.
    struct Change
    {
        std::uint64_t at = 0;
        std::int8_t from = -1, to = -1;
        Tier from_tier = Tier::down, to_tier = Tier::down;
        char why[12] = {};
    };
    static constexpr int kHistory = 8;
    int history(Change *out, int cap) const noexcept;
    int changes() const noexcept
    {
        return changes_;
    }

  private:
    Candidate c_[kMax];
    Meter m_[kMax];
    int n_ = 0;
    int active_ = -1;
    std::uint64_t since_ = 0;
    std::uint64_t better_since_[kMax] = {}; // since when that backup has been a tier above
    std::uint64_t left_at_[kMax] = {};
    std::uint64_t poor_since_ = 0;
    std::uint64_t by_choice_[kBudget] = {};
    Change hist_[kHistory];
    int n_hist_ = 0;
    int changes_ = 0;
};

// The budget for checking the backups during play: a token bucket in bytes on the wire (both
// ways), so no number of backups can ever take more than `rate` bytes a second.
class Pacer
{
  public:
    Pacer(unsigned bytes_per_s, unsigned burst) noexcept : rate_{bytes_per_s}, burst_{burst}, tokens_{burst}
    {
    }
    bool take(std::uint64_t now, unsigned bytes) noexcept;
    unsigned rate() const noexcept
    {
        return rate_;
    }

  private:
    unsigned rate_, burst_;
    std::uint64_t tokens_; // bytes
    std::uint64_t at_ = 0;
    bool started_ = false;
};

// Checking the backups while playing: at most this many bytes a second for all of them together
// (a check and its answer are about 160 bytes on the wire), each at most every kBackupEveryUs.
constexpr unsigned kBackupBytesPerS = 256;
constexpr std::uint64_t kBackupEveryUs = 2000000;
constexpr unsigned kCheckWireBytes = 160;
constexpr unsigned kBackupBurst = 3 * kCheckWireBytes; // the bucket's depth
// At connect: how long the race may take before the best answer so far wins.
constexpr unsigned kRaceMs = 900;
// The burst at connect (never during play).
constexpr unsigned kBurstBytes = 256 * 1024;

// The stream's bitrate the way carries with room to spare (docs/ROUTES.md: 1.25x), kbit/s; 0 when
// not known. What the host is told to lower to when a guest's way can't carry the stream.
unsigned fit_kbps(unsigned capacity_kbps) noexcept;

// "lan" / "tailscale" / "internet" for an IPv4 address in text (the same rule as the engine's
// reach_rank and the browser guest's netPath.ts): private ranges are the home network, 100.64/10
// Tailscale, anything else the internet.
Kind kind_of_addr(const char *addr) noexcept;
} // namespace play::route
