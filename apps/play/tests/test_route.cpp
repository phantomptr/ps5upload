// PS5Upload Play - host unit tests for choosing the way to the engine (src/net/route.cpp,
// docs/ROUTES.md): the tiers, the meters, the choice at connect, the switch rules (no flapping)
// and the budget for checking backups.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "../src/net/route.hpp"

#include <cstdio>
#include <cstring>

namespace
{
int failures = 0, checks = 0;
#define CHECK(cond)                                                                            \
    do                                                                                         \
    {                                                                                          \
        ++checks;                                                                              \
        if (!(cond))                                                                           \
        {                                                                                      \
            ++failures;                                                                        \
            std::printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond);                       \
        }                                                                                      \
    } while (0)

using namespace play::route;
constexpr std::uint64_t S = 1000000; // µs in a second

Reading rd(int rtt, int jit = -1, int loss = -1, int head = -1, int behind = 0)
{
    Reading r;
    r.rtt_ms = rtt;
    r.jitter_ms = jit;
    r.loss_permille = loss;
    r.headroom_pct = head;
    r.behind = behind;
    r.samples = 10;
    return r;
}

void test_tiers()
{
    CHECK(classify(rd(2, 1, 0, 400)) == Tier::excellent);
    CHECK(classify(rd(20, 5, 1, 150)) == Tier::excellent); // on the lines
    CHECK(classify(rd(21)) == Tier::good);
    CHECK(classify(rd(40, 15, 5, 125)) == Tier::good);
    CHECK(classify(rd(41)) == Tier::playable);
    CHECK(classify(rd(80, 30, 10, 100)) == Tier::playable);
    CHECK(classify(rd(81)) == Tier::poor);
    CHECK(classify(rd(225)) == Tier::poor); // a phone on mobile data across the hub (measured)
    // the worst part decides
    CHECK(classify(rd(3, 20)) == Tier::playable);
    CHECK(classify(rd(3, -1, 20)) == Tier::poor);
    CHECK(classify(rd(3, -1, 3)) == Tier::good);
    CHECK(classify(rd(3, -1, -1, 110)) == Tier::playable);
    CHECK(classify(rd(3, -1, -1, 90)) == Tier::poor);
    // falling behind caps it
    CHECK(classify(rd(3, 1, 0, -1, 1)) == Tier::playable);
    CHECK(classify(rd(3, 1, 0, -1, 3)) == Tier::poor);
    // nothing known is down; unknown parts never lower it
    Reading none;
    CHECK(classify(none) == Tier::down);
    Reading gone = rd(3);
    gone.down = true;
    CHECK(classify(gone) == Tier::down);
    CHECK(classify(rd(10)) == Tier::excellent);
    CHECK(!std::strcmp(tier_word(Tier::playable), "playable") && !std::strcmp(kind_word(Kind::tailscale), "tailscale"));
    CHECK(fit_kbps(20000) == 16000);
}

// One check a second for `secs`, answered after rtt_ms (lost when rtt_ms < 0).
std::uint64_t steady(Meter &m, std::uint64_t t, int secs, int rtt_ms, std::uint32_t *id)
{
    for (int i = 0; i < secs; ++i, t += S)
    {
        m.sent(++*id, t);
        if (rtt_ms >= 0)
            m.answered(*id, t + static_cast<std::uint64_t>(rtt_ms) * 1000);
    }
    return t;
}

void test_meter()
{
    std::uint32_t id = 0;
    Meter m;
    CHECK(m.tier(0) == Tier::down); // never heard
    std::uint64_t t = steady(m, 1000, 10, 4, &id);
    Reading r = m.reading(t);
    CHECK(r.rtt_ms == 4 && r.jitter_ms == 0 && r.loss_permille == 0 && r.samples == 10);
    CHECK(m.tier(t) == Tier::excellent);
    // a late answer to a check given up is ignored, and the check counts as lost
    m.sent(++id, t);
    m.answered(id, t + 2 * S);
    r = m.reading(t + 2 * S);
    CHECK(r.samples == 10 && r.loss_permille > 0);
    // a way that stops answering is down after 3 s
    Meter q;
    t = steady(q, 1000, 5, 10, &id);
    q.sent(++id, t);
    CHECK(q.tier(t + 2 * S) != Tier::down);
    CHECK(q.tier(t + 4 * S) == Tier::down);
    // round trips that swing: jitter
    Meter j;
    t = 0;
    for (int i = 0; i < 20; ++i, t += S)
    {
        j.sent(++id, t);
        j.answered(id, t + (i % 2 ? 10000 : 50000));
    }
    CHECK(j.reading(t).jitter_ms >= 30);
    // pictures arriving unevenly: jitter
    Meter p;
    p.rtt(3000, 0);
    for (int i = 0; i < 200; ++i)
        p.picture(static_cast<std::uint64_t>(i) * 16667, 5000000 + static_cast<std::uint64_t>(i) * 16667 + (i % 2 ? 20000 : 0));
    CHECK(p.reading(5 * S).jitter_ms >= 15);
    // the burst: too short to say anything (slow start), then a real one
    Meter b;
    b.rtt(50000, 0);
    b.need_kbps(15000);
    b.burst(256 * 1024, 100000, 50000);
    CHECK(b.reading(1).headroom_pct == -1);
    b.burst(256 * 1024, 80000, 2000); // 26 Mbit/s
    CHECK(b.capacity_kbps() == 26214 && b.reading(1).headroom_pct == 174);
    // fell behind once: Playable at best for 10 s
    b.behind(2 * S);
    CHECK(b.reading(3 * S).behind == 1 && b.reading(13 * S).behind == 0);
    // a lost connection is down at once
    Meter c;
    c.rtt(3000, 0);
    c.lost_connection(1);
    CHECK(c.tier(1) == Tier::down);
}

