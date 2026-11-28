// PS5Upload Play - host unit tests for the portable core (make -C apps/play test).
// SPDX-License-Identifier: GPL-3.0-or-later
#include "../src/core/chord.hpp"
#include "../src/core/hdr.hpp"
#include "../src/core/jitter.hpp"
#include "../src/core/json.hpp"
#include "../src/core/proto.hpp"
#include "../src/core/settings.hpp"
#include "../src/core/ws.hpp"
#include "../src/ui/i18n.hpp"
#include "../src/ui/strings.hpp"

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

using namespace play;

void hex(const std::uint8_t *d, std::size_t n, char *out)
{
    for (std::size_t i = 0; i < n; ++i)
        std::snprintf(out + i * 2, 3, "%02x", d[i]);
}

void test_sha1_base64()
{
    std::uint8_t d[20];
    char h[41];
    ws::sha1("abc", 3, d);
    hex(d, 20, h);
    CHECK(!std::strcmp(h, "a9993e364706816aba3e25717850c26c9cd0d89d"));
    ws::sha1("", 0, d);
    hex(d, 20, h);
    CHECK(!std::strcmp(h, "da39a3ee5e6b4b0d3255bfef95601890afd80709"));
    // 56 bytes: the padding needs a second block
    const char *s56 = "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
    ws::sha1(s56, std::strlen(s56), d);
    hex(d, 20, h);
    CHECK(!std::strcmp(h, "84983e441c3bd26ebaae4aa1f95129e5e54670f1"));
    // 64 bytes exactly
    char a64[65];
    std::memset(a64, 'a', 64);
    a64[64] = 0;
    ws::sha1(a64, 64, d);
    hex(d, 20, h);
    CHECK(!std::strcmp(h, "0098ba824b5c16427bd7a1122a5a442a25ec644d"));
    char b[32];
    const std::uint8_t x[] = {'f', 'o', 'o', 'b', 'a'};
    ws::base64(x, 5, b, sizeof b);
    CHECK(!std::strcmp(b, "Zm9vYmE="));
}

void test_handshake()
{
    // RFC 6455's example key and answer.
    const char *key = "dGhlIHNhbXBsZSBub25jZQ==";
    const char resp[] = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                        "sec-websocket-accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n";
    int status = 0;
    CHECK(ws::header_end(resp, sizeof resp - 1) == sizeof resp - 1);
    CHECK(ws::handshake_ok(resp, sizeof resp - 1, key, &status));
    CHECK(status == 101);
    const char bad[] = "HTTP/1.1 429 Too Many Requests\r\n\r\n";
    CHECK(!ws::handshake_ok(bad, sizeof bad - 1, key, &status));
    CHECK(status == 429);
    char req[512], k[32];
    std::uint8_t k16[16] = {};
    CHECK(ws::handshake_request(req, sizeof req, "10.0.0.2:19113", "/api/join/ws", k16, k) > 0);
    CHECK(std::strstr(req, "GET /api/join/ws HTTP/1.1\r\nHost: 10.0.0.2:19113\r\n"));
    CHECK(!std::strcmp(k, "AAAAAAAAAAAAAAAAAAAAAA=="));
}

