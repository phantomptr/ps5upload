import { describe, expect, it } from "vitest";

import { addLine, ChatPace, cleanChat, hideRude, quickText, readChat, unread, type ChatLine } from "./chat";
import { b64url, fromB64url, knockStep, readKnockReply, seal } from "./knock";
import { knockNoText } from "./knockText";
import { cleanName, looksLikeCode, looksLikeLink, nameKey, playerNameProblem, roomNameProblem } from "./names";
import { carriesStream, readPreview, streamParts } from "./preview";
import { nearRegion, readRegion, regionEstMs, regionFromTimeZone } from "./regions";
import { approvalFor, DEFAULT_ROOM, peopleIn, readRoom, readRoomAsk, readRoomPlayer, refusalOf, sizeChoices, visibilityDefaults } from "./room";
import {
  cardAction,
  DEFAULT_QUERY,
  expectedFor,
  filtered,
  hiddenBy,
  matchesFilters,
  olderVersion,
  queryString,
  readCard,
  readRoomList,
  refineOrder,
  ROOM_PROTO,
  versionForProto,
  type RoomCard,
} from "./rooms";
import {
  dbfs,
  duckGain,
  JitterBuffer,
  levelByte,
  personGain,
  pttKeyOk,
  readVoiceBundle,
  talking,
  VoiceGate,
  voiceBundle,
  voiceForTier,
  VoiceMeter,
  voiceUpFrame,
} from "./voice";
import { barsOf, msText, slowEnough, tierOfMs } from "../../join/routeTier";

const tr = (_k: string, _v: unknown, f: string) => f;

describe("tier table (design 6.1)", () => {
  it("bars, ms text and the slow callout", () => {
    expect([barsOf("excellent"), barsOf("good"), barsOf("playable"), barsOf("poor"), barsOf(null)]).toEqual([4, 3, 2, 1, 0]);
    expect(msText(0.4)).toBe("<1 ms");
    expect(msText(33.6)).toBe("34 ms");
    expect(msText(null)).toBeNull();
    expect([tierOfMs(20), tierOfMs(21), tierOfMs(40), tierOfMs(80), tierOfMs(81)]).toEqual(["excellent", "good", "good", "playable", "poor"]);
    expect(slowEnough("poor", false)).toBe(true);
    expect(slowEnough("playable", false)).toBe(false);
    expect(slowEnough("playable", true)).toBe(true);
    expect(slowEnough("good", true)).toBe(false);
  });
});

describe("regions", () => {
  it("reads only the 12 codes", () => {
    expect(readRegion("eu-west")).toBe("eu-west");
    expect(readRegion("mars")).toBeNull();
  });
  it("estimates like the hub", () => {
    expect(regionEstMs("eu-west", "eu-west")).toBe(30);
    expect(regionEstMs("eu-west", "eu-east")).toBe(70);
    expect(regionEstMs("asia-east", "asia-south")).toBe(100);
    expect(regionEstMs("eu-west", "oceania")).toBe(160);
    expect(regionEstMs(null, "eu-west")).toBeNull();
    expect(nearRegion("asia-se", "oceania")).toBe(true);
    expect(nearRegion("eu-west", "asia-east")).toBe(false);
  });
  it("guesses from the time zone", () => {
    expect(regionFromTimeZone("Europe/Berlin")).toBe("eu-west");
    expect(regionFromTimeZone("Europe/Warsaw")).toBe("eu-east");
    expect(regionFromTimeZone("America/Los_Angeles")).toBe("na-west");
    expect(regionFromTimeZone("America/Denver")).toBe("na-west");
    expect(regionFromTimeZone("America/Chicago")).toBe("na-east");
    expect(regionFromTimeZone("America/New_York")).toBe("na-east");
    expect(regionFromTimeZone("America/Sao_Paulo")).toBe("south-am");
    expect(regionFromTimeZone("America/Mexico_City")).toBe("central-am");
    expect(regionFromTimeZone("Asia/Tokyo")).toBe("asia-east");
    expect(regionFromTimeZone("Asia/Kolkata")).toBe("asia-south");
    expect(regionFromTimeZone("Asia/Singapore")).toBe("asia-se");
    expect(regionFromTimeZone("Asia/Dubai")).toBe("middle-east");
    expect(regionFromTimeZone("Africa/Cairo")).toBe("middle-east");
    expect(regionFromTimeZone("Africa/Lagos")).toBe("africa");
    expect(regionFromTimeZone("Australia/Sydney")).toBe("oceania");
    expect(regionFromTimeZone("Pacific/Honolulu")).toBe("na-west");
    expect(regionFromTimeZone("UTC")).toBeNull();
    expect(regionFromTimeZone("")).toBeNull();
  });
});

