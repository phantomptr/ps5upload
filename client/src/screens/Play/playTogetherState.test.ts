import { afterEach, describe, expect, it, vi } from "vitest";

import { friendsIn, inviteOn, foundOf, joinApi, openUntilPlaying, openWhenPlaying, playerSlots, presenceJoin, readJoinState, slowFriend, streamSeatFriend } from "./playTogetherState";
import { nearbyJoins } from "../../state/mesh";
import type { MeshDevice } from "../../api/mesh";
import { guestStatusText } from "./PlayTogether";

// The engine's GET /api/join (join.rs state_json), field for field.
const engine = {
  open: true,
  console: "192.168.1.100",
  playing: true,
  host_away_s: null,
  invite: {
    key: "0f".repeat(16),
    code: "4821",
    code_display: "4821",
    url: `http://192.168.1.9:19113/join#i=${"0f".repeat(16)}`,
    urls: [`http://192.168.1.9:19113/join#i=${"0f".repeat(16)}`],
    expires_in_s: 1800,
  },
  players_in: 2,
  guests: [
    { id: 7, name: "Ben", player: 3, state: "reconnecting", connected: false, reconnect_left_s: 42, rtt_ms: 30, queue: 0, skipped: 0, since_s: 60, why: null },
    { id: 5, name: "Ann", player: 2, state: "playing", connected: true, reconnect_left_s: null, rtt_ms: 180, queue: 0, skipped: 0, since_s: 90, why: null },
    { id: 9, name: "Cy", player: 4, state: "left", connected: false, reconnect_left_s: null, rtt_ms: 10, queue: 0, skipped: 0, since_s: 30, why: "left" },
  ],
  pending: [{ id: 11, name: "Dee", state: "asking" }],
  settings: { block_system_buttons: false, max_players: 3 },
  max_guests: 3,
  ws: "ws://192.168.1.9:19113/api/join/ws",
};

