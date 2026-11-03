import { describe, expect, it } from "vitest";

import { friendsIn, inviteOn, playerSlots, presenceJoin, readJoinState, slowFriend } from "./playTogetherState";
import { nearbyJoins } from "../../state/mesh";
import type { MeshDevice } from "../../api/mesh";
import { guestStatusText } from "./PlayTogether";

const engine = {
  open: true,
  invite: { url: "http://192.168.1.9:19113/join#i=00112233445566778899aabbccddeeff", code: "4821", expires_ms: 1800000 },
  guests: [
    { id: 7, name: "Ben", player: 3, status: "reconnecting", reconnecting_left_s: 42, rtt_ms: 30, connected: false },
    { id: 5, name: "Ann", player: 2, status: "playing", rtt_ms: 180, connected: true },
    { id: 9, name: "Cy", player: 4, status: "left", rtt_ms: 10, connected: false },
  ],
  asks: [{ id: 11, name: "Dee" }],
  settings: { block_system_buttons: false, max_players: 3 },
  helper_ok: true,
};

describe("Play Together host state", () => {
  it("reads the engine's answer, players in order", () => {
    const s = readJoinState(engine);
    expect(s.open).toBe(true);
    expect(s.invite).toEqual({ url: engine.invite.url, code: "4821", expiresMs: 1800000, urls: [] });
    expect(s.guests.map((g) => g.name)).toEqual(["Ann", "Ben", "Cy"]);
    expect(s.guests[1]).toMatchObject({ status: "reconnecting", reconnectingLeftS: 42, connected: false });
    expect(s.asks).toEqual([{ id: 11, name: "Dee" }]);
    expect(s.settings).toEqual({ blockSystemButtons: false, maxPlayers: 3 });
  });

  it("an older or broken answer never breaks the panel", () => {
    const s = readJoinState({ guests: [null, { id: 1 }], asks: "x", invite: { code: 1 } });
    expect(s.invite).toBeNull();
    expect(s.guests).toEqual([
      { id: 1, name: "?", player: 0, status: "playing", reconnectingLeftS: null, rttMs: null, connected: true },
    ]);
    expect(s.settings.blockSystemButtons).toBe(true);
    expect(readJoinState(null).helperOk).toBe(true);
    expect(readJoinState({ helper_ok: false, reason: "old_helper" })).toMatchObject({ helperOk: false, reason: "old_helper" });
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