describe("names", () => {
  it("cleans invisible characters and spaces", () => {
    expect(cleanName(`  Sam${String.fromCodePoint(0x200b)}  's   room `)).toBe("Sam 's room");
  });
  it("room names: 3–32, no links/codes/reserved", () => {
    expect(roomNameProblem("Couch chaos")).toBeNull();
    expect(roomNameProblem("Ко-оп вечер")).toBeNull();
    expect(roomNameProblem("ab")).toBe("length");
    expect(roomNameProblem("x".repeat(33))).toBe("length");
    expect(roomNameProblem("join discord")).toBe("link");
    expect(roomNameProblem("play at games.gg")).toBe("link");
    expect(roomNameProblem("code K7P2Q9")).toBe("code");
    expect(roomNameProblem("call 5551234")).toBe("code");
    expect(roomNameProblem("NBA2K25 night")).toBeNull();
    expect(roomNameProblem("Official room")).toBe("reserved");
    expect(roomNameProblem("Sam@home")).toBe("chars");
    expect(roomNameProblem("!!!???!!")).toBe("spam");
    expect(roomNameProblem("aaaaaa")).toBe("spam");
    expect(playerNameProblem("J")).toBeNull();
  });
  it("same name key for look-alikes", () => {
    expect(nameKey("Sam")).toBe(nameKey("SAM "));
    expect(nameKey("Sam")).toBe(nameKey("Ѕаm"));
    expect(looksLikeLink("a . com")).toBe(true);
    expect(looksLikeCode("GT7")).toBe(false);
  });
});

describe("chat", () => {
  it("quick messages in words, unknown ids null", () => {
    expect(quickText(tr, 1)).toBe("Ready?");
    expect(quickText(tr, 16)).toBe("👍");
    expect(quickText(tr, 40)).toBeNull();
  });
  it("typed text: 140 chars, links not clickable, codes out of public rooms", () => {
    expect(cleanChat("  hi   there ")).toBe("hi there");
    expect(cleanChat("go to evil.com now")).toBe("go to evil·com now");
    expect(cleanChat("https://x.io")).not.toContain("://");
    expect([...cleanChat("é".repeat(300))].length).toBe(140);
    expect(cleanChat("code K7M-2QX", { publicRoom: true })).toBe("code …");
    expect(cleanChat("code K7M-2QX")).toBe("code K7M-2QX");
  });
  it("reads engine messages and folds repeats", () => {
    expect(readChat({ from: "Mia", quick: 5 })).toEqual({ from: "Mia", quick: 5, text: null });
    expect(readChat({ from: "Mia" })).toBeNull();
    let lines: ChatLine[] = [];
    lines = addLine(lines, { from: "Mia", mine: false, quick: 3, text: null, at: 1 });
    lines = addLine(lines, { from: "Mia", mine: false, quick: 3, text: null, at: 2 });
    lines = addLine(lines, { from: "Lee", mine: false, quick: null, text: "hi", at: 3 });
    expect(lines.map((l) => [l.from, l.times])).toEqual([["Mia", 2], ["Lee", 1]]);
    expect(unread(lines, 0)).toBe(3);
    expect(unread(lines, 1)).toBe(1);
    for (let i = 0; i < 60; i++) lines = addLine(lines, { from: "x", mine: false, quick: null, text: String(i), at: i });
    expect(lines.length).toBe(50);
  });
  it("paces sending: one per 2 s, ten a minute", () => {
    let t = 0;
    const p = new ChatPace(() => t);
    expect(p.take()).toBe(true);
    t = 1000;
    expect(p.take()).toBe(false);
    for (let i = 0; i < 9; i++) {
      t += 2000;
      expect(p.take()).toBe(true);
    }
    t += 2000;
    expect(p.take()).toBe(false);
    t += 60_000;
    expect(p.take()).toBe(true);
  });
  it("hides rude words locally", () => {
    expect(hideRude("well shit")).toBe("well ****");
    expect(hideRude("fine")).toBe("fine");
  });
});