void test_ws_frames()
{
    std::uint8_t out[300];
    const std::uint8_t mask[4] = {1, 2, 3, 4};
    const std::size_t n = ws::frame(out, sizeof out, ws::kText, "Hi", 2, mask);
    CHECK(n == 8);
    CHECK(out[0] == 0x81 && out[1] == 0x82 && out[6] == ('H' ^ 1) && out[7] == ('i' ^ 2));
    std::uint8_t big[200];
    std::memset(big, 7, sizeof big);
    CHECK(ws::frame(out, sizeof out, ws::kBinary, big, 200, mask) == 208);
    CHECK(out[1] == (0x80 | 126) && out[2] == 0 && out[3] == 200);

    // server frames: a fragmented binary message, a ping in the middle, fed byte by byte
    std::uint8_t stream[64];
    std::size_t s = 0;
    const std::uint8_t f1[] = {0x02, 3, 'a', 'b', 'c'};
    const std::uint8_t ping[] = {0x89, 2, 'p', 'q'};
    const std::uint8_t f2[] = {0x80, 2, 'd', 'e'};
    std::memcpy(stream + s, f1, sizeof f1), s += sizeof f1;
    std::memcpy(stream + s, ping, sizeof ping), s += sizeof ping;
    std::memcpy(stream + s, f2, sizeof f2), s += sizeof f2;
    std::uint8_t buf[16];
    ws::Reader r(buf, sizeof buf);
    int msgs = 0, ctls = 0;
    for (std::size_t i = 0; i < s; ++i)
    {
        std::size_t used = 0;
        const auto got = r.feed(stream + i, 1, &used);
        CHECK(used == 1);
        if (got == ws::Reader::Got::control)
        {
            ++ctls;
            CHECK(r.op() == ws::kPing && r.size() == 2 && r.data()[0] == 'p');
        }
        else if (got == ws::Reader::Got::message)
        {
            ++msgs;
            CHECK(r.op() == ws::kBinary && r.size() == 5 && !std::memcmp(r.data(), "abcde", 5));
        }
    }
    CHECK(msgs == 1 && ctls == 1);

    // two messages in one feed, the second with a 16-bit length
    std::uint8_t two[2 + 1 + 4 + 130];
    std::size_t t = 0;
    two[t++] = 0x81, two[t++] = 1, two[t++] = 'x';
    two[t++] = 0x82, two[t++] = 126, two[t++] = 0, two[t++] = 130;
    std::memset(two + t, 9, 130), t += 130;
    std::uint8_t mb[256];
    ws::Reader r2(mb, sizeof mb);
    std::size_t at = 0, used = 0;
    CHECK(r2.feed(two, t, &used) == ws::Reader::Got::message);
    CHECK(r2.op() == ws::kText && r2.size() == 1 && used == 3);
    at += used;
    CHECK(r2.feed(two + at, t - at, &used) == ws::Reader::Got::message);
    CHECK(r2.op() == ws::kBinary && r2.size() == 130 && used == t - at);

    // too big for the buffer: reported, and the next message still reads
    std::uint8_t tiny[4];
    ws::Reader r3(tiny, sizeof tiny);
    const std::uint8_t tb[] = {0x82, 6, 1, 2, 3, 4, 5, 6, 0x81, 1, 'k'};
    CHECK(r3.feed(tb, sizeof tb, &used) == ws::Reader::Got::too_big);
    CHECK(r3.feed(tb + used, sizeof tb - used, &used) == ws::Reader::Got::message);
    CHECK(r3.size() == 1 && r3.data()[0] == 'k');
    // a masked server frame is a protocol error
    const std::uint8_t masked[] = {0x81, 0x81, 0, 0, 0, 0, 'z'};
    ws::Reader r4(tiny, sizeof tiny);
    CHECK(r4.feed(masked, sizeof masked, &used) == ws::Reader::Got::bad);
}

void test_json()
{
    json::Token t[64];
    json::Doc d(t, 64);
    const char *s = R"({"a": [1, 2.5, -3e2], "b": {"c": "x\"yé", "d": true, "e": null}, "f": "z"})";
    CHECK(d.parse(s, std::strlen(s)));
    const int a = d.get(0, "a");
    CHECK(d.count(a) == 3 && d.num(d.item(a, 1)) == 2.5 && d.num(d.item(a, 2)) == -300);
    const char *path[] = {"b", "c"};
    char out[32];
    CHECK(d.str(d.path(0, path, 2), out, sizeof out) && !std::strcmp(out, "x\"y\xc3\xa9"));
    const int b = d.get(0, "b");
    CHECK(d.boolean(d.get(b, "d")) && d.is_null(d.get(b, "e")));
    CHECK(d.str_is(d.get(0, "f"), "z"));
    CHECK(d.get(0, "nope") < 0);
    CHECK(!d.parse("{\"a\":}", 6));
    CHECK(!d.parse("[1,2", 4));
    json::Token few[2];
    json::Doc small(few, 2);
    CHECK(!small.parse("[1,2,3]", 7));
    char w[64];
    json::Writer wr(w, sizeof w);
    wr.open_obj().key("k").str("a\"b").key("n").num(5LL).close_obj();
    CHECK(wr.ok() && !std::strcmp(w, R"({"k":"a\"b","n":5})"));
}