describe("Play Together host state", () => {
  it("reads the engine's answer, players in order", () => {
    const s = readJoinState(engine);
    expect(s.open).toBe(true);
    expect(s.invite).toEqual({ url: engine.invite.url, code: "4821", expiresMs: 1800000, urls: engine.invite.urls, tailscale: null, internet: null, share: null });
    expect(s.ws).toBe(engine.ws);
    expect(s.guests.map((g) => g.name)).toEqual(["Ann", "Ben", "Cy"]);
    expect(s.guests[1]).toMatchObject({ status: "reconnecting", reconnectingLeftS: 42, connected: false });
    expect(s.asks).toEqual([{ id: 11, name: "Dee" }]);
    expect(s.settings).toEqual({ blockSystemButtons: false, maxPlayers: 3, streamSeat: false, internetInvites: null, invites: null, hubUrl: "", stun: [], upnp: true, videoTrack: true });
    expect(s.seatNext).toBe(false);
  });

  it("reads the stream seat: the setting, who has it, and whether the next friend gets it (#720)", () => {
    const s = readJoinState({
      ...engine,
      settings: { ...engine.settings, stream_seat: true },
      seat_next: true,
      guests: [
        { id: 1, name: "Ann", player: 1, state: "playing", stream_seat: true },
        // Moving off it (the host needs it back): Player 1 still, no longer on it.
        { id: 2, name: "Ben", player: 1, state: "reconnecting", stream_seat: false },
      ],
    });
    expect(s.settings.streamSeat).toBe(true);
    expect(s.seatNext).toBe(true);
    expect(s.guests.map((g) => g.streamSeat)).toEqual([true, false]);
    // An older engine says nothing: its Player 1 was the stream's controller.
    expect(readJoinState({ guests: [{ id: 3, player: 1 }] }).guests[0].streamSeat).toBe(true);
    expect(readJoinState({ guests: [{ id: 3, player: 2 }] }).guests[0].streamSeat).toBe(false);
  });

  it("sends the stream seat setting", async () => {
    const calls: Array<[string, unknown]> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        calls.push([url, init?.body ? JSON.parse(String(init.body)) : null]);
        return new Response(JSON.stringify(engine), { status: 200 });
      }),
    );
    await joinApi.settings("192.168.1.100", { stream_seat: true });
    expect(calls[0][0]).toMatch(/\/api\/join\/settings$/);
    expect(calls[0][1]).toEqual({ addr: "192.168.1.100", stream_seat: true });
    vi.unstubAllGlobals();
  });

  it("an older or broken answer never breaks the panel", () => {
    const s = readJoinState({ guests: [null, { id: 1 }], asks: "x", invite: { code: 1 } });
    expect(s.invite).toBeNull();
    expect(s.guests).toEqual([
      { id: 1, name: "", player: 0, status: "playing", reconnectingLeftS: null, rttMs: null, connected: true, behind: false, streamSeat: false, via: "lan", path: null, netPath: null, ipv6: false, playsAs: null, seatNote: null, quality: null, carries: null, link: null },
    ]);
    expect(s.settings.blockSystemButtons).toBe(false);
    expect(readJoinState(null).helperOk).toBe(true);
    // An older engine's names still read.
    const old = readJoinState({ asks: [{ id: 2, name: "Eve" }], guests: [{ id: 3, player: 2, status: "reconnecting", reconnecting_left_s: 9 }] });
    expect(old.asks).toEqual([{ id: 2, name: "Eve" }]);
    expect(old.guests[0]).toMatchObject({ status: "reconnecting", reconnectingLeftS: 9 });
  });

  it("counts the friends stopping Play would end (not asks, not those who left)", () => {
    expect(friendsIn(readJoinState(engine)).map((g) => g.name)).toEqual(["Ann", "Ben"]);
  });

  it("advises on the slowest friend who is playing", () => {
    expect(slowFriend(readJoinState(engine))?.name).toBe("Ann");
    expect(slowFriend(readJoinState({ ...engine, guests: [engine.guests[0]] }))).toBeNull();
  });

  it("lays out P2–P4 with the friend in each", () => {
    const slots = playerSlots(readJoinState(engine));
    expect(slots.map((s) => [s.player, s.guest?.name ?? null])).toEqual([
      [2, "Ann"],
      [3, "Ben"],
      [4, "Cy"],
    ]);
    expect(playerSlots(readJoinState({ settings: { max_players: 1 } })).map((s) => s.player)).toEqual([2]);
  });

  it("puts the friend on the stream's controller first, within the friends allowed", () => {
    const guests = [
      { id: 1, name: "Ann", player: 1, status: "playing" },
      { id: 2, name: "Ben", player: 2, status: "playing" },
    ];
    const slots = playerSlots(readJoinState({ settings: { max_players: 3 }, guests }));
    expect(slots.map((s) => [s.player, s.guest?.name ?? null])).toEqual([
      [1, "Ann"],
      [2, "Ben"],
      [3, null],
    ]);
    const left = [{ id: 1, name: "Ann", player: 1, status: "left" }];
    const after = readJoinState({ settings: { max_players: 1 }, guests: left });
    expect(streamSeatFriend(after)).toBeNull();
    expect(playerSlots(after).map((s) => s.player)).toEqual([2]);
  });

  it("moves an invite to another address of this computer", () => {
    expect(inviteOn("http://192.168.1.9:19113/join#i=abc", "http://10.0.0.4:19113")).toBe("http://10.0.0.4:19113/join#i=abc");
    expect(inviteOn("http://192.168.1.9:19113/join#i=abc", "100.64.0.2")).toBe("http://100.64.0.2:19113/join#i=abc");
  });

  it("says each status in words, with the reconnect countdown", () => {
    const tr = ((_k: string, a?: unknown, b?: string) => (typeof a === "string" ? a : (b ?? _k))) as never;
    const s = readJoinState(engine);
    expect(s.guests.map((g) => guestStatusText(tr, g))).toEqual(["Playing", "Reconnecting 0:42", "Left"]);
    expect(guestStatusText(tr, { ...s.guests[0], status: "picking_user" })).toBe("Picking a user on the TV");
    expect(guestStatusText(tr, { ...s.guests[0], status: "asking" })).toBe("Asking");
  });

  it("tells nearby apps about an open Play Together, and nothing when closed", () => {
    const s = readJoinState(engine);
    expect(s.ws).toBe("ws://192.168.1.9:19113/api/join/ws");
    expect(presenceJoin(s, "Ann's Mac", "Living room", "Astro Bot")).toEqual({
      code: "4821",
      players: 3,
      max: 4,
      host_name: "Ann's Mac",
      console_name: "Living room",
      game_title: "Astro Bot",
      ws: "ws://192.168.1.9:19113/api/join/ws",
    });
    expect(presenceJoin({ ...s, open: false }, "a", "b")).toBeNull();
    expect(presenceJoin({ ...s, helperOk: false }, "a", "b")).toBeNull();
  });
});