describe("room state", () => {
  it("an engine without rooms: Private with the defaults", () => {
    expect(readRoom(undefined)).toEqual(DEFAULT_ROOM);
    expect(readRoom(undefined).known).toBe(false);
  });
  it("reads every field", () => {
    const r = readRoom({
      name: "Couch chaos",
      visibility: "public",
      size: 3,
      password: true,
      approval: { friends: false, password: true },
      lock: true,
      voice_kbps: { in: 30, out: 90 },
      preview_shows: { pings: false },
      region: "eu-west",
      lang: "en",
      game: { title: "ASTRO BOT", id: "PPSA01234" },
      listed: { state: "refused", why: "name" },
      blocked: [{ name: "Jo", tag: "9C11", at: 5 }, { bad: 1 }],
      host_link: { rtt_ms: 2, tier: "excellent" },
      ready: { left_s: 20, ready: [2, 3] },
    });
    expect(r.known).toBe(true);
    expect(r.chat).toBe("quick");
    expect(r.voice).toBe("ptt");
    expect(r.startMuted).toBe(true);
    expect(r.locked).toBe(true);
    expect(r.previewShows).toEqual({ names: true, pings: false, stream: true, time: true });
    expect(r.listed).toEqual({ state: "refused", why: "name", untilUnix: null });
    expect(readRoom({ listed: { state: "not_listed", why: "hub" } }).listed.state).toBe("refused");
    expect(readRoom({ listed: { state: "not_listed", why: "old_hub" } }).listed.state).toBe("old_hub");
    expect(r.blocked).toEqual([{ name: "Jo", tag: "9C11", at: 5 }]);
    expect(r.hostLink).toEqual({ rttMs: 2, tier: "excellent" });
    expect(r.ready).toEqual({ leftS: 20, ready: [2, 3] });
    expect(r.game).toEqual({ title: "ASTRO BOT", id: "PPSA01234", from: "auto" });
  });
  it("player and ask facts; `via` lan/internet is not a source", () => {
    expect(readRoomPlayer({ via: "internet" }).source).toBeNull();
    const p = readRoomPlayer({ source: "rooms", tag: "4F2A", muted: { voice: true }, speaking: true });
    expect(p).toMatchObject({ source: "rooms", tag: "4F2A", fromRooms: true, speaking: true, muted: { voice: true, chat: false } });
    expect(readRoomAsk({ via: "rooms", password_ok: true, tag: "9C11", rtt_hint: 70 })).toEqual({
      source: "rooms",
      passwordOk: true,
      tag: "9C11",
      rttHint: 70,
      group: null,
    });
  });
  it("defaults by visibility, size choices, approval needs a password", () => {
    expect(visibilityDefaults("public")).toEqual({ chat: "quick", voice: "ptt", startMuted: true });
    expect(visibilityDefaults("friends")).toEqual({ chat: "on", voice: "open", startMuted: false });
    expect(peopleIn([{ status: "playing" }, { status: "left" }, { status: "asking" }])).toBe(2);
    expect(sizeChoices(3)).toEqual([{ size: 2, ok: false }, { size: 3, ok: true }, { size: 4, ok: true }]);
    expect(approvalFor({ friends: true, password: true }, false)).toEqual({ friends: true, password: false });
  });
  it("refusals: known words, 404 = old engine", () => {
    expect(refusalOf(409, { error: "size_below_players" })).toBe("size_below_players");
    expect(refusalOf(409, { error: "friends_playing" })).toBe("friends_playing");
    expect(refusalOf(404, {})).toBe("old_engine");
    expect(refusalOf(500, { error: "x" })).toBe("engine");
  });
});

