// PS5Upload Play - host unit tests for Play Rooms (docs/ROOMS.md): the lists, sorting and
// filtering, previews, the room push, names, chat, voice framing and gating, push-to-talk on
// Create, the notice gate, engine-id pairings, the on-screen keyboard, many controllers and the
// sealed knock. make -C apps/play test.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "../src/core/chat.hpp"
#include "../src/core/chord.hpp"
#include "../src/core/invite.hpp"
#include "../src/core/notice.hpp"
#include "../src/core/proto.hpp"
#include "../src/core/rooms.hpp"
#include "../src/core/seats.hpp"
#include "../src/core/settings.hpp"
#include "../src/core/textkb.hpp"
#include "../src/core/voice.hpp"
#include "../src/net/seal.hpp"

#include <bearssl.h>

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

const char *kList = R"({"you":{"country":"DE"},"proto":{"min":2,"latest":2},"app":{"latest":"13.0.1"},
 "rooms":[
  {"room":"r1","name":"Couch chaos","host":"Sam","tag":"4F2A","game":{"title":"It Takes Two","id":"PPSA01234"},
   "proto":2,"ver":"13.0.0","caps":["web","ps5app","preview","chat","voice"],"slots":[1,2],"pw":true,
   "lang":"en","region":"eu-west","est_ms":70,"age_s":300,"pk":"AAAA"},
  {"room":"r2","name":"Sunday racers","host":"Kai","tag":"9C11","game":{"title":"Gran Turismo 7","id":"PPSA01476"},
   "proto":2,"ver":"13.0.0","caps":["ps5app"],"slots":[4,4],"pw":false,"lang":"de","region":"eu-west","est_ms":30,"age_s":60},
  {"room":"r3","name":"Old host","host":"Lee","tag":"0001","game":{"title":"ASTRO BOT","id":"PPSA02345"},
   "proto":5,"ver":"16.0.0","caps":["ps5app"],"slots":[1,4],"pw":false,"lang":"en","region":"na-east","est_ms":120,"age_s":10},
  {"room":"r4","name":"Web only","host":"Mo","tag":"7777","game":{"title":"Astro Bot","id":"PPSA02345"},
   "proto":2,"ver":"13.0.0","caps":["web"],"slots":[2,4],"pw":false,"lang":"en","est_ms":20,"age_s":5}
 ],"next":1})";