describe("playing nearby", () => {
  const dev = (id: string, ws: string | null): MeshDevice => ({
    id,
    name: id,
    nick: "",
    kind: "desktop",
    os: "",
    app: "10.8.0",
    first: 0,
    last: 0,
    ip: "",
    online: true,
    blocked: false,
    activity: [
      {
        kind: "Remote Play",
        ...(ws ? { join: { code: "K7M2QX", players: 2, max: 4, host_name: id, console_name: "Pro", ws } } : {}),
      },
    ],
  });

  it("lists other hosts once, never this app or its own engine", () => {
    const state = {
      you: "me",
      devices: [
        dev("me", "ws://10.0.0.1:19113/api/join/ws"),
        dev("ann", "ws://10.0.0.2:19113/api/join/ws"),
        dev("ann-tab2", "ws://10.0.0.2:19113/api/join/ws"),
        dev("own-engine-tab", "ws://10.0.0.9:19113/api/join/ws"),
        dev("ben", null),
      ],
    } as never;
    expect(nearbyJoins(state, "ws://10.0.0.9:19113/api/join/ws").map((x) => x.device.id)).toEqual(["ann"]);
    expect(nearbyJoins(null)).toEqual([]);
  });
});

describe("New link (#657)", () => {
  afterEach(() => vi.unstubAllGlobals());

  /** The engine's answers, and what each call sent. */
  function engine() {
    const calls: Array<{ url: string; body: unknown }> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        calls.push({ url, body: init?.body ? JSON.parse(String(init.body)) : null });
        const json = url.includes("/api/join/invite") ? { key: "k2" } : { open: true, invite: { url: "http://192.0.2.1:19113/join#i=k2", code: "ABCD" } };
        return new Response(JSON.stringify(json), { status: 200 });
      }),
    );
    return calls;
  }

  it("asks the engine for a new invite (without `new` it hands back the same one)", async () => {
    const calls = engine();
    const s = await joinApi.invite("192.0.2.9", true);
    expect(calls[0].url).toContain("/api/join/invite");
    expect(calls[0].body).toEqual({ addr: "192.0.2.9", new: true });
    expect(s?.invite?.url).toContain("i=k2");
  });

  it("opening the panel keeps the open invite", async () => {
    const calls = engine();
    await joinApi.invite("192.0.2.9");
    expect(calls[0].body).toEqual({ addr: "192.0.2.9" });
  });

  it("an engine that says no is a null, for the panel to show", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({ error: "closed" }), { status: 409 })));
    expect(await joinApi.invite("192.0.2.9", true)).toBeNull();
  });
});

