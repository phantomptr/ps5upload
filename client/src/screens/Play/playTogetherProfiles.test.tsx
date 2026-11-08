import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

import { installWindowStub } from "../../test-utils/storage";
import { JoinAsks, PlayTogetherPanel } from "./PlayTogether";
import { joinApi, readJoinState, readProfiles, withSeat } from "./playTogetherState";

// "Guests play as": what the engine says (join.rs snapshot / state_json / profiles), field for field.
const engine = {
  open: true,
  console: "192.168.1.100",
  invite: { key: "0f".repeat(16), code: "4821", url: "http://192.168.1.9:19113/join#i=0f", urls: [], expires_in_s: 1800 },
  guests: [
    { id: 5, name: "Ann", player: 2, state: "playing", connected: true, plays_as: { id: 8192, name: "Guest 1" }, seat_note: null },
    { id: 6, name: "Ben", player: 3, state: "picking_user", connected: true, plays_as: null, seat_note: "signed_out" },
    { id: 7, name: "Cy", player: 4, state: "picking_user", connected: true, plays_as: null, seat_note: "failed" },
  ],
  pending: [{ id: 11, name: "Dee", state: "asking" }],
  settings: { block_system_buttons: true, max_players: 3, stream_seat: false },
  seat_next: false,
  next_profile: { player: 2, id: 8192, name: "Guest 1" },
  guest_profiles: [{ player: 2, user: 8192, name: "Guest 1", owner: false }],
};

afterEach(() => vi.unstubAllGlobals());

describe("Guests play as: the host's panel", () => {
  it("reads who plays as which profile, why a choice wasn't used, and the next friend's", () => {
    const s = readJoinState(engine);
    expect(s.guests.map((g) => [g.name, g.playsAs, g.seatNote])).toEqual([
      ["Ann", "Guest 1", null],
      ["Ben", null, "signed_out"],
      ["Cy", null, "failed"],
    ]);
    expect(s.nextProfile).toEqual({ player: 2, name: "Guest 1" });
    // An older engine says nothing: the TV asks every friend.
    const old = readJoinState({ ...engine, next_profile: undefined, guests: [{ id: 1, player: 2 }] });
    expect(old.nextProfile).toBeNull();
    expect(old.guests[0]).toMatchObject({ playsAs: null, seatNote: null });
    expect(readJoinState({ next_profile: { name: "X" } }).nextProfile).toBeNull();
  });

  it("player cards say the profile, or why the TV asks; the ask names the profile", () => {
    installWindowStub();
    const out = renderToStaticMarkup(
      <PlayTogetherPanel addr="192.168.1.100" state={readJoinState(engine)} onState={() => {}} onClose={() => {}} onPhones={() => {}} />,
    );
    expect(out).toMatch(/data-testid="pt-player-plays-as">Plays as Guest 1</);
    expect(out.match(/data-testid="pt-player-plays-as"/g)).toHaveLength(1);
    expect(out).toContain('data-note="signed_out"');
    expect(out).toContain("Their chosen profile isn&#x27;t signed in on the PS5, so the TV asks who they are.");
    expect(out).toContain("Their chosen profile couldn&#x27;t be used, so the TV asks who they are.");
    expect(out).toMatch(/data-testid="pt-ask-profile">If you allow Dee, they play as Guest 1\.</);
  });

  it("the Allow prompt over the picture names the profile, not when the stream's controller is next", () => {
    const asks = [{ id: 11, name: "Dee" }];
    const withProfile = renderToStaticMarkup(<JoinAsks addr="c" asks={asks} nextProfile="Guest 1" />);
    expect(withProfile).toContain("If you allow Dee, they play as Guest 1.");
    const seat = renderToStaticMarkup(<JoinAsks addr="c" asks={asks} seatNext nextProfile="Guest 1" />);
    expect(seat).not.toContain('data-testid="pt-ask-profile"');
    expect(renderToStaticMarkup(<JoinAsks addr="c" asks={asks} />)).not.toContain("pt-ask-profile");
  });
});

describe("Guests play as: Settings", () => {
  const profiles = {
    users: [
      { id: 505721148, name: "Me", foreground: true },
      { id: 8192, name: "Guest 1", foreground: false },
      { id: 0, name: "nobody", foreground: false },
      { id: 4294967295, name: "nobody either", foreground: false },
    ],
    seats: [
      { player: 2, user: 8192, name: "Guest 1", owner: false },
      { player: 9, user: 1, name: "odd", owner: false },
    ],
    companion: "running",
  };

  it("reads the signed-in profiles (never 0 or the PS5's 'nobody') and the choices", () => {
    const p = readProfiles(profiles);
    expect(p.users.map((u) => [u.id, u.foreground])).toEqual([
      [505721148, true],
      [8192, false],
    ]);
    expect(p.seats).toEqual([{ player: 2, user: 8192, name: "Guest 1", owner: false }]);
    expect(p.companion).toBe("running");
    expect(readProfiles(null)).toEqual({ users: [], seats: [], companion: null });
  });

  it("a profile is on one player at most, and Let the TV ask removes the choice", () => {
    const guest = { id: 8192, name: "Guest 1", foreground: false };
    const me = { id: 505721148, name: "Me", foreground: true };
    let seats = withSeat([], 3, guest, false);
    expect(seats).toEqual([{ player: 3, user: 8192, name: "Guest 1", owner: false }]);
    // chosen for Player 2: it moves there
    seats = withSeat(seats, 2, guest, false);
    expect(seats).toEqual([{ player: 2, user: 8192, name: "Guest 1", owner: false }]);
    // the owner's profile carries the host's confirmation
    seats = withSeat(seats, 4, me, true);
    expect(seats.map((s) => [s.player, s.owner])).toEqual([
      [2, false],
      [4, true],
    ]);
    expect(withSeat(seats, 2, null, false).map((s) => s.player)).toEqual([4]);
  });

  it("sends the choices for this console, and says why the engine refused", async () => {
    const calls: Array<[string, unknown]> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        calls.push([url, init?.body ? JSON.parse(String(init.body)) : null]);
        if (url.includes("/api/join/profiles")) return new Response(JSON.stringify(profiles), { status: 200 });
        return new Response(JSON.stringify({ error: "owner_confirm" }), { status: 409 });
      }),
    );
    const seats = [{ player: 2, user: 505721148, name: "Me", owner: false }];
    expect(await joinApi.seats("192.168.1.100", seats)).toEqual({ error: "owner_confirm" });
    expect(calls[0][0]).toMatch(/\/api\/join\/settings$/);
    expect(calls[0][1]).toEqual({ addr: "192.168.1.100", guest_profiles: seats });
    const p = await joinApi.profiles("192.168.1.100");
    expect(calls[1][0]).toMatch(/\/api\/join\/profiles\?addr=192\.168\.1\.100$/);
    expect(p?.users).toHaveLength(2);
    vi.stubGlobal("fetch", vi.fn(async () => new Response("{}", { status: 502 })));
    expect(await joinApi.profiles("192.168.1.100")).toBeNull();
  });
});