void test_pad_frame()
{
    proto::PadState s;
    s.buttons = proto::pad::kCross | proto::pad::kL2 | proto::pad::kTouchpad;
    s.lx = 0, s.ly = 255, s.l2 = 200;
    s.touches = 1;
    s.touch[0] = {7, 1919, 1079};
    std::uint8_t f[proto::kPadFrameMax];
    std::size_t n = proto::pad_frame(f, sizeof f, 0x01020304, s, true);
    // golden bytes, as join.rs parse_pad reads them
    const std::uint8_t want[] = {0x01, 0x04, 0x03, 0x02, 0x01, 0x00, 0x41, 0x10, 0x00, 0, 255, 128, 128,
                                 200,  0,    1,    7,    0x7f, 0x07, 0x37, 0x04};
    CHECK(n == sizeof want && !std::memcmp(f, want, n));
    // the apps' bits for an engine without raw_pad: cross 1, touchpad 16384 (L2 comes from the trigger)
    n = proto::pad_frame(f, sizeof f, 1, s, false);
    CHECK(f[5] == 0x01 && f[6] == 0x40 && f[7] == 0 && f[8] == 0);
    CHECK(proto::raw_to_app(proto::pad::kPs | proto::pad::kOptions) == (32768u | 4096u));
    s.has_motion = true;
    s.gyro[0] = 1.5f;
    s.accel[2] = -1.0f;
    n = proto::pad_frame(f, sizeof f, 1, s, true);
    CHECK(n == 21 + 25 && f[21] == 0x02);
    float g;
    std::memcpy(&g, f + 22, 4);
    CHECK(g == 1.5f);
    // the system's own bit never goes
    s.buttons = proto::pad::kIntercepted | proto::pad::kCross;
    proto::pad_frame(f, sizeof f, 1, s, true);
    CHECK(f[8] == 0 && f[5] == 0);
}

void test_guest_messages()
{
    char h[512];
    const proto::ClientCaps caps{"13.0.0", true, false, true, true};
    CHECK(proto::hello(h, sizeof h, "ABCDEF", nullptr, "Sam", caps) > 0);
    CHECK(!std::strcmp(h, R"({"hello":{"code":"ABCDEF","name":"Sam","client":{"kind":"ps5","app":"play","v":"13.0.0","caps":{"hevc":true,"main10":false,"raw_pad":true,"timing":true}}}})"));
    proto::hello(h, sizeof h, nullptr, nullptr, nullptr, caps);
    CHECK(!std::strncmp(h, R"({"hello":{"client":)", 19));
    proto::GuestEvent e;
    const char *j = R"({"stage":"joined","player":2,"token":"tok","hevc":true,"track":false,"audio":false,"codec_warning":null})";
    CHECK(proto::parse_guest(j, std::strlen(j), e));
    CHECK(e.type == proto::GuestEvent::joined && e.player == 2 && e.hevc && e.caps_known && !std::strcmp(e.token, "tok"));
    const char *old = R"({"stage":"joined","player":3,"token":"t","hevc":false,"track":false,"audio":false})";
    CHECK(proto::parse_guest(old, std::strlen(old), e) && !e.caps_known);
    const char *r = R"({"stage":"refused","why":"denied"})";
    CHECK(proto::parse_guest(r, std::strlen(r), e) && e.type == proto::GuestEvent::refused && !std::strcmp(e.why, "denied"));
    const char *st = R"({"status":{"player":2,"state":"playing","rtt_ms":12,"host":"Desk","block_system":true,"stream_seat":false,"plays_as":null,"seat_note":null,"move_first":false,"host_user":null,"took_host":false,"queue":0,"skipped":3,"codec_warning":null}})";
    CHECK(proto::parse_guest(st, std::strlen(st), e) && e.type == proto::GuestEvent::status && e.rtt_ms == 12 && e.block_system && !std::strcmp(e.host, "Desk"));
    const char *ru = R"({"rumble":[255,10]})";
    CHECK(proto::parse_guest(ru, std::strlen(ru), e) && e.has_rumble && e.rumble_lr[0] == 255 && e.rumble_lr[1] == 10);
    const char *li = R"({"light":[1,2,300]})";
    CHECK(proto::parse_guest(li, std::strlen(li), e) && e.has_light && e.light_rgb[2] == 255);
    const char *by = R"({"bye":"kicked"})";
    CHECK(proto::parse_guest(by, std::strlen(by), e) && e.type == proto::GuestEvent::bye && !std::strcmp(e.why, "kicked"));
    const char *wa = R"({"watch":{"hevc":true}})";
    CHECK(proto::parse_guest(wa, std::strlen(wa), e) && e.type == proto::GuestEvent::none);
    const char *po = R"({"pong":123.5,"engine_us":99})";
    CHECK(proto::parse_guest(po, std::strlen(po), e) && e.pong_t == 123.5 && e.has_engine_us && e.engine_us == 99);
}

