import { describe, expect, it } from "vitest";

import { advancedBody, ownPagesNeedOwnHub, pageBaseOf, pageOrigin, readBuiltInHub, readJoinState, readSources } from "./playTogetherState";

// Settings › Play Together › Advanced on a self-hosted engine (#881): the guest page address, where
// each value comes from, and what Save sends.

describe("the guest page address", () => {
  it("takes what the engine takes, in its one form", () => {
    expect(pageBaseOf("https://play.example.com")).toBe("https://play.example.com/");
    expect(pageBaseOf(" HTTPS://Play.Example.com:443/ ")).toBe("https://play.example.com/");
    expect(pageBaseOf("https://play.example.com:8443/ps5//")).toBe("https://play.example.com:8443/ps5/");
    expect(pageBaseOf("http://localhost:8080")).toBe("http://localhost:8080/");
    expect(pageBaseOf("http://127.0.0.1/pages")).toBe("http://127.0.0.1/pages/");
    for (const bad of [
      "",
      "play.example.com",
      "http://play.example.com/",
      "wss://play.example.com/",
      "https://play.example.com/?a=1",
      "https://play.example.com/#x",
      "https://user@play.example.com/",
      "https://play.example.com/a/../b",
      "https://play.example.com/a b",
    ]) {
      expect(pageBaseOf(bad), bad).toBeNull();
    }
    expect(pageOrigin("https://play.example.com/ps5/")).toBe("https://play.example.com");
  });

  it("warns when the pages are the host's own but the hub is the default one", () => {
    expect(ownPagesNeedOwnHub("https://play.example.com/", "wss://hub.phantomptr.com/ws")).toBe(true);
    expect(ownPagesNeedOwnHub("https://play.example.com/", "wss://play.example.com/ws")).toBe(false);
    expect(ownPagesNeedOwnHub("https://ps5upload.phantomptr.com/", "wss://hub.phantomptr.com/ws")).toBe(false);
    // The website's pages with the host's own hub work (a hub allows the website's origin by default).
    expect(ownPagesNeedOwnHub("https://ps5upload.phantomptr.com/", "wss://play.example.com/ws")).toBe(false);
  });
});

describe("reading the settings", () => {
  it("reads the page base and the sources, and an older engine's as unknown", () => {
    const s = readJoinState({
      settings: {
        internet_invites: true,
        hub_url: "wss://play.example.com/ws",
        page_base: "https://play.example.com/",
        stun: [],
        sources: { hub_url: "environment", page_base: "custom", stun: "odd" },
      },
    });
    expect(s.settings.pageBase).toBe("https://play.example.com/");
    expect(s.settings.sources).toEqual({ hubUrl: "environment", pageBase: "custom", stun: "default" });
    const old = readJoinState({ settings: { internet_invites: true, hub_url: "wss://hub.phantomptr.com/ws" } });
    expect(old.settings.pageBase).toBeNull();
    expect(old.settings.sources).toBeNull();
    expect(readSources("x")).toBeNull();
  });

  it("reads the engine's own hub, shown only when it runs one", () => {
    const on = readJoinState({ settings: { built_in_hub: { port: 19115, public_url: "https://play.example.com/", stun_ports: [] } } });
    expect(on.settings.builtInHub).toEqual({ port: 19115, publicUrl: "https://play.example.com/" });
    expect(readBuiltInHub({ port: 19115, public_url: null })).toEqual({ port: 19115, publicUrl: null });
    expect(readJoinState({ settings: { built_in_hub: null } }).settings.builtInHub).toBeNull();
    expect(readJoinState({ settings: {} }).settings.builtInHub).toBeNull();
  });
});

describe("what Save sends", () => {
  const net = {
    hubUrl: "wss://hub.phantomptr.com/ws",
    pageBase: "https://ps5upload.phantomptr.com/",
    stun: ["stun.cloudflare.com:3478"],
  };
  const typed = { hub: net.hubUrl, pageBase: net.pageBase, stun: net.stun.join("\n") };

  it("nothing when nothing changed", () => {
    expect(advancedBody(net, typed)).toBeNull();
  });

  it("only what changed, the page base in its one form", () => {
    expect(advancedBody(net, { ...typed, pageBase: "https://Play.Example.com" })).toEqual({
      page_base: "https://play.example.com/",
    });
    expect(advancedBody(net, { ...typed, hub: " wss://play.example.com/ws ", stun: "" })).toEqual({
      hub_url: "wss://play.example.com/ws",
      stun: [],
    });
  });

  it("an emptied hub or page base goes back to the default", () => {
    expect(advancedBody({ ...net, pageBase: "https://play.example.com/" }, { ...typed, pageBase: "", hub: "" })).toEqual({
      hub_url: "",
      page_base: "",
    });
  });

  it("nothing for a value the engine would refuse", () => {
    expect(advancedBody(net, { ...typed, pageBase: "http://play.example.com/" })).toBeNull();
    expect(advancedBody(net, { ...typed, hub: "https://hub.example.com/ws" })).toBeNull();
    expect(advancedBody(net, { ...typed, stun: "no port" })).toBeNull();
  });

  it("no page base for an engine from before it", () => {
    expect(advancedBody({ ...net, pageBase: null }, { ...typed, pageBase: "https://play.example.com/" })).toBeNull();
  });
});
