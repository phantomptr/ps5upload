import type { ReactElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { installWindowStub } from "../../../test-utils/storage";
import { RoomCardView } from "../../../components/RoomCardView";
import { RoomChatBox } from "../../../components/RoomChatBox";
import { PingBars } from "../../../components/PingBars";
import { KnockForm } from "../../../components/RoomsBrowserCore";
import { readJoinState } from "../playTogetherState";
import { playerActions } from "./PlayerMenu";
import { closeQuestion, listedText, RoomHeader } from "./RoomHeader";
import { ApprovalFields, createBody } from "./CreateRoom";
import { refusalText } from "./useRoomActions";
import type { RoomCard } from "../../../lib/room/rooms";

const html = (el: ReactElement) => renderToStaticMarkup(<MemoryRouter>{el}</MemoryRouter>);
const tr = ((_k: string, _v: Record<string, unknown> | undefined, f: string) => f) as never;
afterEach(() => vi.unstubAllGlobals());

const card: RoomCard = {
  room: "A".repeat(22),
  name: "Couch chaos",
  host: "Sam",
  tag: "4f2a",
  game: { title: "It Takes Two", id: "PPSA01234" },
  proto: 2,
  ver: "13.0.0",
  caps: [],
  inRoom: 1,
  size: 2,
  pw: true,
  lang: "en",
  region: "eu-west",
  estMs: 70,
  ageS: 0,
  pk: null,
};

describe("Play Room screens", () => {
  it("a room card: three lines of facts, lock, about-ping, and its button", () => {
    installWindowStub();
    const out = html(<RoomCardView card={card} expected={{ ms: 70, kind: "about", tier: "playable" }} action="ask" onAction={() => {}} onDetails={() => {}} />);
    expect(out).toContain("Couch chaos");
    expect(out).toContain("Sam 4F2A");
    expect(out).toContain("It Takes Two · 1/2 · English · Europe West");
    expect(out).toContain('data-testid="room-card-lock"');
    expect(out).toContain("about 70 ms");
    expect(out).toContain("Ask to join");
    const full = html(<RoomCardView card={{ ...card, inRoom: 2 }} expected={null} action="full" onAction={() => {}} />);
    expect(full).toMatch(/disabled=""[^>]*data-testid="room-card-action"/);
    const old = html(<RoomCardView card={{ ...card, proto: 5 }} expected={null} action="update" onAction={() => {}} />);
    expect(old).toContain("Update to join (needs PS5Upload 16.0.0 or newer)");
  });

  it("ping bars: bars, word and ms; hollow for an estimate", () => {
    installWindowStub();
    const live = html(<PingBars tier="good" ms={34} />);
    expect(live).toContain('data-bars="3"');
    expect(live).toContain("34 ms");
    expect(html(<PingBars tier="playable" ms={70} estimate />)).toContain('data-kind="estimate"');
    expect(html(<PingBars tier={null} ms={null} />)).toContain("—");
  });

  it("chat box: quick messages always, typed text only when on, muted and off say so", () => {
    installWindowStub();
    const props = { lines: [{ n: 1, from: "Mia", mine: false, quick: 2, text: null, times: 3 }], onSend: () => {}, ensureNotice: async () => true };
    const on = html(<RoomChatBox {...props} mode="on" />);
    expect(on).toContain("Ready!");
    expect(on).toContain("×3");
    expect(on).toContain('data-testid="room-chat-input"');
    expect(html(<RoomChatBox {...props} mode="quick" />)).not.toContain("room-chat-input");
    expect(html(<RoomChatBox {...props} mode="off" />)).toContain("Chat is off");
    expect(html(<RoomChatBox {...props} mode="on" mutedByHost />)).toContain("The host muted chat for you");
  });

  it("the knock form asks the password only for a locked room", () => {
    installWindowStub();
    expect(html(<KnockForm card={{ name: "Couch", pw: true }} dark={false} defaultName="Jo" onGo={() => {}} />)).toContain("room-knock-password");
    expect(html(<KnockForm card={{ name: "Couch", pw: false }} dark={false} defaultName="Jo" onGo={() => {}} />)).not.toContain("room-knock-password");
  });

  it("the header: chips, players, listed state; close names who leaves", () => {
    installWindowStub();
    const state = readJoinState({
      open: true,
      guests: [{ id: 1, name: "Mia", player: 2, state: "playing" }],
      room: { name: "Couch chaos", visibility: "public", size: 2, password: true, lock: true, listed: { state: "listed" }, open_for_s: 600 },
    });
    const actions = {} as never;
    const out = html(<RoomHeader state={state} actions={actions} onClose={() => {}} onRules={() => {}} lang="en" now={Date.now()} />);
    expect(out).toContain("Your room · Couch chaos");
    expect(out).toContain('data-testid="room-chip-password"');
    expect(out).toContain('data-testid="room-chip-locked"');
    expect(out).toContain("Full (2 of 2)");
    expect(out).toContain("No one new can ask");
    expect(listedText(tr, { ...state.room, locked: false })).toBe("Listed in Rooms");
    expect(closeQuestion(tr, ["Sam", "Mia"], "en")).toBe("Close your room? Sam and Mia leave the game.");
    expect(closeQuestion(tr, ["Mia"], "en")).toBe("Close your room? Mia leaves the game.");
    expect(closeQuestion(tr, [], "en")).toBe("Close your room?");
  });

  it("player menu: free seats, Player 1 never for strangers, mutes, remove", () => {
    const g = { player: 2, status: "playing" as const, streamSeat: false, room: { source: "rooms" as const, tag: null, muted: { voice: false, chat: true }, fromRooms: true, speaking: false, ready: false, joinedAt: null, client: null } };
    const o = { size: 4, taken: [2, 3], streamSeatAllowed: true, pictureHere: false, voice: true, chat: true };
    expect(playerActions(g, o)).toEqual([{ kind: "move", to: 4 }, { kind: "mute_voice", on: true }, { kind: "mute_chat", on: false }, { kind: "remove" }]);
    const friend = { ...g, room: { ...g.room, fromRooms: false, source: "invite" as const } };
    expect(playerActions(friend, o).some((a) => a.kind === "give1")).toBe(true);
    expect(playerActions(friend, { ...o, pictureHere: true }).some((a) => a.kind === "give1")).toBe(false);
    expect(playerActions({ ...friend, player: 1, streamSeat: true }, o).some((a) => a.kind === "take1")).toBe(true);
    expect(playerActions({ ...g, status: "left" as const }, o)).toEqual([]);
  });

  it("create sends only what was set, the password only when long enough", () => {
    expect(createBody({ name: "  ", password: "abc", visibility: "private" })).toEqual({ visibility: "private" });
    expect(createBody({ name: " Couch ", password: "abcd", visibility: "public", approval: { friends: true, password: true } })).toEqual({
      name: "Couch",
      password: "abcd",
      visibility: "public",
      public_ok: true,
      approval: { friends: true, password: true },
    });
    expect(createBody({ approval: { friends: false, password: true } }).approval).toEqual({ friends: false, password: false });
    expect(createBody({ approval: { friends: true, password: false, household: true } }).approval).toEqual({
      friends: true,
      password: false,
      household: true,
    });
    expect(refusalText(tr, "size_below_players", 3)).toBe("3 people are in; remove someone first.");
  });

  it("approvals offer the household switch (engine approval.household)", () => {
    const out = html(<ApprovalFields approval={{ friends: true, password: false, household: true }} hasPassword={false} onChange={() => {}} />);
    expect(out).toContain("Your own PS5s (the ones PS5Upload knows) get in without asking");
  });
});