void test_codes_and_discovery()
{
    char c[8], d[8];
    CHECK(proto::normalize_code("abc-def", c) && !std::strcmp(c, "ABCDEF"));
    CHECK(!proto::normalize_code("ABCDE0", c)); // 0 is not in the alphabet
    CHECK(!proto::normalize_code("ABCDEFG", c));
    proto::code_display("ABCDEF", d);
    CHECK(!std::strcmp(d, "ABC-DEF"));
    std::uint8_t p[proto::kProbeSize];
    proto::probe(p);
    CHECK(!std::memcmp(p, "PS5UPLOAD_PLAY_DISCOVER v1", 26) && p[511] == ' ');
    proto::Found f;
    const char *ans = R"({"name":"Desk","port":19113,"version":"13.0.0","id":"ab12"})";
    CHECK(proto::parse_found(ans, std::strlen(ans), f) && f.port == 19113 && !std::strcmp(f.name, "Desk"));
    CHECK(!proto::parse_found("{\"port\":0}", 10, f));
    CHECK(proto::version_number("13.0.0") == 13000000 && proto::version_number("12.6.44") == 12006044);
}

void test_keyframes()
{
    const std::uint8_t idr[] = {0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2, 0, 0, 1, 0x65, 9};
    const std::uint8_t p[] = {0, 0, 0, 1, 0x09, 0x10, 0, 0, 1, 0x41, 9};
    CHECK(proto::is_keyframe(idr, sizeof idr, false));
    CHECK(!proto::is_keyframe(p, sizeof p, false));
    const std::uint8_t hidr[] = {0, 0, 1, 0x40, 1, 0, 0, 1, 0x26, 1, 7}; // VPS, IDR_W_RADL (19)
    const std::uint8_t htrail[] = {0, 0, 1, 0x02, 1, 7};                // TRAIL_R (1)
    CHECK(proto::is_keyframe(hidr, sizeof hidr, true));
    CHECK(!proto::is_keyframe(htrail, sizeof htrail, true));
    proto::KeyframeAsk k;
    CHECK(k.take(5000000) && !k.take(5500000) && k.take(6000001));
}