void test_list_sort_filter()
{
    rooms::List l;
    CHECK(rooms::parse_list(kList, std::strlen(kList), l));
    CHECK(l.n == 4 && l.next == 1 && l.proto_min == 2 && !std::strcmp(l.app_latest, "13.0.1") &&
          !std::strcmp(l.country, "DE"));
    const rooms::Card &a = l.cards[0];
    CHECK(!std::strcmp(a.room, "r1") && !std::strcmp(a.name, "Couch chaos") && !std::strcmp(a.tag, "4F2A"));
    CHECK(!std::strcmp(a.game_id, "PPSA01234") && a.in == 1 && a.max == 2 && a.pw && a.cap_ps5app && a.cap_voice);
    CHECK(a.est_ms == 70 && !std::strcmp(a.region, "eu-west"));
    // a second page appends, a repeat is not added twice
    CHECK(rooms::parse_list(kList, std::strlen(kList), l) && l.n == 4);
    // compatible: the protocol window and PS5 app guests welcome
    CHECK(rooms::compatible(l.cards[0]) && !rooms::compatible(l.cards[2]) && !rooms::compatible(l.cards[3]));
    rooms::Filter f; // Works with my version on by default
    CHECK(rooms::passes(l.cards[0], f) && !rooms::passes(l.cards[2], f) && !rooms::passes(l.cards[3], f));
    f.space = true;
    CHECK(!rooms::passes(l.cards[1], f));
    f = rooms::Filter{};
    f.nopw = true;
    CHECK(!rooms::passes(l.cards[0], f) && rooms::passes(l.cards[1], f));
    f = rooms::Filter{};
    std::snprintf(f.game, sizeof f.game, "%s", "astro");
    f.myver = false;
    CHECK(rooms::passes(l.cards[2], f) && rooms::passes(l.cards[3], f) && !rooms::passes(l.cards[0], f));
    std::snprintf(f.game, sizeof f.game, "%s", "PPSA01476");
    CHECK(rooms::passes(l.cards[1], f) && !rooms::passes(l.cards[0], f));
    f = rooms::Filter{};
    f.near = true;
    CHECK(rooms::passes(l.cards[1], f) && rooms::passes(l.cards[0], f) && !rooms::passes(l.cards[2], f));
    std::snprintf(f.lang, sizeof f.lang, "%s", "de");
    CHECK(rooms::passes(l.cards[1], f) && !rooms::passes(l.cards[0], f));
    CHECK(f.any() && !rooms::Filter{}.any());

    // Best for you: has space first, then the round trip
    rooms::Card c[4] = {l.cards[0], l.cards[1], l.cards[2], l.cards[3]};
    rooms::sort(c, 4, rooms::Sort::best);
    CHECK(!std::strcmp(c[0].room, "r4") && !std::strcmp(c[1].room, "r1") && !std::strcmp(c[3].room, "r2"));
    // measured beats the estimate
    c[1].measured_ms = 10;
    rooms::sort(c, 4, rooms::Sort::latency);
    CHECK(!std::strcmp(c[0].room, "r1"));
    rooms::sort(c, 4, rooms::Sort::game);
    CHECK(!std::strcmp(c[0].room, "r2") || !std::strcmp(c[0].room, "r3") || !std::strcmp(c[0].room, "r4"));
    rooms::sort(c, 4, rooms::Sort::players);
    CHECK(!std::strcmp(c[0].room, "r2"));
    rooms::sort(c, 4, rooms::Sort::newest);
    CHECK(!std::strcmp(c[0].room, "r4") && !std::strcmp(c[3].room, "r1"));
    // only the public cards move: a friend's room stays on top
    rooms::Card g[3] = {l.cards[0], l.cards[1], l.cards[3]};
    g[0].group = rooms::Group::friends;
    rooms::sort(g, 3, rooms::Sort::best);
    CHECK(!std::strcmp(g[0].room, "r1") && !std::strcmp(g[1].room, "r4"));
    bool m = false;
    CHECK(rooms::expected_ms(c[0], &m) == 20 && !m);
    rooms::Card h = l.cards[0];
    h.history_ms = 34;
    CHECK(rooms::expected_ms(h, &m) == 34 && !m);
    h.measured_ms = 28;
    CHECK(rooms::expected_ms(h, &m) == 28 && m);
    CHECK(rooms::bars_for_ms(15) == 4 && rooms::bars_for_ms(40) == 3 && rooms::bars_for_ms(70) == 2 &&
          rooms::bars_for_ms(200) == 1 && rooms::bars_for_ms(-1) == 0);
    CHECK(rooms::bars_of("excellent") == 4 && rooms::bars_of("poor") == 1 && rooms::bars_of("down") == 0);

    char q[256];
    rooms::Filter qf;
    qf.space = true;
    std::snprintf(qf.game, sizeof qf.game, "%s", "It Takes");
    std::snprintf(qf.lang, sizeof qf.lang, "%s", "en");
    CHECK(rooms::list_query(q, sizeof q, rooms::Sort::latency, qf, 2) > 0);
    CHECK(!std::strcmp(q, "/rooms?sort=latency&game=It%20Takes&free=1&proto=2&lang=en&page=2"));
    CHECK(rooms::list_query(q, sizeof q, rooms::Sort::best, rooms::Filter{}, 0) &&
          !std::strcmp(q, "/rooms?sort=best&proto=2"));
    CHECK(rooms::sort_from("game") == rooms::Sort::game && rooms::sort_from("x") == rooms::Sort::best);
}

