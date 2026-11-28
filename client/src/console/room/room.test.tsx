import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it } from "vitest";
import { t } from "../strings";
import { ConfirmDialog } from "../ui/ConfirmDialog";
import { parseRoom, pingBars, playerNames, resetRoomStore, roomWarning, type Room } from "./room";
import { RoomCardView } from "./RoomCard";
import { RoomRules } from "./RoomRules";

/** /api/room's "room" as the helper writes it (payload/src/play_host.c). */
const RAW = {
  engine_id: "01".repeat(32),
  hosted_from: "Desk-PC",
  kind: "desktop",
  online: true,
  name: "Sam's room",
  visibility: "public",
  locked: true,
  game: { name: "ASTRO BOT", title_id: "PPSA01234" },
  size: 4,
  playing: 2,
  asks: 1,
  chat: true,
  voice: false,
  players: [
    { name: "Mia", ping_ms: 42, seat: 2, bars: 3, via: "browser" },
    { name: "Lee", ping_ms: null, seat: null, bars: null, via: "ps5app" },
  ],
  age_s: 3,
};

const room = (over: Partial<Room> = {}): Room => ({ ...(parseRoom(RAW) as Room), ...over });

afterEach(() => resetRoomStore(null));

describe("parseRoom", () => {
  it("reads the helper's answer", () => {
    const r = parseRoom(RAW)!;
    expect(r.name).toBe("Sam's room");
    expect(r.visibility).toBe("public");
    expect(r.game).toEqual({ name: "ASTRO BOT", title_id: "PPSA01234" });
    expect(r.players).toHaveLength(2);
    expect(r.players[1]).toEqual({ name: "Lee", ping_ms: null, seat: null, bars: null, via: "ps5app" });
    expect(r.online).toBe(true);
  });
  it("is null for no room, an older helper or junk", () => {
    expect(parseRoom(null)).toBeNull();
    expect(parseRoom(undefined)).toBeNull();
    expect(parseRoom({})).toBeNull();
    expect(parseRoom("room")).toBeNull();
  });
  it("is forgiving about odd fields", () => {
    const r = parseRoom({ name: "x", visibility: "everyone", players: [{ name: "" }, 7, { name: "A", ping_ms: "9" }], playing: 0 })!;
    expect(r.visibility).toBe("private");
    expect(r.players).toEqual([{ name: "A", ping_ms: null, seat: null, bars: null, via: "" }]);
    expect(r.playing).toBe(1); /* never fewer than listed */
    expect(r.game).toBeNull();
  });
});

describe("pingBars", () => {
  it("uses the engine's tier, else the design's round-trip lines", () => {
    expect(pingBars({ bars: 2, ping_ms: 5 })).toBe(2);
    expect(pingBars({ bars: null, ping_ms: 20 })).toBe(4);
    expect(pingBars({ bars: null, ping_ms: 40 })).toBe(3);
    expect(pingBars({ bars: null, ping_ms: 80 })).toBe(2);
    expect(pingBars({ bars: null, ping_ms: 81 })).toBe(1);
    expect(pingBars({ bars: null, ping_ms: null })).toBe(0);
    expect(pingBars({ bars: 9, ping_ms: null })).toBe(4);
  });
});

describe("the players' names", () => {
  it("lists a few and counts the rest", () => {
    expect(playerNames(room())).toBe("Mia, Lee");
    const many = room({
      players: ["A", "B", "C", "D"].map((name) => ({ name, ping_ms: null, seat: null, bars: null, via: "" })),
      playing: 6,
    });
    expect(playerNames(many)).toBe(t("cw_room_and_more", { names: "A, B, C", n: 3 }));
  });
  it("warns with the names and the host device, or only the count", () => {
    expect(roomWarning(null)).toBeNull();
    expect(roomWarning(room({ playing: 0, players: [] }))).toBeNull();
    const w = roomWarning(room())!;
    expect(w).toContain("Mia, Lee");
    expect(w).toContain("Desk-PC");
    expect(roomWarning(room({ players: [], playing: 2, hosted_from: "" }))).toBe(t("cw_room_warn_count", { n: 2 }));
  });
});

describe("RoomCardView", () => {
  it("shows the room, the players with bars and words, and where it is hosted from", () => {
    const html = renderToStaticMarkup(<RoomCardView room={room()} onOpenPlay={() => {}} onOpenRules={() => {}} />);
    expect(html).toContain("Sam&#x27;s room");
    expect(html).toContain("ASTRO BOT · " + t("cw_room_vis_public") + " · " + t("cw_room_count", { n: 2, size: 4 }));
    expect(html).toContain(t("cw_room_bars_3"));
    expect(html).toContain(t("cw_room_ping", { ms: 42 }));
    expect(html).toContain(t("cw_room_ping_none")); /* Lee: no reading yet */
    expect(html).toContain(t("cw_room_asks", { n: 1 }));
    expect(html).toContain(t("cw_room_hosted_from", { device: "Desk-PC" }));
    expect(html).toContain('data-action="room-open-play"');
    expect(html).toContain('data-action="room-rules"');
    expect(html).toContain(t("cw_room_locked"));
  });
  it("says when the host device is away, and when nobody joined yet", () => {
    const html = renderToStaticMarkup(<RoomCardView room={room({ online: false, players: [], playing: 0, size: null })} />);
    expect(html).toContain(t("cw_room_host_away", { device: "Desk-PC" }));
    expect(html).toContain(t("cw_room_no_players"));
    expect(html).toContain(t("cw_room_count_n", { n: 0 }));
    expect(html).not.toContain("room-open-play");
  });
});

describe("the confirmations", () => {
  it("name the players of a room here when the action ends the game", () => {
    resetRoomStore(room());
    const html = renderToStaticMarkup(
      <ConfirmDialog title={t("cw_home_close_game")} confirmLabel={t("cw_home_close_game")} roomNotice onConfirm={() => {}} onCancel={() => {}} />,
    );
    expect(html).toContain("data-room-warning");
    expect(html).toContain("Mia, Lee");
  });
  it("say nothing more with no room, or when the action does not end the game", () => {
    resetRoomStore(null);
    expect(renderToStaticMarkup(<ConfirmDialog title="x" confirmLabel="y" roomNotice onConfirm={() => {}} onCancel={() => {}} />)).not.toContain(
      "data-room-warning",
    );
    resetRoomStore(room());
    expect(renderToStaticMarkup(<ConfirmDialog title="x" confirmLabel="y" onConfirm={() => {}} onCancel={() => {}} />)).not.toContain(
      "data-room-warning",
    );
  });
});

describe("RoomRules", () => {
  it("shows the responsibility notice and the disclaimer", () => {
    const html = renderToStaticMarkup(<RoomRules onBack={() => {}} />);
    expect(html).toContain(t("cw_room_notice_title").replace(/'/g, "&#x27;"));
    expect(html).toContain(t("cw_room_notice_parents"));
    expect(html).toContain(t("cw_about_disclaimer_title"));
  });
});