void test_jitter()
{
    audio::Jitter j;
    j.set_depth(3, 8);
    const std::uint8_t *d;
    std::size_t l;
    std::uint8_t pkt[4] = {0};
    auto put = [&](std::uint16_t i) {
        pkt[0] = static_cast<std::uint8_t>(i);
        j.insert(i, pkt, 4);
    };
    put(65534);
    put(65535);
    CHECK(j.next(&d, &l) == audio::Jitter::Act::silence); // not primed yet
    put(0);                                                // wraps
    CHECK(j.buffered() == 3);
    CHECK(j.next(&d, &l) == audio::Jitter::Act::play && d[0] == 254);
    CHECK(j.next(&d, &l) == audio::Jitter::Act::play && d[0] == 255);
    // 1 is lost, 2 and 3 arrive: 1 comes from 2's redundancy
    put(2);
    put(3);
    CHECK(j.next(&d, &l) == audio::Jitter::Act::play && d[0] == 0);
    CHECK(j.next(&d, &l) == audio::Jitter::Act::fec && d[0] == 2);
    CHECK(j.next(&d, &l) == audio::Jitter::Act::play && d[0] == 2);
    // a late copy of 1 is ignored
    put(1);
    CHECK(j.stats().late == 1);
    CHECK(j.next(&d, &l) == audio::Jitter::Act::play && d[0] == 3);
    // ran dry: silence until 3 frames are held again
    CHECK(j.next(&d, &l) == audio::Jitter::Act::silence && j.stats().underruns == 1);
    put(4);
    CHECK(j.next(&d, &l) == audio::Jitter::Act::silence);
    put(6); // the span 4-6 is held (5 may still come)
    put(5);
    CHECK(j.next(&d, &l) == audio::Jitter::Act::play && d[0] == 4);
    // a frame missing with nothing after it to recover from: concealed
    put(8);
    put(9);
    put(10);
    CHECK(j.next(&d, &l) == audio::Jitter::Act::play && d[0] == 5);
    CHECK(j.next(&d, &l) == audio::Jitter::Act::play && d[0] == 6);
    CHECK(j.next(&d, &l) == audio::Jitter::Act::fec && d[0] == 8); // 7 from 8
    // too much held: the oldest go
    for (std::uint16_t i = 11; i < 30; ++i)
        put(i);
    CHECK(j.next(&d, &l) == audio::Jitter::Act::play && d[0] == 22 && j.stats().dropped > 0);

    audio::PcmRing r;
    std::int16_t in[480 * 2], out[256 * 2];
    for (int i = 0; i < 480 * 2; ++i)
        in[i] = static_cast<std::int16_t>(i);
    r.push(in, 480);
    CHECK(r.pop(out, 256) && out[0] == 0 && out[511] == 511);
    CHECK(!r.pop(out, 256) && r.frames() == 224);
    r.push(in, 480);
    CHECK(r.pop(out, 256) && out[0] == 512 && out[2 * 224] == 0);
}

void test_chords()
{
    using proto::pad::kCross;
    Chords c;
    const std::uint32_t base = Chords::kBase;
    auto o = c.update(kCross, 0);
    CHECK(o.buttons == kCross && !o.ps_sent);
    o = c.update(base | kCross, 1000);
    CHECK(o.buttons == kCross && o.ps_progress == 0); // the chord's own buttons are kept back
    o = c.update(base, 600000);
    CHECK(o.ps_progress > 0.5f && !o.ps_sent);
    o = c.update(base, 1001000);
    CHECK(o.ps_sent && (o.buttons & proto::pad::kPs));
    o = c.update(base, 1100000);
    CHECK(!o.ps_sent && (o.buttons & proto::pad::kPs)); // still pressed, once
    o = c.update(base, 1300000);
    CHECK(!(o.buttons & proto::pad::kPs));
    // let go of the touchpad first: Options alone stays kept back until it is up too
    o = c.update(proto::pad::kOptions, 1400000);
    CHECK(o.buttons == 0);
    o = c.update(0, 1500000);
    o = c.update(proto::pad::kOptions, 1600000);
    CHECK(o.buttons == proto::pad::kOptions); // a plain Options press goes through
    c.update(0, 1700000);
    // leave: all four for 1 s, no PS on the way
    const std::uint32_t all = Chords::kLeave;
    o = c.update(all, 2000000);
    CHECK(o.leave_progress == 0 && o.buttons == (all & ~base));
    o = c.update(all, 3100000);
    CHECK(o.leave && !o.ps_sent && !(o.buttons & proto::pad::kPs));
    o = c.update(all, 3200000);
    CHECK(!o.leave);
}