void test_status_preview_push()
{
    rooms::Card f[3];
    std::snprintf(f[0].room, sizeof f[0].room, "%s", "lobbyA");
    std::snprintf(f[1].room, sizeof f[1].room, "%s", "lobbyB");
    std::snprintf(f[2].room, sizeof f[2].room, "%s", "lobbyC");
    const char *st = R"({"every_s":60,"rooms":{"lobbyA":{"listed":false,"slots":[2,4],"game":{"title":"ASTRO BOT","id":"PPSA02345"},"pw":false,"locked":false,"proto":2},
                         "lobbyC":{"listed":true,"slots":[1,3],"pw":true,"locked":true,"pk":"KEY"}}})";
    int every = 0;
    const int n = rooms::parse_status(st, std::strlen(st), f, 3, &every);
    CHECK(every == 60 && !std::strcmp(f[0].vis, "friends"));
    CHECK(n == 2 && !std::strcmp(f[0].room, "lobbyA") && f[0].in == 2 && !std::strcmp(f[0].game_title, "ASTRO BOT"));
    CHECK(!std::strcmp(f[1].room, "lobbyC") && f[1].locked && f[1].pw && !std::strcmp(f[1].pk, "KEY"));
    const char *ids[] = {"lobbyA", "bad id!", "lobbyC"};
    char q[128];
    CHECK(rooms::status_query(q, sizeof q, ids, 3) && !std::strcmp(q, "/rooms/status?r=lobbyA,lobbyC"));

    const char *pv = R"({"proto": 2, "caps": ["chat","ready","seat_move","group","preview"],
 "host": {"name": "Sam", "tag": "4F2A", "ver": "13.0.0", "playing_for_min": 42},
 "room": {"name": "Couch chaos", "visibility": "public", "size": 4, "pw": true,
          "approval": "ask", "chat": "quick", "lang": "en", "region": "eu-west", "watchers": 1},
 "game": {"id": "PPSA01234", "title": "It Takes Two", "players": "1-2", "genre": null},
 "stream": {"res": "1080p", "fps": 60, "codec": "hevc", "kbps": 15000, "hdr": false},
 "host_link": {"rtt_ms": 2, "tier": "excellent"},
 "players": [{"name": "Sam", "seat": 1, "kind": "host"},
             {"name": "Mia", "seat": 2, "kind": "ps5", "tier": "good", "rtt_ms": 31},
             {"name": "Lee", "seat": 3, "kind": "web", "tier": "playable", "rtt_ms": 74}],
 "asking": 1})";
    rooms::Preview p;
    CHECK(rooms::parse_preview(pv, std::strlen(pv), p));
    CHECK(p.ok && p.proto == 2 && p.cap_chat && !p.cap_voice && p.playing_min == 42 && p.size == 4 && p.pw);
    CHECK(!std::strcmp(p.res, "1080p") && p.fps == 60 && p.kbps == 15000 && p.link_rtt == 2);
    CHECK(p.n_players == 3 && p.players[2].rtt_ms == 74 && !std::strcmp(p.players[1].tier, "good"));
    CHECK(!std::strcmp(p.game_players, "1-2") && p.asking == 1);
    // the hub's relayed copy
    char relayed[2400];
    std::snprintf(relayed, sizeof relayed, "{\"data\":%s}", pv);
    rooms::Preview p2;
    CHECK(rooms::parse_preview(relayed, std::strlen(relayed), p2) && p2.n_players == 3);
    CHECK(!rooms::parse_preview("{}", 2, p2));

    rooms::RoomState rs;
    const char *push = R"({"room":{"name":"Couch chaos","size":4,"players":[{"id":1,"name":"Sam","color":[0,0,255]},
        {"id":2,"name":"Mia","you":true}],"you":{"rtt_ms":31,"tier":"good"},"host_link":{"rtt_ms":2,"tier":"excellent"},
        "chat":"quick","voice":"ptt","muted":{"voice":false,"chat":true}}})";
    CHECK(rooms::parse_room_push(push, std::strlen(push), rs));
    CHECK(rs.known && rs.n_players == 2 && rs.players[1].you && rs.players[0].color == 0x0000ff);
    CHECK(rs.you_rtt == 31 && !std::strcmp(rs.link_tier, "excellent") && !std::strcmp(rs.chat, "quick"));
    CHECK(rs.muted_chat && !rs.muted_voice && !std::strcmp(rs.voice, "ptt"));
    const char *vp = R"({"voice":{"speaking":[1],"paused":null,"mode":"open"}})";
    CHECK(rooms::parse_room_push(vp, std::strlen(vp), rs) && rs.players[0].speaking && !rs.players[1].speaking);
    CHECK(!std::strcmp(rs.voice, "open"));
    // a new room message keeps who is speaking
    CHECK(rooms::parse_room_push(push, std::strlen(push), rs) && rs.players[0].speaking);
    const char *mu = R"({"muted":{"voice":true,"chat":false}})";
    CHECK(rooms::parse_room_push(mu, std::strlen(mu), rs) && rs.muted_voice && !rs.muted_chat);
    const char *pz = R"({"voice":{"paused":"poor"}})";
    CHECK(rooms::parse_room_push(pz, std::strlen(pz), rs) && !std::strcmp(rs.paused, "poor"));
    CHECK(!rooms::parse_room_push("{\"status\":{}}", 13, rs));
}

