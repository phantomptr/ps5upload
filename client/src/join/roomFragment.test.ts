import { describe, expect, it } from "vitest";

import { joinAppLink, joinLinkFromAppLink, parseJoinLink } from "../lib/joinProtocol";
import { joinUrl, onPs5Browser } from "../rooms/RoomsPage";
import { parseRoomFragment, roomFragment } from "./roomFragment";

const LOBBY = "AbCdEfGhIjKlMnOpQrStUh";

describe("a room in the join page's fragment", () => {
  it("round-trips and refuses what could point anywhere else", () => {
    const f = { lobby: LOBBY, hub: "hub.phantomptr.com", host: "Sam", tag: "4F2A", pw: true, via: "rooms" as const };
    expect(parseRoomFragment(roomFragment(f))).toEqual(f);
    expect(parseRoomFragment(`#lobby=${LOBBY}&hub=127.0.0.1:8787`)).toMatchObject({ hub: "127.0.0.1:8787", pw: false, via: "rooms", host: null });
    for (const bad of [`#lobby=short&hub=hub.example`, `#lobby=${LOBBY}&hub=https://evil.example`, `#lobby=${LOBBY}&hub=evil.example/x`, `#lobby=${LOBBY}&hub=u@evil`, `#hub=hub.example`]) {
      expect(parseRoomFragment(bad), bad).toBeNull();
    }
    expect(parseRoomFragment(`#lobby=${LOBBY}&hub=h.example&via=weird`)?.via).toBe("rooms");
  });

  it("the website's card goes to /join/ with it; the app opens the same link and knocks", () => {
    const url = joinUrl({ room: LOBBY, host: "Sam", tag: "4F2A", pw: true }, "hub.phantomptr.com");
    expect(url.startsWith("https://ps5upload.phantomptr.com/join/#lobby=")).toBe(true);
    const link = parseJoinLink(url);
    expect(link).toMatchObject({ invite: null, name: "Sam", room: { lobby: LOBBY, hub: "hub.phantomptr.com", via: "rooms", tag: "4F2A" } });
    expect(joinLinkFromAppLink(joinAppLink(url))).toBe(url);
  });

  it("a PS5's browser is told to use PS5Upload Play", () => {
    expect(onPs5Browser("Mozilla/5.0 (PlayStation 5 7.00) AppleWebKit/605.1.15")).toBe(true);
    expect(onPs5Browser("Mozilla/5.0 (iPhone)")).toBe(false);
  });
});