Candidate cand(Kind k, const char *addr)
{
    Candidate c;
    c.kind = k;
    std::snprintf(c.addr, sizeof c.addr, "%s", addr);
    c.port = 19113;
    return c;
}

void test_choice_at_connect()
{
    std::uint32_t id = 0;
    Selector s;
    const int lan = s.add(cand(Kind::lan, "192.168.1.20"));
    const int ts = s.add(cand(Kind::tailscale, "100.101.1.2"));
    const int br = s.add(cand(Kind::internet, "192.168.1.30"));
    CHECK(s.best(0) == -1); // nothing answered yet
    steady(s.meter(ts), 0, 5, 3, &id);
    steady(s.meter(lan), 0, 5, 2, &id);
    steady(s.meter(br), 0, 5, 1, &id);
    // all Excellent: the shortest kind wins, not the lowest round trip
    CHECK(s.best(5 * S) == lan);
    // the home network is Poor: the better tier wins
    Selector s2;
    const int l2 = s2.add(cand(Kind::lan, "192.168.1.20"));
    const int t2 = s2.add(cand(Kind::tailscale, "100.101.1.2"));
    steady(s2.meter(l2), 0, 6, 120, &id);
    steady(s2.meter(t2), 0, 6, 30, &id);
    CHECK(s2.best(6 * S) == t2);
    CHECK(s2.best(6 * S, t2) == l2);
    // same tier and kind: the lower round trip
    Selector s3;
    const int a = s3.add(cand(Kind::lan, "192.168.1.20"));
    const int b = s3.add(cand(Kind::lan, "10.0.0.20"));
    steady(s3.meter(a), 0, 5, 9, &id);
    steady(s3.meter(b), 0, 5, 4, &id);
    CHECK(s3.best(5 * S) == b);
    CHECK(s3.decide(5 * S).to == b);
}

// Two ways checked every second until `until`; the way in use changes as decide() says.
struct Sim
{
    Selector s;
    std::uint32_t id = 0;
    std::uint64_t t = 0;
    int switches = 0;
    int run(std::uint64_t until, int (*rtt_of)(int way, std::uint64_t t))
    {
        for (; t < until; t += S)
        {
            for (int i = 0; i < s.size(); ++i)
            {
                const int r = rtt_of(i, t);
                s.meter(i).sent(++id, t);
                if (r >= 0)
                    s.meter(i).answered(id, t + static_cast<std::uint64_t>(r) * 1000);
            }
            const auto d = s.decide(t + S / 2);
            if (d.to >= 0 && d.to != s.active())
            {
                s.use(d.to, t + S / 2, d.why);
                ++switches;
            }
        }
        return switches;
    }
};