void test_settings()
{
    Settings s;
    s.music = false;
    s.music_volume = 40;
    Host *h = s.remember("192.168.1.5", 19113, "Desk|top");
    CHECK(h && !std::strcmp(h->name, "")); // a '|' never goes into the file
    h = s.remember("192.168.1.5", 19113, "Desk");
    std::strcpy(h->token, "abc123");
    s.remember("10.0.0.9", 9000, "Laptop");
    char text[2048];
    s.write(text, sizeof text);
    Settings t;
    t.parse(text, std::strlen(text));
    CHECK(!t.music && t.music_volume == 40);
    const Host *list[8];
    CHECK(t.recent(list, 8) == 2);
    CHECK(!std::strcmp(list[0]->addr, "10.0.0.9") && list[0]->port == 9000 && !std::strcmp(list[0]->name, "Laptop"));
    CHECK(!std::strcmp(list[1]->token, "abc123") && !std::strcmp(list[1]->name, "Desk"));
    for (int i = 0; i < 12; ++i)
    {
        char a[32];
        std::snprintf(a, sizeof a, "10.1.1.%d", i);
        t.remember(a, 19113, "");
    }
    CHECK(t.recent(list, 8) == 8 && !std::strcmp(list[0]->addr, "10.1.1.11"));
    char addr[64];
    std::uint16_t port;
    CHECK(split_address("192.168.86.20:19200", addr, sizeof addr, &port, 19113) && port == 19200 &&
          !std::strcmp(addr, "192.168.86.20"));
    CHECK(split_address("192.168.86.20", addr, sizeof addr, &port, 19113) && port == 19113);
    CHECK(!split_address("1.2.3.4:99999", addr, sizeof addr, &port, 19113));
    CHECK(!split_address("bad/addr", addr, sizeof addr, &port, 19113));
}
// A language file as tools/i18n.py writes it: two strings (the second left English), one run.
void test_i18n()
{
    using namespace play::ui;
    std::uint8_t f[256] = {};
    std::size_t o = 0;
    auto u32 = [&](std::uint32_t v) {
        for (int k = 0; k < 4; ++k)
            f[o++] = static_cast<std::uint8_t>(v >> (8 * k));
    };
    auto u16 = [&](std::uint16_t v) {
        f[o++] = static_cast<std::uint8_t>(v);
        f[o++] = static_cast<std::uint8_t>(v >> 8);
    };
    const char text[] = "Hallo \xee\x80\x80" "0000!";
    std::memcpy(f, "PPL1", 4), o = 4;
    u32(2), u32(1), u32(1), u32(3);
    u32(24), u32(32), u32(44);
    u32(0), u32(sizeof text - 1); // string 0
    u32(0), u32(0);               // string 1: English
    for (int size = 0; size < 3; ++size)
        u16(2), u16(1), u16(1), u16(0), u32(0), u32(1); // a 2x1 run, one byte of pixels
    std::memcpy(f + o, text, sizeof text - 1), o += sizeof text - 1;
    f[o++] = 0xF1; // level 15, 2 pixels
    CHECK(i18n::load(f, o));
    CHECK(!std::strcmp(tr(S::app_name), text) && i18n::right_to_left());
    CHECK(!std::strcmp(tr(S::tagline), "Play together with friends, from the couch."));
    int table = -1, id = -1;
    const char *t = tr(S::app_name);
    CHECK(i18n::run_ref(t + 6, std::strlen(t + 6), &table, &id) && table == 0 && id == 0);
    i18n::Run r;
    CHECK(i18n::run(0, 0, 1, r) && r.w == 2 && r.h == 1 && r.len == 1 && r.rle[0] == 0xF1);
    CHECK(!i18n::run(0, 1, 1, r) && !i18n::run(0, 0, 3, r));
    CHECK(!i18n::load(f, 10)); // cut short: English
    CHECK(!std::strcmp(tr(S::app_name), "PS5Upload Play"));
    i18n::use_english();
    CHECK(i18n::find("pt-BR") > 0 && i18n::find("xx") < 0 && i18n::count() == 21);
    CHECK(!std::strcmp(i18n::code(i18n::from_system_id(0)), "ja") &&
          !std::strcmp(i18n::code(i18n::from_system_id(17)), "pt-BR") &&
          !std::strcmp(i18n::code(i18n::from_system_id(1)), "en") &&
          !std::strcmp(i18n::code(i18n::from_system_id(99)), "en"));
}
} // namespace

