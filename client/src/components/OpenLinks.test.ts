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

  it("round-trips an internet invite (12.1's one link) through the app link", () => {
    const net = `https://ps5upload.phantomptr.com/join/#room=Demo0Room0Not0Real0000&k=abcdefgh12&fp=${"AB:".repeat(31)}CD&hub=hub.example.com&n=Sam`;
    const app = joinAppLink(net);
    expect(joinLinkFromAppLink(app)).toBe(net);
    expect(openLinkRoute(app)).toBe(`/play?join=${encodeURIComponent(net)}`);
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

describe("ps5upload:// controller links", () => {
  const pad = "http://192.168.1.20:19113/pad#t=0f0f0f0f0f0f0f0f";

  it("route to this device as the controller, through the relay", () => {
    const r = openLinkRoute(`ps5upload://pad?link=${encodeURIComponent(pad)}`)!;
    const q = new URL(r, "http://x").searchParams;
    expect(r.startsWith("/play/pad?")).toBe(true);
    expect(q.get("url")).toBe("ws://192.168.1.20:19113/api/pad/ws");
    expect(q.get("name")).toBe("192.168.1.20");
    expect(q.get("t")).toBe("0f0f0f0f0f0f0f0f");
    expect(q.get("w")).toBeNull();
  });

  it("keep the path an engine is served under", () => {
    const prefixed = "http://192.168.1.20:19113/ps5/pad#t=0f0f0f0f0f0f0f0f";
    const r = openLinkRoute(`ps5upload://pad?link=${encodeURIComponent(prefixed)}`)!;
    expect(new URL(r, "http://x").searchParams.get("url")).toBe(
      "ws://192.168.1.20:19113/ps5/api/pad/ws",
    );
  });

  it("keep watch links watching", () => {
    const r = openLinkRoute(`ps5upload://pad?link=${encodeURIComponent(`${pad}&w=1`)}`)!;
    expect(new URL(r, "http://x").searchParams.get("w")).toBe("1");
  });

  it("refuse what isn't a controller link", () => {
    for (const bad of [
      "http://192.168.1.20:19113/pad",
      "http://192.168.1.20:19113/pad#t=xyz",
      "http://192.168.1.20:19113/other#t=0f0f0f0f0f0f0f0f",
      "javascript:alert(1)//pad#t=0f0f0f0f0f0f0f0f",
      "http://u:p@192.168.1.20/pad#t=0f0f0f0f0f0f0f0f",
    ]) {
      expect(openLinkRoute(`ps5upload://pad?link=${encodeURIComponent(bad)}`)).toBeNull();
    }
  });
});

describe("ps5upload:// receive links", () => {
  const share = "https://ps5upload.phantomptr.com/share/#room=AbCdEfGhIjKlMnOpQrStUv&fp=AA:BB&hub=hub.phantomptr.com";
  it("opens Share with the Share link", async () => {
    const { receiveAppLink } = await import("../lib/appLinks");
    expect(openLinkRoute(receiveAppLink(share))).toBe(`/share?receive=${encodeURIComponent(share)}`);
  });
  it("refuses anything that isn't a Share link", () => {
    expect(openLinkRoute("ps5upload://receive?link=https%3A%2F%2Fevil%2Fx")).toBeNull();
    expect(openLinkRoute("ps5upload://receive?link=javascript%3Aalert(1)")).toBeNull();
    expect(openLinkRoute("ps5upload://receive")).toBeNull();
  });
});