const card = (o: Partial<RoomCard> & { room: string }): RoomCard =>
  ({
    name: "Room",
    host: "Sam",
    tag: "4F2A",
    game: null,
    proto: ROOM_PROTO,
    ver: "13.0.0",
    caps: [],
    inRoom: 1,
    size: 4,
    pw: false,
    lang: null,
    region: null,
    estMs: null,
    ageS: 0,
    pk: null,
    ...o,
  }) as RoomCard;

describe("rooms list", () => {
  it("reads the hub's answer and drops bad cards", () => {
    const l = readRoomList({
      you: { country: "DE", region: "eu-west" },
      proto: { min: 2, latest: 2 },
      app: { latest: "13.0.1" },
      rooms: [
        { room: "A".repeat(22), name: "Couch", host: "Sam", tag: "4F2A", game: { title: "It Takes Two", id: "PPSA01234" }, slots: [1, 2], pw: true, est_ms: 70, region: "eu-west", proto: 2 },
        { room: "bad id!", name: "x" },
        { name: "no id" },
      ],
      next: 1,
    });
    expect(l.rooms).toHaveLength(1);
    expect(l.rooms[0]).toMatchObject({ inRoom: 1, size: 2, pw: true, estMs: 70, region: "eu-west" });
    expect(l.you.region).toBe("eu-west");
    expect(l.appLatest).toBe("13.0.1");
    expect(l.next).toBe(1);
    expect(readCard(null)).toBeNull();
  });
  it("query string: only what differs", () => {
    expect(queryString(DEFAULT_QUERY)).toBe("free=1&proto=2");
    expect(queryString({ ...DEFAULT_QUERY, sort: "game", game: " astro ", free: false, compatible: false, lang: "de", near: true, me: "eu-west", page: 2 })).toBe(
      "sort=game&game=astro&lang=de&near=1&me=eu-west&page=2",
    );
    expect(filtered(DEFAULT_QUERY)).toBe(false);
    expect(filtered({ ...DEFAULT_QUERY, nopw: true })).toBe(true);
  });
  it("expected latency: measured > last time > hub > region > unknown", () => {
    const c = card({ room: "A".repeat(22), estMs: 70, region: "eu-east" });
    expect(expectedFor(c, "eu-west", { tag: "4F2A", rtts: [], measured: { ms: 31, at: 1000 } }, 2000)).toEqual({ ms: 31, kind: "measured", tier: "good" });
    expect(expectedFor(c, "eu-west", { tag: "4F2A", rtts: [50, 20, 34, 90] }, 0)).toEqual({ ms: 34, kind: "last_time", tier: "good" });
    expect(expectedFor(c, "eu-west", null, 0)).toEqual({ ms: 70, kind: "about", tier: "playable" });
    expect(expectedFor({ ...c, estMs: null }, "eu-west", null, 0)?.ms).toBe(70);
    expect(expectedFor({ ...c, estMs: null, region: null }, "eu-west", null, 0)).toBeNull();
    // A measurement older than 10 minutes no longer counts.
    expect(expectedFor(c, null, { tag: "4F2A", rtts: [], measured: { ms: 31, at: 0 } }, 11 * 60_000)?.kind).toBe("about");
  });
  it("card buttons", () => {
    expect(cardAction({ inRoom: 1, size: 4, proto: 2, pw: false })).toBe("ask");
    expect(cardAction({ inRoom: 1, size: 4, proto: 2, pw: false }, { autoIn: true })).toBe("join");
    expect(cardAction({ inRoom: 4, size: 4, proto: 2, pw: false })).toBe("full");
    expect(cardAction({ inRoom: 1, size: 4, proto: 5, pw: false })).toBe("update");
    expect(cardAction({ inRoom: 1, size: 4, proto: 2, pw: false }, { minProto: 3 })).toBe("update");
  });
  it("refines the order by measured pings, never while focused", () => {
    const a = card({ room: "A".repeat(22), estMs: 70 });
    const b = card({ room: "B".repeat(22), estMs: 30, inRoom: 4 });
    const c = card({ room: "C".repeat(22), estMs: 50 });
    const exp = (x: RoomCard) => expectedFor(x, null, x === a ? { tag: "", rtts: [], measured: { ms: 10, at: 0 } } : null, 0);
    expect(refineOrder([a, b, c], "best", exp, false).map((x) => x.room[0])).toEqual(["A", "C", "B"]);
    expect(refineOrder([a, b, c], "latency", exp, false).map((x) => x.room[0])).toEqual(["A", "B", "C"]);
    expect(refineOrder([a, b, c], "latency", exp, true).map((x) => x.room[0])).toEqual(["A", "B", "C"]);
    expect(refineOrder([b, a], "game", exp, false).map((x) => x.room[0])).toEqual(["B", "A"]);
  });
  it("filters friends' and nearby rooms the same way", () => {
    const c = card({ room: "A".repeat(22), game: { title: "ASTRO BOT", id: "PPSA01234" }, pw: true, lang: "en", region: "eu-west" });
    expect(matchesFilters(c, DEFAULT_QUERY, null)).toBe(true);
    expect(matchesFilters(c, { ...DEFAULT_QUERY, game: "astro" }, null)).toBe(true);
    expect(matchesFilters(c, { ...DEFAULT_QUERY, game: "ppsa01234" }, null)).toBe(true);
    expect(matchesFilters(c, { ...DEFAULT_QUERY, game: "gran" }, null)).toBe(false);
    expect(matchesFilters(c, { ...DEFAULT_QUERY, nopw: true }, null)).toBe(false);
    expect(matchesFilters(c, { ...DEFAULT_QUERY, lang: "de" }, null)).toBe(false);
    expect(matchesFilters(c, { ...DEFAULT_QUERY, near: true }, "eu-east")).toBe(true);
    expect(matchesFilters(c, { ...DEFAULT_QUERY, near: true }, "asia-east")).toBe(false);
    expect(matchesFilters({ ...c, inRoom: 4 }, DEFAULT_QUERY, null)).toBe(false);
    expect(hiddenBy(c, new Set(["tag:4F2A"]))).toBe(true);
  });
  it("versions", () => {
    expect(versionForProto(2)).toBe("13.0.0");
    expect(olderVersion("12.6.45", "13.0.0")).toBe(true);
    expect(olderVersion("13.0.0", "13.0.0")).toBe(false);
    expect(olderVersion("13.0.1", "13.0.0")).toBe(false);
  });
});

