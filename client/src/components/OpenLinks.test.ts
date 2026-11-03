import { describe, expect, it } from "vitest";
import { openLinkRoute } from "./OpenLinks";
import { joinAppLink, joinLinkFromAppLink } from "../lib/joinProtocol";

const invite = "http://192.168.1.20:19113/join#i=abcdefgh12&n=Sam";

describe("ps5upload:// join links", () => {
  it("round-trips an invite through the app link", () => {
    const app = joinAppLink(invite);
    expect(app.startsWith("ps5upload://join?link=")).toBe(true);
    expect(joinLinkFromAppLink(app)).toBe(invite);
    expect(openLinkRoute(app)).toBe(`/play?join=${encodeURIComponent(invite)}`);
  });

  it("takes the path form and ignores case in the action", () => {
    expect(joinLinkFromAppLink(`ps5upload:/join?link=${encodeURIComponent(invite)}`)).toBe(invite);
    expect(joinLinkFromAppLink(`ps5upload://JOIN/?link=${encodeURIComponent(invite)}`)).toBe(invite);
  });

  it("refuses anything that isn't a join invite", () => {
    expect(joinLinkFromAppLink(`https://x/join?link=${encodeURIComponent(invite)}`)).toBeNull();
    expect(joinLinkFromAppLink(`ps5upload://send?link=${encodeURIComponent(invite)}`)).toBeNull();
    expect(joinLinkFromAppLink("ps5upload://join?link=http%3A%2F%2Fevil%2Fother")).toBeNull();
    expect(joinLinkFromAppLink("ps5upload://join")).toBeNull();
    expect(joinLinkFromAppLink("not a url")).toBeNull();
    expect(openLinkRoute("ps5upload://join?link=javascript%3Aalert(1)")).toBeNull();
  });
});