void test_names()
{
    char out[96];
    CHECK(rooms::clean_name("  Sam   the  Great ", out, sizeof out) && !std::strcmp(out, "Sam the Great"));
    CHECK(!rooms::clean_name("visit http://x.io", out, sizeof out));
    CHECK(!rooms::clean_name("me@mail", out, sizeof out));
    CHECK(!rooms::clean_name("\xe2\x80\x8b", out, sizeof out)); // zero-width only
    CHECK(rooms::clean_name("Sa\xe2\x80\x8bm", out, sizeof out) && !std::strcmp(out, "Sam"));
    CHECK(rooms::clean_name("Ѕаm", out, sizeof out)); // Cyrillic letters are letters
    CHECK(rooms::clean_name("abcdefghijklmnopqrstuvwxyz0123", out, sizeof out) && std::strlen(out) == 24);
    CHECK(rooms::password_ok("abcd") && !rooms::password_ok("abc") && !rooms::password_ok("ab\ncd"));
}

void test_chat()
{
    char out[1024];
    CHECK(chat::clean_text("  hi   there ", out, sizeof out, false) && !std::strcmp(out, "hi there"));
    CHECK(chat::clean_text("join ABC-DEF now", out, sizeof out, true) && !std::strcmp(out, "join now"));
    CHECK(chat::clean_text("join abc2ef now", out, sizeof out, true) && !std::strcmp(out, "join now"));
    CHECK(chat::clean_text("HELLOO there", out, sizeof out, true) && !std::strcmp(out, "HELLOO there"));
    CHECK(chat::clean_text("join ABC-DEF now", out, sizeof out, false) && !std::strcmp(out, "join ABC-DEF now"));
    char longer[400];
    std::memset(longer, 'a', 300);
    longer[300] = 0;
    CHECK(chat::clean_text(longer, out, sizeof out, false) == static_cast<std::size_t>(chat::kTextMax));
    CHECK(chat::clean_text("a\x01" "b\xe2\x80\xae" "c", out, sizeof out, false) && !std::strcmp(out, "abc"));
    CHECK(!chat::clean_text("   ", out, sizeof out, false));
    CHECK(chat::looks_like_link("www.x.com", 9) && chat::looks_like_link("bit.ly/abc", 10) &&
          chat::looks_like_link("https://a", 9) && !chat::looks_like_link("ok.", 3) &&
          !chat::looks_like_link("3.5", 3));
    chat::for_display("see example.com now", out, sizeof out, false);
    CHECK(!std::strcmp(out, "see example\xc2\xb7"
                            "com now"));
    chat::for_display("oh shit", out, sizeof out, true);
    CHECK(!std::strcmp(out, "oh ****"));
    CHECK(chat::quick_message(out, sizeof out, 5) && !std::strcmp(out, "{\"chat\":{\"quick\":5}}"));
    CHECK(!chat::quick_message(out, sizeof out, 17));
    CHECK(chat::text_message(out, sizeof out, "say \"hi\"") && !std::strcmp(out, "{\"chat\":{\"text\":\"say \\\"hi\\\"\"}}"));
    chat::Incoming in;
    const char *a = R"({"chat":{"from":"Mia","quick":3}})";
    CHECK(chat::parse(a, std::strlen(a), in) && in.quick == 3 && !std::strcmp(in.from, "Mia"));
    const char *b = R"({"chat":{"from":"Lee","text":"lag \u0007 here"}})";
    CHECK(chat::parse(b, std::strlen(b), in) && !std::strcmp(in.text, "lag here"));
    const char *c = R"({"chat":{"error":"slow"}})";
    CHECK(chat::parse(c, std::strlen(c), in) && !std::strcmp(in.error, "slow"));
    const char *d = R"({"chat":{"from":"X","quick":99}})";
    CHECK(!chat::parse(d, std::strlen(d), in));

    chat::Rate r;
    CHECK(r.take(1000000) && !r.take(2000000) && r.take(3100000));
    std::uint64_t t = 3100000;
    int ok = 0;
    for (int i = 0; i < 20; ++i)
        t += 2100000, ok += r.take(t) ? 1 : 0;
    CHECK(ok < 20 && ok >= 8); // 10 a minute at most

    chat::Log log;
    log.add("Mia", 2, "", 1000000, false);
    log.add("Mia", 2, "", 2000000, false);
    log.add("Lee", 0, "hi", 3000000, false);
    log.add("Me", 1, "", 3500000, true);
    CHECK(log.size() == 3 && log.at(0).times == 2 && log.unread() == 3);
    const chat::Line *rec[3];
    CHECK(log.recent(rec, 3, 8500000) == 2 && !std::strcmp(rec[1]->from, "Me"));
    log.read_all();
    CHECK(log.unread() == 0);
    for (int i = 0; i < 60; ++i)
        log.add("x", 0, i % 2 ? "a" : "b", 10000000 + static_cast<std::uint64_t>(i), false);
    CHECK(log.size() == chat::Log::kLines);
}