describe("preview", () => {
  it("reads the host's preview, never more than it holds", () => {
    const p = readPreview({
      proto: 2,
      caps: ["chat", "preview"],
      host: { name: "Sam", tag: "4F2A", ver: "13.0.0", playing_for_min: 42 },
      room: { name: "Couch chaos", visibility: "public", size: 4, pw: true, approval: "ask", chat: "quick", lang: "en", region: "eu-west", watchers: 1 },
      game: { id: "PPSA01234", title: "It Takes Two", players: "1-2", genre: null },
      stream: { res: "1080p", fps: 60, codec: "hevc", kbps: 15000, hdr: false },
      host_link: { rtt_ms: 2, tier: "excellent" },
      players: [
        { name: "Lee", seat: 3, kind: "web", tier: "playable", rtt_ms: 74 },
        { name: "Sam", seat: 1, kind: "host" },
      ],
      asking: 1,
    });
    expect(p?.players.map((x) => x.seat)).toEqual([1, 3]);
    expect(p?.room.chat).toBe("quick");
    expect(streamParts(p!.stream!)).toEqual(["1080p", "60 fps", "HEVC", "15 Mbps"]);
    expect(readPreview({ host: {} })).toBeNull();
    expect(carriesStream(20000, 15000)).toBe(true);
    expect(carriesStream(16000, 15000)).toBe(false);
    expect(carriesStream(null, 15000)).toBeNull();
  });
});

