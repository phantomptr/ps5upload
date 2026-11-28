// PS5Upload Play - the app's network side on a computer, for tests and debugging:
//   play-cli discover
//   play-cli join ADDR PORT [CODE] [--seconds N] [--dump FILE] [--leave-after N]
//   play-cli companion ADDR PORT [TOKEN] [--pair] [--seconds N] [--invite] [--allow-first]
// The same guest and companion code as the console's; pictures and sound are counted (and the
// pictures optionally written out as an Annex-B file to check with ffmpeg).
// SPDX-License-Identifier: GPL-3.0-or-later
#include "../src/app/companion.hpp"
#include "../src/app/guest.hpp"
#include "../src/app/log.hpp"
#include "../src/core/proto.hpp"

#include <cstdio>
#include <cstdlib>
#include <cstring>

using namespace play;

namespace
{
class CountSinks final : public GuestSinks
{
  public:
    FILE *dump = nullptr;
    unsigned headers = 0, pictures = 0, keyframes = 0, sounds = 0, rumbles = 0, lights = 0;
    bool hevc = false, need_key = true;
    std::uint64_t bytes = 0;
    void begin(bool h) override
    {
        hevc = h;
        need_key = true;
        std::printf("begin hevc=%d\n", h);
    }
    void end() override
    {
        std::printf("end\n");
    }
    void video_header(const std::uint8_t *d, std::size_t n) override
    {
        ++headers;
        if (dump)
            std::fwrite(d, 1, n, dump);
    }
    void video(const std::uint8_t *d, std::size_t n, std::uint64_t) override
    {
        ++pictures;
        bytes += n;
        if (proto::is_keyframe(d, n, hevc))
        {
            ++keyframes;
            need_key = false;
        }
        if (dump && !need_key)
            std::fwrite(d, 1, n, dump);
    }
    void audio_header(const std::uint8_t *, std::size_t) override
    {
    }
    void audio(std::uint16_t, const std::uint8_t *, std::size_t) override
    {
        ++sounds;
    }
    void rumble(std::uint8_t l, std::uint8_t r) override
    {
        ++rumbles;
        std::printf("rumble %u %u\n", l, r);
    }
    void light(std::uint8_t r, std::uint8_t g, std::uint8_t b) override
    {
        ++lights;
        std::printf("light %u %u %u\n", r, g, b);
    }
    bool wants_keyframe() override
    {
        return need_key;
    }
    void stats(char *out, std::size_t cap) override
    {
        std::snprintf(out, cap, "\"frames\":%u,\"dropped\":0,\"decoder\":\"native\",\"snd\":\"native\",\"snd_decoded\":%u",
                      pictures, sounds);
    }
};

std::uint8_t big[8u << 20];

int arg_int(int argc, char **argv, const char *name, int fallback)
{
    for (int i = 1; i + 1 < argc; ++i)
        if (!std::strcmp(argv[i], name))
            return std::atoi(argv[i + 1]);
    return fallback;
}
const char *arg_str(int argc, char **argv, const char *name)
{
    for (int i = 1; i + 1 < argc; ++i)
        if (!std::strcmp(argv[i], name))
            return argv[i + 1];
    return nullptr;
}
bool has(int argc, char **argv, const char *name)
{
    for (int i = 1; i < argc; ++i)
        if (!std::strcmp(argv[i], name))
            return true;
    return false;
}

const char *phase_name(int p)
{
    static const char *n[] = {"idle", "connecting", "asking", "waiting", "joined", "reconnecting", "refused", "ended", "failed"};
    return p >= 0 && p < 9 ? n[p] : "?";
}

int join(int argc, char **argv)
{
    if (argc < 4)
        return 2;
    const char *code = argc > 4 && argv[4][0] != '-' ? argv[4] : "";
    CountSinks sinks;
    if (const char *f = arg_str(argc, argv, "--dump"))
        sinks.dump = std::fopen(f, "wb");
    static Guest guest(sinks, big, sizeof big);
    guest.start(argv[2], static_cast<std::uint16_t>(std::atoi(argv[3])), code, "play-cli", "13.0.0");
    const int seconds = arg_int(argc, argv, "--seconds", 10);
    const int leave_after = arg_int(argc, argv, "--leave-after", -1);
    const std::uint64_t t0 = net::now_us();
    int last_phase = -1;
    std::uint32_t tick = 0;
    while (guest.running())
    {
        const auto v = guest.view();
        if (v.phase != last_phase)
        {
            std::printf("phase %s %s\n", phase_name(v.phase), v.why);
            std::fflush(stdout);
            last_phase = v.phase;
        }
        const double t = static_cast<double>(net::now_us() - t0) / 1e6;
        if (v.phase == GuestView::joined)
        {
            // a pad: cross pressed every other half second, the left stick moving
            proto::PadState s;
            s.buttons = (tick / 25) % 2 ? proto::pad::kCross : 0;
            s.lx = static_cast<std::uint8_t>(tick * 3);
            guest.send_pad(s);
            ++tick;
        }
        if ((leave_after >= 0 && t >= leave_after) || t >= seconds)
            guest.leave();
        net::sleep_ms(20);
    }
    guest.join();
    const auto v = guest.view();
    std::printf("phase %s %s\n", phase_name(v.phase), v.why);
    std::printf("result player=%d headers=%u pictures=%u keyframes=%u bytes=%llu sounds=%u rumbles=%u lights=%u pads=%llu rtt=%d delivery_ms=%d\n",
                v.player, sinks.headers, sinks.pictures, sinks.keyframes, static_cast<unsigned long long>(sinks.bytes),
                sinks.sounds, sinks.rumbles, sinks.lights, static_cast<unsigned long long>(v.pads_sent), v.rtt_ms,
                v.delivery_ms);
    if (sinks.dump)
        std::fclose(sinks.dump);
    return 0;
}

void on_token(const char *addr, std::uint16_t port, const char *token, void *)
{
    std::printf("token %s:%u %s\n", addr, port, token[0] ? "(new)" : "(cleared)");
    std::fflush(stdout);
    if (const char *f = std::getenv("PLAY_CLI_TOKEN_FILE"))
        if (FILE *fp = std::fopen(f, "w"))
        {
            std::fputs(token, fp);
            std::fclose(fp);
        }
}

int companion(int argc, char **argv)
{
    if (argc < 4)
        return 2;
    const char *token = argc > 4 && argv[4][0] != '-' ? argv[4] : "";
    static Companion c(big, sizeof big);
    c.start(argv[2], static_cast<std::uint16_t>(std::atoi(argv[3])), token, "13.0.0", on_token, nullptr);
    const int seconds = arg_int(argc, argv, "--seconds", 10);
    const bool want_pair = has(argc, argv, "--pair");
    const bool want_invite = has(argc, argv, "--invite");
    const bool allow_first = has(argc, argv, "--allow-first");
    bool asked_pair = false, asked_invite = false;
    std::uint32_t rev = 0;
    int phase = -1;
    const std::uint64_t t0 = net::now_us();
    while (net::now_us() - t0 < static_cast<std::uint64_t>(seconds) * 1000000)
    {
        const auto v = c.view();
        if (v.phase != phase)
        {
            static const char *n[] = {"idle", "connecting", "unpaired", "pairing", "paired", "retrying", "failed"};
            std::printf("phase %s %s %s\n", n[v.phase], v.why, v.auth);
            std::fflush(stdout);
            phase = v.phase;
        }
        if (v.phase == CompanionView::unpaired && want_pair && !asked_pair)
            asked_pair = c.pair();
        if (v.phase == CompanionView::pairing && asked_pair)
        {
            std::printf("pair_code %s\n", v.pair_code);
            std::fflush(stdout);
            asked_pair = false;
        }
        if (v.doc.rev != rev)
        {
            rev = v.doc.rev;
            const auto &d = v.doc;
            std::printf("doc paired=%d engine=%s v=%s console=%s playing=%d together=%d invite=%s asks=%d guests=%d rp=%d helper=%d block=%d max=%d\n",
                        d.paired, d.engine_name, d.engine_version, d.console_addr, d.playing, d.together_open,
                        d.code_display, d.n_asks, d.n_guests, d.rp_paired, d.helper_present, d.block_system_buttons,
                        d.max_players);
            std::fflush(stdout);
            if (want_invite && d.paired && !asked_invite)
                asked_invite = c.invite(false);
            if (allow_first && d.n_asks > 0)
                c.decide(d.asks[0].id, true);
        }
        if (v.phase == CompanionView::failed)
            break;
        net::sleep_ms(50);
    }
    c.stop();
    return 0;
}

int discover()
{
    net::Answer a[16];
    const int n = net::discover(nullptr, 0, 1500, a, 16);
    for (int i = 0; i < n; ++i)
    {
        char ip[16];
        net::ip_text(a[i].ip_be, ip);
        std::printf("found %s:%u name=%s version=%s id=%s\n", ip, a[i].info.port, a[i].info.name, a[i].info.version,
                    a[i].info.id);
    }
    std::printf("found %d\n", n);
    return 0;
}
} // namespace

int main(int argc, char **argv)
{
    log_open(std::getenv("PLAY_CLI_LOG"));
    net::init();
    if (argc >= 2 && !std::strcmp(argv[1], "join"))
        return join(argc, argv);
    if (argc >= 2 && !std::strcmp(argv[1], "companion"))
        return companion(argc, argv);
    if (argc >= 2 && !std::strcmp(argv[1], "discover"))
        return discover();
    std::fprintf(stderr, "usage: play-cli discover | join ADDR PORT [CODE] | companion ADDR PORT [TOKEN] [--pair]\n");
    return 2;
}