void test_voice()
{
    std::uint8_t opus[40];
    for (int i = 0; i < 40; ++i)
        opus[i] = static_cast<std::uint8_t>(i);
    std::uint8_t f[64];
    CHECK(voice::up_frame(f, sizeof f, 0x1234, 200, opus, 40) == 44);
    CHECK(f[0] == 6 && f[1] == 0x12 && f[2] == 0x34 && f[3] == 200 && f[4] == 0 && f[43] == 39);
    CHECK(!voice::up_frame(f, sizeof f, 1, 1, opus, 0));
    std::uint8_t big[300];
    CHECK(!voice::up_frame(big, sizeof big, 1, 1, big, 256));

    std::uint8_t b[] = {9, 2, 1, 0, 7, 3, 0xa, 0xb, 0xc, 3, 0x12, 0x34, 1, 0xd};
    voice::Item it[3];
    CHECK(voice::parse_down(b, sizeof b, it, 3) == 2);
    CHECK(it[0].speaker == 1 && it[0].seq == 7 && it[0].len == 3 && it[0].opus[2] == 0xc);
    CHECK(it[1].speaker == 3 && it[1].seq == 0x1234 && it[1].len == 1 && it[1].opus[0] == 0xd);
    b[1] = 3; // says 3, holds 2
    CHECK(voice::parse_down(b, sizeof b, it, 3) == -1);
    b[1] = 2;
    CHECK(voice::parse_down(b, sizeof b - 1, it, 3) == -1);
    b[0] = 7; // the timed video kind is not voice
    CHECK(voice::parse_down(b, sizeof b, it, 3) == -1);
    char m[64];
    CHECK(voice::mic_message(m, sizeof m, "ptt_down") && !std::strcmp(m, "{\"mic\":\"ptt_down\"}"));

    std::int16_t pcm[320] = {};
    CHECK(voice::level_of(pcm, 320) == 0);
    for (int i = 0; i < 320; ++i)
        pcm[i] = static_cast<std::int16_t>(i % 2 ? 16000 : -16000); // about -6 dBFS
    const int loud = voice::level_of(pcm, 320);
    CHECK(loud > 210 && loud < 240);
    for (int i = 0; i < 320; ++i)
        pcm[i] = static_cast<std::int16_t>(i % 2 ? 30 : -30); // about -61 dBFS
    CHECK(voice::level_of(pcm, 320) < 10);

    voice::Gate g;
    std::uint64_t now = 1000000;
    for (int i = 0; i < 50; ++i, now += 20000)
        CHECK(!g.feed(20, now)); // a quiet room never opens it
    CHECK(g.feed(220, now)); // a word
    now += 20000;
    CHECK(g.feed(20, now)); // the hangover keeps it open
    now += 300000;
    CHECK(!g.feed(20, now)); // and then it closes

    voice::Allow a;
    CHECK(!std::strcmp(voice::why_not(a), "off")); // the mic is off on every join
    a.mic_on = true;
    a.room = "ptt";
    a.silent = 0;
    CHECK(!std::strcmp(voice::why_not(a), "hold") && !voice::may_send(a));
    a.talk_held = true;
    CHECK(voice::may_send(a));
    a.in_front = false;
    CHECK(!std::strcmp(voice::why_not(a), "behind"));
    a.in_front = true;
    a.silent = 4;
    CHECK(!std::strcmp(voice::why_not(a), "mic_muted"));
    a.silent = 5;
    CHECK(!std::strcmp(voice::why_not(a), "no_mic"));
    a.silent = 0;
    a.host_muted = true;
    CHECK(!std::strcmp(voice::why_not(a), "muted"));
    a.host_muted = false;
    a.room = "off";
    CHECK(!std::strcmp(voice::why_not(a), "room_off"));
    a.room = "ptt";
    a.talk_held = false;
    a.mode = voice::Mode::open; // open mic only where the room allows it
    CHECK(!voice::may_send(a));
    a.room = "open";
    CHECK(voice::may_send(a));
    a.paused = true;
    CHECK(!std::strcmp(voice::why_not(a), "paused"));

    voice::Ptt p;
    CHECK(!p.update(false) && !std::strcmp(p.update(true), "ptt_down") && !p.update(true) &&
          !std::strcmp(p.update(false), "ptt_up"));

    voice::Mixer mx;
    CHECK(mx.gain_q8(3) == 256 && mx.volume(3) == 100);
    mx.set_volume(3, 150);
    mx.set_muted(4, true);
    CHECK(mx.gain_q8(3) == 384 && mx.gain_q8(4) == 0 && mx.muted(4) && !mx.muted(3));
    mx.set_volume(3, 999);
    CHECK(mx.volume(3) == 200);

    voice::Duck dk;
    int gq = 0;
    for (int i = 0; i < 40; ++i)
        gq = dk.step(true, 256); // about 200 ms
    CHECK(gq < 17500 && gq > 15500); // -6 dB
    for (int i = 0; i < 400; ++i)
        gq = dk.step(false, 256);
    CHECK(gq > 32000);
    dk.set_level(0);
    CHECK(dk.step(true, 256) > 32000);
}