describe("voice", () => {
  it("frames both ways", () => {
    const up = voiceUpFrame(0x1234, 200, new Uint8Array([9, 8, 7]));
    expect([...up]).toEqual([0x0a, 0x12, 0x34, 200, 9, 8, 7]);
    const b = voiceBundle([
      { speaker: 2, seq: 1, opus: new Uint8Array([1, 2]) },
      { speaker: 3, seq: 0xffff, opus: new Uint8Array([3]) },
    ]);
    expect(readVoiceBundle(b)).toEqual([
      { speaker: 2, seq: 1, opus: new Uint8Array([1, 2]) },
      { speaker: 3, seq: 0xffff, opus: new Uint8Array([3]) },
    ]);
    expect(readVoiceBundle(b.subarray(0, b.length - 1))).toBeNull();
    expect(readVoiceBundle(new Uint8Array([3, 0]))).toBeNull();
    // The engine's bytes as API.md writes them: [0x0B][1][sid 2][seq 0x0102 BE][len 1][0xAA].
    expect(readVoiceBundle(new Uint8Array([0x0b, 1, 2, 1, 2, 1, 0xaa]))).toEqual([{ speaker: 2, seq: 0x0102, opus: new Uint8Array([0xaa]) }]);
  });
  it("levels", () => {
    expect(levelByte(-Infinity)).toBe(0);
    expect(levelByte(0)).toBe(255);
    expect(levelByte(-45)).toBe(128);
    expect(dbfs(new Float32Array(480).fill(0.5))).toBeCloseTo(-6.02, 1);
  });
  it("jitter buffer: waits for depth, conceals gaps, ends spurts, survives wrap", () => {
    const j = new JitterBuffer(2, 5);
    const f = (n: number) => new Uint8Array([n]);
    expect(j.pop()).toBeNull();
    j.push(0xfffe, f(1));
    expect(j.pop()).toBeNull();
    j.push(0xffff, f(2));
    expect(j.pop()).toEqual(f(1));
    j.push(1, f(4));
    expect(j.pop()).toEqual(f(2));
    expect(j.pop()).toBe("lost"); // seq 0 missing
    expect(j.pop()).toEqual(f(4));
    expect(j.pop()).toBeNull(); // spurt over
    // Late frames grow the target.
    j.push(10, f(10));
    j.push(11, f(11));
    j.pop();
    j.pop();
    j.push(12, f(12));
    j.push(13, f(13));
    j.pop();
    j.push(5, f(5));
    j.push(6, f(6));
    expect(j.target).toBe(3);
  });
  it("who talks when", () => {
    const base = { mic: true, how: "ptt" as const, held: false, room: "open" as const, hostMuted: false, paused: false };
    expect(talking(base)).toBe(false);
    expect(talking({ ...base, held: true })).toBe(true);
    expect(talking(base, 200)).toBe(true);
    expect(talking(base, 400)).toBe(false);
    expect(talking({ ...base, how: "open" })).toBe(true);
    expect(talking({ ...base, how: "open", room: "ptt" })).toBe(false);
    expect(talking({ ...base, held: true, hostMuted: true })).toBe(false);
    expect(talking({ ...base, held: true, mic: false })).toBe(false);
    expect(talking({ ...base, held: true, paused: true })).toBe(false);
    expect(talking({ ...base, held: true, room: "off" })).toBe(false);
  });
  it("gate, tiers, ducking, keys, volume, meter", () => {
    const g = new VoiceGate(-48);
    expect(g.feed(-30, 0)).toBe(true);
    expect(g.feed(-80, 250)).toBe(true);
    expect(g.feed(-80, 301)).toBe(false);
    expect(voiceForTier("good")).toEqual({ kbps: 24, paused: false });
    expect(voiceForTier("playable")).toEqual({ kbps: 12, paused: false });
    expect(voiceForTier("poor").paused).toBe(true);
    expect(duckGain("little", true)).toBe(0.5);
    expect(duckGain("more", true)).toBe(0.25);
    expect(duckGain("more", false)).toBe(1);
    expect(pttKeyOk("Backquote", new Set(["KeyW"]))).toBe(true);
    expect(pttKeyOk("KeyW", new Set(["KeyW"]))).toBe(false);
    expect(pttKeyOk("Escape", new Set())).toBe(false);
    expect(personGain(150, false)).toBe(1.5);
    expect(personGain(300, false)).toBe(2);
    expect(personGain(100, true)).toBe(0);
    let t = 0;
    const m = new VoiceMeter(() => t);
    m.in(3000);
    m.out(1500);
    expect(m.read()).toEqual({ inKbps: 12, outKbps: 6 });
    t = 3000;
    expect(m.read()).toEqual({ inKbps: 0, outKbps: 0 });
  });
});

