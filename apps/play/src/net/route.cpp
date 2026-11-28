// PS5Upload Play - choosing the way to the host's engine (route.hpp, docs/ROUTES.md).
// SPDX-License-Identifier: GPL-3.0-or-later
#include "route.hpp"

#include <cstdio>
#include <cstring>

namespace play::route
{
// docs/ROUTES.md, "The tiers": round trip, jitter, loss per thousand, headroom percent.
const Limits kLimits[3] = {
    {20, 5, 1, 150},   // Excellent: a home network or a near Tailscale
    {40, 15, 5, 125},  // Good
    {80, 30, 10, 100}, // Playable
};

const char *kind_word(Kind k) noexcept
{
    switch (k)
    {
    case Kind::lan:
        return "lan";
    case Kind::tailscale:
        return "tailscale";
    case Kind::internet:
        return "internet";
    }
    return "lan";
}

const char *tier_word(Tier t) noexcept
{
    switch (t)
    {
    case Tier::excellent:
        return "excellent";
    case Tier::good:
        return "good";
    case Tier::playable:
        return "playable";
    case Tier::poor:
        return "poor";
    case Tier::down:
        return "down";
    }
    return "down";
}

namespace
{
bool within(const Reading &r, const Limits &l) noexcept
{
    return (r.rtt_ms < 0 || r.rtt_ms <= l.rtt_ms) && (r.jitter_ms < 0 || r.jitter_ms <= l.jitter_ms) &&
           (r.loss_permille < 0 || r.loss_permille <= l.loss_permille) &&
           (r.headroom_pct < 0 || r.headroom_pct >= l.headroom_pct);
}

Tier lower(Tier a, Tier b) noexcept
{
    return a < b ? a : b;
}

int popcount64(std::uint64_t v) noexcept
{
    int n = 0;
    for (; v; v &= v - 1)
        ++n;
    return n;
}

std::uint64_t absdiff(std::uint64_t a, std::uint64_t b) noexcept
{
    return a > b ? a - b : b - a;
}
} // namespace

Tier classify(const Reading &r) noexcept
{
    if (r.down || r.samples <= 0)
        return Tier::down;
    Tier t = Tier::poor;
    if (within(r, kLimits[0]))
        t = Tier::excellent;
    else if (within(r, kLimits[1]))
        t = Tier::good;
    else if (within(r, kLimits[2]))
        t = Tier::playable;
    if (r.behind >= 3)
        t = lower(t, Tier::poor);
    else if (r.behind >= 1)
        t = lower(t, Tier::playable);
    return t;
}

// ---- Meter ----------------------------------------------------------------------------------

void Meter::reset() noexcept
{
    *this = Meter{};
}

void Meter::outcome(bool lost) noexcept
{
    outcomes_ = outcomes_ << 1 | (lost ? 1u : 0u);
    if (n_outcomes_ < kWindow)
        ++n_outcomes_;
}

void Meter::sweep(std::uint64_t now) noexcept
{
    int k = 0;
    for (int i = 0; i < n_pending_; ++i)
    {
        if (now - pending_[i].at >= kAnswerWithinUs)
            outcome(true);
        else
            pending_[k++] = pending_[i];
    }
    n_pending_ = k;
}

void Meter::sent(std::uint32_t id, std::uint64_t now) noexcept
{
    sweep(now);
    if (n_pending_ == kPending)
    {
        // the oldest is given up
        outcome(true);
        std::memmove(pending_, pending_ + 1, sizeof(Pending) * (kPending - 1));
        --n_pending_;
    }
    pending_[n_pending_++] = Pending{id, now};
    last_sent_ = now;
}

void Meter::answered(std::uint32_t id, std::uint64_t now) noexcept
{
    // A way back after being down is measured afresh (its old losses say nothing now).
    if (last_sent_ > last_heard_ && now - last_heard_ > kDownAfterUs)
    {
        outcomes_ = 0, n_outcomes_ = 0, samples_ = 0, have_transit_ = false;
        int keep = 0;
        for (int i = 0; i < n_pending_ && !keep; ++i)
            if (pending_[i].id == id)
                pending_[0] = pending_[i], keep = 1;
        n_pending_ = keep;
    }
    sweep(now);
    for (int i = 0; i < n_pending_; ++i)
        if (pending_[i].id == id)
        {
            const std::uint64_t at = pending_[i].at;
            pending_[i] = pending_[--n_pending_];
            outcome(false);
            rtt(now - at, now);
            return;
        }
}

void Meter::rtt(std::uint64_t rtt_us, std::uint64_t now) noexcept
{
    heard(now);
    gone_ = false;
    if (samples_ == 0)
        srtt_us_ = rtt_us;
    else
    {
        // RFC 6298: srtt += (rtt - srtt) / 8; the change between two round trips with gain 1/4
        const std::int64_t d = static_cast<std::int64_t>(rtt_us) - static_cast<std::int64_t>(srtt_us_);
        srtt_us_ = static_cast<std::uint64_t>(static_cast<std::int64_t>(srtt_us_) + d / 8);
        const std::uint64_t ch = absdiff(rtt_us, last_rtt_us_);
        rtt_jit_us_ = static_cast<std::uint64_t>(static_cast<std::int64_t>(rtt_jit_us_) +
                                                 (static_cast<std::int64_t>(ch) - static_cast<std::int64_t>(rtt_jit_us_)) / 4);
    }
    last_rtt_us_ = rtt_us;
    ++samples_;
}

void Meter::picture(std::uint64_t engine_us, std::uint64_t now) noexcept
{
    heard(now);
    // RFC 3550 A.8: the transit time's change, J += (|D| - J) / 16 (the clocks' offset cancels)
    const std::int64_t transit = static_cast<std::int64_t>(now) - static_cast<std::int64_t>(engine_us);
    if (have_transit_)
    {
        const std::int64_t d = transit - last_transit_;
        const std::int64_t ad = d < 0 ? -d : d;
        pic_jit_us_ = static_cast<std::uint64_t>(static_cast<std::int64_t>(pic_jit_us_) +
                                                 (ad - static_cast<std::int64_t>(pic_jit_us_)) / 16);
    }
    last_transit_ = transit;
    have_transit_ = true;
}

void Meter::behind(std::uint64_t now) noexcept
{
    std::memmove(behind_at_ + 1, behind_at_, sizeof behind_at_ - sizeof behind_at_[0]);
    behind_at_[0] = now ? now : 1;
}

void Meter::burst(std::uint64_t bytes, std::uint64_t us, std::uint64_t rtt_us) noexcept
{
    if (!us || us < 4 * rtt_us)
        return;
    const unsigned kbps = static_cast<unsigned>(bytes * 8000 / us);
    // Under the stream's rate after fewer than 8 round trips may still be slow start: it says
    // nothing either way (over the rate it says enough).
    if (need_kbps_ && kbps < need_kbps_ && us < 8 * rtt_us)
        return;
    cap_kbps_ = kbps;
}

void Meter::lost_connection(std::uint64_t now) noexcept
{
    (void)now;
    gone_ = true;
}

Reading Meter::reading(std::uint64_t now) const noexcept
{
    Reading r;
    r.samples = samples_;
    if (samples_ > 0)
        r.rtt_ms = static_cast<int>((srtt_us_ + 500) / 1000);
    if (samples_ > 1 || have_transit_)
    {
        const std::uint64_t j = rtt_jit_us_ > pic_jit_us_ ? rtt_jit_us_ : pic_jit_us_;
        r.jitter_ms = static_cast<int>((j + 500) / 1000);
    }
    // the checks given up by now count as lost, as sweep() will count them
    std::uint64_t o = outcomes_;
    int n = n_outcomes_;
    for (int i = 0; i < n_pending_; ++i)
        if (now - pending_[i].at >= kAnswerWithinUs)
        {
            o = o << 1 | 1u;
            n = n < kWindow ? n + 1 : n;
        }
    if (n >= 5)
    {
        const std::uint64_t mask = n >= 64 ? ~0ull : (1ull << n) - 1;
        r.loss_permille = popcount64(o & mask) * 1000 / n;
    }
    if (need_kbps_ && cap_kbps_)
        r.headroom_pct = static_cast<int>(static_cast<std::uint64_t>(cap_kbps_) * 100 / need_kbps_);
    for (const std::uint64_t at : behind_at_)
        if (at && now - at < kBehindForUs)
            ++r.behind;
    r.down = gone_ || (last_sent_ > last_heard_ && now - last_heard_ > kDownAfterUs);
    return r;
}

// ---- Selector -------------------------------------------------------------------------------

void Selector::clear() noexcept
{
    *this = Selector{};
}

int Selector::add(const Candidate &c) noexcept
{
    if (n_ == kMax)
        return -1;
    c_[n_] = c;
    m_[n_].reset();
    return n_++;
}

bool Selector::better(int a, int b, std::uint64_t now) const noexcept
{
    const Reading ra = m_[a].reading(now), rb = m_[b].reading(now);
    const Tier ta = classify(ra), tb = classify(rb);
    if (ta != tb)
        return ta > tb;
    if (c_[a].kind != c_[b].kind)
        return c_[a].kind < c_[b].kind;
    const int xa = ra.rtt_ms < 0 ? 1 << 30 : ra.rtt_ms, xb = rb.rtt_ms < 0 ? 1 << 30 : rb.rtt_ms;
    return xa < xb;
}

int Selector::best(std::uint64_t now, int skip) const noexcept
{
    int b = -1;
    for (int i = 0; i < n_; ++i)
    {
        if (i == skip || m_[i].tier(now) == Tier::down)
            continue;
        if (b < 0 || better(i, b, now))
            b = i;
    }
    return b;
}

void Selector::use(int i, std::uint64_t now, const char *why) noexcept
{
    if (i < 0 || i >= n_ || i == active_)
        return;
    Change ch;
    ch.at = now;
    ch.from = static_cast<std::int8_t>(active_);
    ch.to = static_cast<std::int8_t>(i);
    ch.from_tier = active_ >= 0 ? m_[active_].tier(now) : Tier::down;
    ch.to_tier = m_[i].tier(now);
    std::snprintf(ch.why, sizeof ch.why, "%s", why ? why : "");
    if (n_hist_ == kHistory)
    {
        std::memmove(hist_, hist_ + 1, sizeof(Change) * (kHistory - 1));
        --n_hist_;
    }
    hist_[n_hist_++] = ch;
    if (active_ >= 0)
    {
        left_at_[active_] = now;
        ++changes_;
    }
    if (why && (!std::strcmp(why, "upgrade") || !std::strcmp(why, "rescue")))
    {
        std::memmove(by_choice_ + 1, by_choice_, sizeof by_choice_ - sizeof by_choice_[0]);
        by_choice_[0] = now ? now : 1;
    }
    active_ = i;
    since_ = now;
    poor_since_ = 0;
    for (auto &b : better_since_)
        b = 0;
}

Selector::Decision Selector::decide(std::uint64_t now) noexcept
{
    Decision d;
    if (!n_)
        return d;
    if (active_ < 0)
    {
        d.to = best(now);
        d.why = "start";
        return d;
    }
    const Tier a = m_[active_].tier(now);
    if (a == Tier::down)
    {
        d.to = best(now, active_);
        d.why = "lost";
        return d;
    }
    poor_since_ = a == Tier::poor ? (poor_since_ ? poor_since_ : now) : 0;
    int cand = -1;
    for (int i = 0; i < n_; ++i)
    {
        if (i == active_)
            continue;
        if (m_[i].tier(now) > a)
        {
            if (!better_since_[i])
                better_since_[i] = now;
            if (cand < 0 || better(i, cand, now))
                cand = i;
        }
        else
            better_since_[i] = 0;
    }
    if (cand < 0)
        return d;
    int used = 0;
    for (const std::uint64_t t : by_choice_)
        if (t && now - t < kBudgetWindowUs)
            ++used;
    if (used >= kBudget)
        return d;
    const bool rescue = a == Tier::poor;
    if (!rescue && now - since_ < kDwellUs)
        return d;
    std::uint64_t hold = rescue ? kRescueHoldUs : kUpgradeHoldUs;
    if (left_at_[cand] && now - left_at_[cand] < kLeftRecentlyUs)
        hold *= 2;
    if (now - better_since_[cand] < hold)
        return d;
    d.to = cand;
    d.why = rescue ? "rescue" : "upgrade";
    return d;
}

int Selector::history(Change *out, int cap) const noexcept
{
    int n = 0;
    for (int i = 0; i < n_hist_ && n < cap; ++i)
        out[n++] = hist_[i];
    return n;
}

// ---- SwitchBackoff --------------------------------------------------------------------------

void SwitchBackoff::failed(std::uint64_t now) noexcept
{
    std::uint64_t w = kFirstUs;
    for (int i = 0; i < fails_ && w < kMaxUs; ++i)
        w *= 2;
    wait_ = w < kMaxUs ? w : kMaxUs;
    until_ = now + wait_;
    if (fails_ < 1000)
        ++fails_;
}

// ---- Pacer ----------------------------------------------------------------------------------

bool Pacer::take(std::uint64_t now, unsigned bytes) noexcept
{
    // tokens in byte-µs (bytes x 1e6), so slow rates add up exactly
    constexpr std::uint64_t kScale = 1000000;
    if (!started_)
    {
        started_ = true;
        at_ = now;
        tokens_ = static_cast<std::uint64_t>(burst_) * kScale;
    }
    else if (now > at_)
    {
        tokens_ += (now - at_) * rate_;
        at_ = now;
        const std::uint64_t cap = static_cast<std::uint64_t>(burst_) * kScale;
        if (tokens_ > cap)
            tokens_ = cap;
    }
    const std::uint64_t need = static_cast<std::uint64_t>(bytes) * kScale;
    if (tokens_ < need)
        return false;
    tokens_ -= need;
    return true;
}

// ---- helpers --------------------------------------------------------------------------------

unsigned fit_kbps(unsigned capacity_kbps) noexcept
{
    return static_cast<unsigned>(static_cast<std::uint64_t>(capacity_kbps) * 100 / 125);
}

Kind kind_of_addr(const char *addr) noexcept
{
    if (!addr)
        return Kind::lan;
    unsigned o[4] = {};
    int part = 0, digits = 0;
    for (const char *p = addr;; ++p)
    {
        if (*p >= '0' && *p <= '9' && digits < 3)
            o[part] = o[part] * 10 + static_cast<unsigned>(*p - '0'), ++digits;
        else if (*p == '.' && digits && part < 3)
            ++part, digits = 0;
        else if (!*p && digits && part == 3)
            break;
        else
            return Kind::lan; // a name: the home network's (a typed .local or router name)
    }
    if (o[0] > 255 || o[1] > 255 || o[2] > 255 || o[3] > 255)
        return Kind::lan;
    if (o[0] == 10 || (o[0] == 172 && o[1] >= 16 && o[1] < 32) || (o[0] == 192 && o[1] == 168) ||
        (o[0] == 169 && o[1] == 254) || o[0] == 127)
        return Kind::lan;
    if (o[0] == 100 && o[1] >= 64 && o[1] < 128)
        return Kind::tailscale;
    return Kind::internet;
}
} // namespace play::route