void test_ptt_chord()
{
    namespace pb = proto::pad;
    Chords c;
    auto o = c.update(pb::kCreate | pb::kCross, 1000);
    CHECK((o.buttons & pb::kCreate) && !o.talk); // a plain room: Create is the game's
    c.set_ptt_on_create(true);
    o = c.update(pb::kCreate | pb::kCross, 2000);
    CHECK(!(o.buttons & pb::kCreate) && (o.buttons & pb::kCross) && o.talk);
    o = c.update(pb::kCross, 3000);
    CHECK(!o.talk && !(o.buttons & pb::kCreate));
    // the PS chord still works with push-to-talk on
    o = c.update(Chords::kBase | pb::kCreate, 4000);
    o = c.update(Chords::kBase | pb::kCreate, 4000 + Chords::kHoldUs);
    CHECK(o.ps_sent && (o.buttons & pb::kPs) && !(o.buttons & pb::kCreate) && o.talk);
}

void test_notice()
{
    CHECK(notice::needed(notice::Need::chat, 0) && notice::needed(notice::Need::public_room, 0));
    CHECK(!notice::needed(notice::Need::private_play, 0));
    CHECK(!notice::needed(notice::Need::voice, notice::kVersion));
    CHECK(notice::needed(notice::Need::voice, notice::kVersion - 1));
}

void test_engine_ids()
{
    Settings s;
    Host *a = s.remember_id("engA", "192.168.1.5", 19113, "Desk");
    std::strcpy(a->token, "tokA");
    // the computer moved: the same row, the token kept (no surprise re-pair)
    Host *b = s.remember_id("engA", "192.168.1.9", 19113, "Desk");
    CHECK(a == b && !std::strcmp(b->addr, "192.168.1.9") && !std::strcmp(b->token, "tokA"));
    // two engines over Tailscale share one forward: never each other's token
    Host *t1 = s.remember_id("engT1", "127.0.0.1", 9182, "Laptop");
    std::strcpy(t1->token, "tokT1");
    Host *t2 = s.remember_id("engT2", "127.0.0.1", 9182, "Docker");
    CHECK(t1 != t2 && !t2->token[0] && !std::strcmp(t1->token, "tokT1"));
    CHECK(s.find_id("engT1") == t1 && s.find_id("engT2") == t2 && !s.find_id("nope"));
    // a row from before ids becomes the engine's once it answers there
    Host *old = s.remember("10.0.0.3", 19113, "Old");
    std::strcpy(old->token, "tokOld");
    Host *now = s.remember_id("engOld", "10.0.0.3", 19113, nullptr);
    CHECK(now == old && !std::strcmp(now->token, "tokOld") && !std::strcmp(now->id, "engOld"));
    s.notice = notice::kVersion;
    s.duck = 2;
    s.f_nopw = true;
    std::snprintf(s.f_lang, sizeof s.f_lang, "%s", "pt-BR");
    std::snprintf(s.sort, sizeof s.sort, "%s", "latency");
    char text[4096];
    s.write(text, sizeof text);
    Settings r;
    r.parse(text, std::strlen(text));
    CHECK(r.find_id("engA") && !std::strcmp(r.find_id("engA")->token, "tokA"));
    CHECK(r.find_id("engT2") && r.find_id("engT1") && r.find_id("engT1") != r.find_id("engT2"));
    CHECK(!std::strcmp(r.find_id("engT1")->token, "tokT1"));
    CHECK(r.notice == notice::kVersion && r.duck == 2 && r.f_nopw && r.f_myver && !std::strcmp(r.f_lang, "pt-BR"));
    CHECK(!std::strcmp(r.sort, "latency"));
}