void test_hdr()
{
    static hdr::Tables t;
    hdr::build(t);
    // PQ code 520 is 100 cd/m2 (ST 2084): linear 100/10000 of full scale, within 2%
    const double want = 100.0 / 10000.0 * (1u << hdr::kLinBits);
    CHECK(t.lin[520] > want * 0.98 && t.lin[519] < want * 1.02);
    CHECK(t.lin[0] == 0 && t.lin[1023] == (1u << hdr::kLinBits));
    const hdr::Chroma grey = hdr::chroma(512, 512);
    auto rgb = [](std::uint32_t p, int i) { return static_cast<int>(p >> (8 * i) & 0xff); };
    // black stays black, greys stay grey, brighter is never darker, the top is white
    CHECK(hdr::pixel(t, 64, grey) == 0);
    int last = -1;
    bool neutral = true, rising = true;
    for (int y = 64; y <= 940; ++y)
    {
        const std::uint32_t p = hdr::pixel(t, y, grey);
        neutral = neutral && rgb(p, 0) == rgb(p, 1) && rgb(p, 1) == rgb(p, 2);
        rising = rising && rgb(p, 1) >= last;
        last = rgb(p, 1);
    }
    CHECK(neutral);
    CHECK(rising);
    CHECK(last == 255);
    // 100 cd/m2 (Y' 509) is below the roll-off: (100/203)^(1/2.4) of 255, about 190
    const int g100 = rgb(hdr::pixel(t, 509, grey), 1);
    CHECK(g100 >= 187 && g100 <= 193);
    // 1000 cd/m2 (the assumed peak, PQ 0.7518: Y' 64 + 769 * 876/1023 = 722) is white, 203 not quite
    CHECK(rgb(hdr::pixel(t, 722, grey), 1) >= 253);
    const int g203 = rgb(hdr::pixel(t, 64 + 593 * 876 / 1023, grey), 1);
    CHECK(g203 > 225 && g203 < 245);
    // a saturated BT.2020 red ends up red (out of BT.709's gamut, clipped, not tinted)
    const std::uint32_t red = hdr::pixel(t, 400, hdr::chroma(400, 960));
    CHECK(rgb(red, 0) > 200 && rgb(red, 1) < 40 && rgb(red, 2) < 40);
    // a row of 16-bit samples, chroma shared by a pixel pair: the decoder's low-aligned 10 bits
    // (shift 0) and P010's top-aligned (shift 6), each told apart by shift_of
    for (int shift = 0; shift <= 6; shift += 6)
    {
        std::uint8_t y[16 * 8 * 2] = {}, uv[8];
        const int ys[4] = {64, 509, 940, 509};
        for (int i = 0; i < 4; ++i)
        {
            const int v = ys[i] << shift, c = 512 << shift;
            for (int r = 0; r < 8; ++r) // every row the same, so shift_of reads real samples
            {
                y[r * 16 + 2 * i] = static_cast<std::uint8_t>(v & 0xff);
                y[r * 16 + 2 * i + 1] = static_cast<std::uint8_t>(v >> 8);
            }
            uv[2 * i] = static_cast<std::uint8_t>(c & 0xff);
            uv[2 * i + 1] = static_cast<std::uint8_t>(c >> 8);
        }
        CHECK(hdr::shift_of(y, 16, 4, 8) == shift);
        std::uint32_t out[4];
        hdr::row(t, y, uv, 4, shift, out);
        CHECK(out[0] == 0xff000000u && rgb(out[1], 0) == g100 && out[2] == 0xffffffffu && out[3] == out[1]);
    }
}

int main()
{
    test_sha1_base64();
    test_handshake();
    test_ws_frames();
    test_json();
    test_pad_frame();
    test_guest_messages();
    test_codes_and_discovery();
    test_keyframes();
    test_jitter();
    test_chords();
    test_settings();
    test_i18n();
    test_hdr();
    std::printf("%d checks, %d failed\n", checks, failures);
    return failures ? 1 : 0;
}