describe("knocks", () => {
  it("reads the host's answers", () => {
    expect(readKnockReply({ knock: "wait" })).toEqual({ kind: "wait" });
    expect(readKnockReply({ knock: "ok" })).toEqual({ kind: "ok", token: null, lobby: null });
    expect(readKnockReply({ knock: "ok", token: "T1", lobby: "L" })).toEqual({ kind: "ok", token: "T1", lobby: "L" });
    expect(readKnockReply({ knock: "no", why: "password", tries_left: 4 })).toEqual({ kind: "no", why: "password", triesLeft: 4, suggest: null });
    expect(readKnockReply({ knock: "no", why: "name_taken", name_taken: { suggest: "Sam (2)" } })).toMatchObject({ why: "name_taken", suggest: "Sam (2)" });
    expect(readKnockReply({ knock: "no", why: "weird" })).toMatchObject({ why: "no" });
    expect(readKnockReply({ knock: "challenge", c: b64url(new Uint8Array(16).fill(7)) })).toMatchObject({ kind: "challenge" });
    expect(readKnockReply({ knock: "challenge", c: "!!" })).toBeNull();
    expect(readKnockReply({})).toBeNull();
  });
  it("steps and words", () => {
    expect(knockStep({ kind: "idle" }, { kind: "wait" }, 5)).toEqual({ kind: "waiting", since: 5 });
    expect(knockStep({ kind: "waiting", since: 5 }, { kind: "wait" }, 9)).toEqual({ kind: "waiting", since: 5 });
    expect(knockNoText(tr, "password", "Sam", 4)).toBe("Wrong password. 4 tries left.");
    expect(knockNoText(tr, "no", "Sam")).toBe("Sam said no.");
  });
  it("seals so only the room key opens it, bound to the gid and challenge", async () => {
    const subtle = globalThis.crypto.subtle;
    const room = (await subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, true, ["deriveBits"])) as CryptoKeyPair;
    const pk = b64url(new Uint8Array(await subtle.exportKey("raw", room.publicKey)));
    const challenge = new Uint8Array(16).fill(3);
    const blob = await seal(pk, "g1", challenge, { pw: "hunter22" });
    expect(blob).toBeTruthy();
    // The host engine's side: open it.
    const raw = fromB64url(blob!)!;
    const eph = await subtle.importKey("raw", raw.slice(0, 65), { name: "ECDH", namedCurve: "P-256" }, false, []);
    const shared = await subtle.deriveBits({ name: "ECDH", public: eph }, room.privateKey, 256);
    const hk = await subtle.importKey("raw", shared, "HKDF", false, ["deriveKey"]);
    const info = new Uint8Array([...new TextEncoder().encode("ps5upload room knock v1"), ...raw.slice(0, 65)]);
    const key = await subtle.deriveKey(
      { name: "HKDF", hash: "SHA-256", salt: challenge, info },
      hk,
      { name: "AES-GCM", length: 128 },
      false,
      ["decrypt"],
    );
    const plain = await subtle.decrypt({ name: "AES-GCM", iv: raw.slice(65, 77), additionalData: new TextEncoder().encode("g1") }, key, raw.slice(77));
    expect(JSON.parse(new TextDecoder().decode(plain))).toEqual({ pw: "hunter22", gid: "g1", c: b64url(challenge) });
    // Another gid can't open it.
    await expect(subtle.decrypt({ name: "AES-GCM", iv: raw.slice(65, 77), additionalData: new TextEncoder().encode("g2") }, key, raw.slice(77))).rejects.toBeTruthy();
    expect(await seal("short", "g1", challenge, { pw: "x" })).toBeNull();
  });
});