void test_hello_rooms()
{
    char h[1024];
    proto::ClientCaps caps{"13.0.0", true, true, true, true};
    proto::HelloExtras x;
    x.device = "AAECAwQFBgcICQoLDA0ODw";
    x.knock_via = "rooms";
    x.rooms = true;
    CHECK(proto::hello_ex(h, sizeof h, "", nullptr, "Mia", caps, x));
    CHECK(std::strstr(h, "\"device\":\"AAECAwQFBgcICQoLDA0ODw\"") && std::strstr(h, "\"knock\":{\"via\":\"rooms\"}"));
    CHECK(std::strstr(h, "\"proto\":2,\"caps\":[\"rooms\""));
    // a resume never knocks again
    CHECK(proto::hello_ex(h, sizeof h, "", "tok", "Mia", caps, x) && !std::strstr(h, "\"knock\":{"));
    proto::HelloExtras g;
    g.no_media = true;
    g.group = "tok1";
    CHECK(proto::hello_ex(h, sizeof h, "", nullptr, "Sam", caps, g) && std::strstr(h, "\"media\":false") &&
          std::strstr(h, "\"group\":\"tok1\""));
    proto::GuestEvent e;
    const char *k = R"({"knock":"challenge","c":"q83vEjRWeJA","pk":"BAAA"})";
    CHECK(proto::parse_guest(k, std::strlen(k), e) && e.type == proto::GuestEvent::knock &&
          !std::strcmp(e.knock_state, "challenge") && !std::strcmp(e.challenge, "q83vEjRWeJA") && !std::strcmp(e.pk, "BAAA"));
    const char *n = R"({"knock":"no","why":"password","left":4})";
    CHECK(proto::parse_guest(n, std::strlen(n), e) && !std::strcmp(e.why, "password") && e.tries_left == 4);
    const char *t = R"x({"name_taken":{"suggest":"Sam (2)"}})x";
    CHECK(proto::parse_guest(t, std::strlen(t), e) && e.type == proto::GuestEvent::name_taken &&
          !std::strcmp(e.suggest, "Sam (2)"));
    const char *j = R"({"stage":"joined","player":2,"token":"t","lobby":"LobbyId123"})";
    CHECK(proto::parse_guest(j, std::strlen(j), e) && !std::strcmp(e.lobby, "LobbyId123"));
}

void test_keyboard()
{
    textkb::Keyboard k;
    k.reset("", 8, false);
    CHECK(k.press() == false && !std::strcmp(k.text(), "q"));
    k.move(1, 0);
    k.press();
    CHECK(!std::strcmp(k.text(), "qw"));
    k.move(0, -1); // up from row 0 wraps to the bottom row
    CHECK(k.row() == 3);
    const char *label;
    CHECK(k.key(k.row(), k.col(), &label) == textkb::k_symbols);
    k.move(1, 0);
    CHECK(k.key(k.row(), k.col(), &label) == textkb::k_space);
    k.press();
    k.move(1, 0);
    CHECK(k.key(k.row(), k.col(), &label) == textkb::k_delete);
    k.press();
    CHECK(!std::strcmp(k.text(), "qw"));
    k.move(1, 0);
    CHECK(k.key(k.row(), k.col(), &label) == textkb::k_done && k.press());
    k.move(1, 0); // wraps to Shift
    CHECK(k.key(k.row(), k.col(), &label) == textkb::k_shift);
    k.press();
    k.move(0, 1); // to row 0, a capital once
    k.press();
    k.press();
    CHECK(!std::strcmp(k.text(), "qwQq"));
    for (int i = 0; i < 10; ++i)
        k.type("x");
    CHECK(k.chars() == 8);
    k.reset("", 64, true);
    k.type("a");
    k.type("b");
    char s[64];
    k.shown(s, sizeof s, true);
    CHECK(!std::strcmp(s, "\xe2\x80\xa2"
                          "b"));
    k.shown(s, sizeof s, false);
    CHECK(!std::strcmp(s, "\xe2\x80\xa2\xe2\x80\xa2"));
    k.reset("Ж", 8, false);
    k.del();
    CHECK(!std::strcmp(k.text(), ""));
}