describe("opening while the stream restarts (#720)", () => {
  afterEach(() => vi.unstubAllGlobals());
  const answers = (list: Array<{ status: number; body: unknown }>) => {
    const f = vi.fn(async () => {
      const a = list.length > 1 ? list.shift()! : list[0];
      return new Response(JSON.stringify(a.body), { status: a.status });
    });
    vi.stubGlobal("fetch", f);
    return f;
  };
  const noWait = async () => {};

  it("asks again while the engine says not_playing, then has the party", async () => {
    const f = answers([
      { status: 409, body: { error: "not_playing" } },
      { status: 409, body: { error: "not_playing" } },
      { status: 200, body: engine },
    ]);
    const r = await openWhenPlaying("192.0.2.9", () => true, 2000, 90_000, noWait);
    expect(r?.state?.open).toBe(true);
    expect(f).toHaveBeenCalledTimes(3);
  });

  it("any other refusal is the answer at once", async () => {
    const f = answers([{ status: 409, body: { error: "old_helper" } }]);
    expect(await openWhenPlaying("192.0.2.9", () => true, 2000, 90_000, noWait)).toEqual({ error: "old_helper" });
    expect(f).toHaveBeenCalledTimes(1);
  });

  it("gives up after its time, and stops when the panel went away", async () => {
    const f = answers([{ status: 409, body: { error: "not_playing" } }]);
    expect(await openWhenPlaying("192.0.2.9", () => true, 2000, 6000, noWait)).toEqual({ error: "not_playing" });
    expect(f).toHaveBeenCalledTimes(4);
    let alive = true;
    const g = answers([{ status: 409, body: { error: "not_playing" } }]);
    const r = await openWhenPlaying("192.0.2.9", () => alive, 2000, 90_000, async () => {
      alive = false;
    });
    expect(r).toBeNull();
    expect(g).toHaveBeenCalledTimes(1);
  });

  // A phone showed "could not start (not_playing)" over a running picture, for good.
  it("after its time the refusal is told with what the engine found, and it keeps asking until the session is up", async () => {
    const no = { status: 409, body: { error: "not_playing", asked: "192.0.2.9", streaming: null, stopping: false } };
    // 4 asks fill the 6 s; the refusal is told; the next round's first ask has the party.
    const f = answers([no, no, no, no, { status: 200, body: engine }]);
    const told: Array<{ error: string; found?: string }> = [];
    const s = await openUntilPlaying("192.0.2.9", () => true, (r) => told.push(r), 2000, 6000, noWait);
    expect(s?.open).toBe(true);
    expect(told).toEqual([{ error: "not_playing", found: "asked 192.0.2.9, streaming none" }]);
    expect(f).toHaveBeenCalledTimes(5);
  });

  it("another refusal is told once and ends it; a panel that went away is told nothing", async () => {
    const f = answers([{ status: 409, body: { error: "old_helper" } }]);
    const told: string[] = [];
    expect(await openUntilPlaying("192.0.2.9", () => true, (r) => told.push(r.error), 2000, 6000, noWait)).toBeNull();
    expect(told).toEqual(["old_helper"]);
    expect(f).toHaveBeenCalledTimes(1);

    answers([{ status: 409, body: { error: "not_playing" } }]);
    let alive = true;
    const quiet: string[] = [];
    const r = await openUntilPlaying("192.0.2.9", () => alive, (x) => quiet.push(x.error), 2000, 6000, async () => {
      alive = false;
    });
    expect(r).toBeNull();
    expect(quiet).toEqual([]);
  });

  it("says what a not_playing refusal found: the console asked for and the one streaming", () => {
    expect(foundOf({ error: "not_playing", asked: "10.0.0.2", streaming: "ps5.lan", stopping: true })).toBe(
      "asked 10.0.0.2, streaming ps5.lan (stopping)",
    );
    expect(foundOf({ error: "not_playing", asked: "10.0.0.2", streaming: null })).toBe("asked 10.0.0.2, streaming none");
    // An engine from before it.
    expect(foundOf({ error: "not_playing" })).toBeUndefined();
  });
});