void test_switching()
{
    // coming home: Tailscale (Good, 30 ms) in use, the home network turns up (Excellent)
    {
        Sim x;
        const int ts = x.s.add(cand(Kind::tailscale, "100.101.1.2"));
        const int lan = x.s.add(cand(Kind::lan, "192.168.1.20"));
        x.s.use(ts, 0, "start");
        x.run(31 * S, [](int way, std::uint64_t t) { return way == 0 ? 30 : (t < 25 * S ? -1 : 2); });
        CHECK(x.s.active() == ts); // the home network only just answered: held
        x.run(60 * S, [](int way, std::uint64_t) { return way == 0 ? 30 : 2; });
        CHECK(x.s.active() == lan && x.switches == 1);
        Selector::Change h[8];
        const int n = x.s.history(h, 8);
        CHECK(n == 2 && !std::strcmp(h[1].why, "upgrade") && h[1].from == ts && h[1].to == lan);
    }
    // no change within the first 20 s, even for a better way
    {
        Sim x;
        x.s.add(cand(Kind::tailscale, "100.101.1.2"));
        x.s.add(cand(Kind::lan, "192.168.1.20"));
        x.s.use(0, 0, "start");
        x.run(19 * S, [](int way, std::uint64_t) { return way == 0 ? 30 : 2; });
        CHECK(x.switches == 0);
    }
    // a better way for a moment only (5 s) is not taken
    {
        Sim x;
        x.s.add(cand(Kind::tailscale, "100.101.1.2"));
        x.s.add(cand(Kind::lan, "192.168.1.20"));
        x.s.use(0, 0, "start");
        x.run(120 * S, [](int way, std::uint64_t t) {
            return way == 0 ? 30 : ((t / S) % 12 < 5 ? 2 : 60);
        });
        CHECK(x.switches == 0);
    }
    // rescue: the way in use turns Poor; a Playable one takes over after 3 s, dwell or not
    {
        Sim x;
        x.s.add(cand(Kind::lan, "192.168.1.20"));
        x.s.add(cand(Kind::tailscale, "100.101.1.2"));
        x.s.use(0, 0, "start");
        x.run(5 * S, [](int, std::uint64_t) { return 5; });
        x.run(9 * S, [](int way, std::uint64_t) { return way == 0 ? 150 : 60; });
        CHECK(x.s.active() == 0); // not yet: the meter's average still has to get there
        x.run(30 * S, [](int way, std::uint64_t) { return way == 0 ? 150 : 60; });
        Selector::Change h[8];
        const int n = x.s.history(h, 8);
        CHECK(x.s.active() == 1 && n == 2 && !std::strcmp(h[1].why, "rescue"));
    }
    // lost: the way in use stops answering: the best other one at once
    {
        Sim x;
        x.s.add(cand(Kind::lan, "192.168.1.20"));
        x.s.add(cand(Kind::tailscale, "100.101.1.2"));
        x.s.add(cand(Kind::internet, "192.168.1.30"));
        x.s.use(0, 0, "start");
        x.run(5 * S, [](int, std::uint64_t) { return 5; });
        x.s.meter(0).lost_connection(5 * S);
        const auto d = x.s.decide(5 * S);
        CHECK(d.to == 1 && !std::strcmp(d.why, "lost"));
    }
    // never flap: two ways that trade places every 15 s for 10 minutes
    {
        Sim x;
        x.s.add(cand(Kind::lan, "192.168.1.20"));
        x.s.add(cand(Kind::tailscale, "100.101.1.2"));
        x.s.use(0, 0, "start");
        x.run(600 * S, [](int way, std::uint64_t t) {
            const bool a_good = (t / (15 * S)) % 2 == 0;
            return (way == 0) == a_good ? 5 : 60;
        });
        CHECK(x.switches <= Selector::kBudget);
        x.switches = 0;
        x.run(1200 * S, [](int way, std::uint64_t t) {
            const bool a_good = (t / (15 * S)) % 2 == 0;
            return (way == 0) == a_good ? 5 : 60;
        });
        CHECK(x.switches <= Selector::kBudget + 1);
    }
    // nothing better to go to: stays, even when Poor
    {
        Sim x;
        x.s.add(cand(Kind::lan, "192.168.1.20"));
        x.s.add(cand(Kind::tailscale, "100.101.1.2"));
        x.s.use(0, 0, "start");
        x.run(60 * S, [](int way, std::uint64_t) { return way == 0 ? 120 : -1; });
        CHECK(x.switches == 0);
    }
}

void test_pacer()
{
    // three backups wanting a check every 100 ms for a minute get the budget and no more
    Pacer p(kBackupBytesPerS, kBackupBurst);
    std::uint64_t bytes = 0;
    for (std::uint64_t t = 0; t < 60 * S; t += 100000)
        for (int b = 0; b < 3; ++b)
            if (p.take(t, kCheckWireBytes))
                bytes += kCheckWireBytes;
    CHECK(bytes <= kBackupBytesPerS * 60ull + kBackupBurst);
    CHECK(bytes >= kBackupBytesPerS * 60ull - 2 * kCheckWireBytes);
    // with two backups every 2 s the budget is enough for all of them
    Pacer q(kBackupBytesPerS, kBackupBurst);
    int ok = 0, asked = 0;
    for (std::uint64_t t = 0; t < 60 * S; t += kBackupEveryUs)
        for (int b = 0; b < 2; ++b, ++asked)
            ok += q.take(t + static_cast<std::uint64_t>(b) * 50000, kCheckWireBytes);
    CHECK(ok == asked);
}

void test_kinds()
{
    CHECK(kind_of_addr("192.168.86.100") == Kind::lan);
    CHECK(kind_of_addr("10.1.2.3") == Kind::lan);
    CHECK(kind_of_addr("172.20.0.1") == Kind::lan);
    CHECK(kind_of_addr("172.32.0.1") == Kind::internet);
    CHECK(kind_of_addr("100.101.102.103") == Kind::tailscale);
    CHECK(kind_of_addr("100.128.0.1") == Kind::internet);
    CHECK(kind_of_addr("8.8.8.8") == Kind::internet);
    CHECK(kind_of_addr("pc.local") == Kind::lan);
    CHECK(kind_of_addr("1.2.3") == Kind::lan);
    CHECK(kind_of_addr("1.2.3.4.5") == Kind::lan);
}
} // namespace

int main()
{
    test_tiers();
    test_meter();
    test_choice_at_connect();
    test_switching();
    test_pacer();
    test_kinds();
    std::printf("route: %d checks, %d failed\n", checks, failures);
    return failures ? 1 : 0;
}