void test_seats()
{
    // Many controllers on one PS5 (integration design (d)): one starts, the others hold ✕ 1 s.
    seats::Seats s;
    s.pad(10, "Alex", true, 0);
    s.pad(11, "Sam", true, 0);
    s.set_primary(10);
    CHECK(s.count() == 2 && s.at(0).state == seats::State::playing && s.at(1).state == seats::State::idle);
    // a short press is nothing
    CHECK(!s.hold(11, true, 100000) && !s.hold(11, false, 400000) && s.at(1).state == seats::State::idle);
    CHECK(!s.hold(11, true, 500000));
    CHECK(s.hold(11, true, 1500000) && s.at(1).state == seats::State::asking);
    s.joined(11, 3, "tokSam");
    CHECK(s.at(1).state == seats::State::playing && s.at(1).player == 3);
    // the controller goes off: the place is kept 60 s, then left
    s.pad(11, "Sam", false, 2000000);
    CHECK(s.at(1).state == seats::State::away && !s.expired(11, 50000000));
    CHECK(s.expired(11, 63000000));
    // back in time: resumes with its token
    s.pad(11, "Sam", true, 30000000);
    CHECK(s.at(1).state == seats::State::playing && !std::strcmp(s.at(1).token, "tokSam"));
    s.left(11);
    CHECK(s.at(1).state == seats::State::idle && !s.at(1).token[0]);
    // the game is full
    s.pad(12, "Mia", true, 0);
    s.set_full(true);
    CHECK(!s.hold(12, true, 100) && !s.hold(12, true, 2000000) && s.at(2).state == seats::State::idle);
    CHECK(s.playing() == 1);
}

void test_seal()
{
    // A room key, as the host engine would make it.
    std::uint8_t room_sk[32];
    br_sha256_context h;
    br_sha256_init(&h);
    br_sha256_update(&h, "room key for tests", 18);
    br_sha256_out(&h, room_sk);
    br_ec_private_key sk{BR_EC_secp256r1, room_sk, 32};
    std::uint8_t pk[BR_EC_KBUF_PUB_MAX_SIZE];
    CHECK(br_ec_compute_pub(&br_ec_p256_m31, nullptr, pk, &sk) == 65);
    char pk64[100];
    invite::b64url(pk, 65, pk64, sizeof pk64);
    const char *challenge = "AAECAwQFBgcICQoLDA0ODw"; // 16 bytes
    char blob[1024];
    const std::size_t n = seal::seal(pk64, challenge, "g7", "hunter22", blob, sizeof blob);
    CHECK(n > 100);
    char plain[512];
    CHECK(seal::open(blob, challenge, room_sk, plain, sizeof plain));
    CHECK(!std::strcmp(plain, "{\"secret\":\"hunter22\",\"gid\":\"g7\",\"challenge\":\"AAECAwQFBgcICQoLDA0ODw\"}"));
    // another challenge (a replay) doesn't open
    CHECK(!seal::open(blob, "BBECAwQFBgcICQoLDA0ODw", room_sk, plain, sizeof plain));
    // a changed byte doesn't open
    blob[n / 2] = blob[n / 2] == 'A' ? 'B' : 'A';
    CHECK(!seal::open(blob, challenge, room_sk, plain, sizeof plain));
    // a bad key seals nothing
    CHECK(!seal::seal("AAAA", challenge, "g", "x", blob, sizeof blob));
    // fixed inputs give a fixed blob (the vector tests/seal_check.py opens with standard libraries)
    std::uint8_t eph[32], nonce[12];
    for (int i = 0; i < 32; ++i)
        eph[i] = static_cast<std::uint8_t>(i + 1);
    for (int i = 0; i < 12; ++i)
        nonce[i] = static_cast<std::uint8_t>(0xa0 + i);
    char b1[1024], b2[1024];
    CHECK(seal::seal_with(pk64, challenge, "g7", "pw", eph, nonce, b1, sizeof b1));
    CHECK(seal::seal_with(pk64, challenge, "g7", "pw", eph, nonce, b2, sizeof b2) && !std::strcmp(b1, b2));
    std::FILE *f = std::fopen("build/host-tests/seal-vector.txt", "w");
    if (f)
    {
        char sk64[64];
        invite::b64url(room_sk, 32, sk64, sizeof sk64);
        std::fprintf(f, "%s\n%s\n%s\n", sk64, challenge, b1);
        std::fclose(f);
    }
    std::uint8_t dec[8];
    CHECK(seal::b64url_decode("AQID", dec, sizeof dec) == 3 && dec[2] == 3);
    CHECK(seal::b64url_decode("A*", dec, sizeof dec) == -1);
}
} // namespace

int main()
{
    test_list_sort_filter();
    test_status_preview_push();
    test_names();
    test_chat();
    test_voice();
    test_ptt_chord();
    test_notice();
    test_engine_ids();
    test_hello_rooms();
    test_keyboard();
    test_seats();
    test_seal();
    std::printf("rooms: %d checks, %d failed\n", checks, failures);
    return failures ? 1 : 0;
}
