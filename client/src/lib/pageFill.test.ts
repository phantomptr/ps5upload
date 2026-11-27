import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { createElement } from "react";
import { HOME_TIP_KEY, HomeScreenTip, fillWithoutApi, showsFullButton, sidewaysNow, turnStep } from "./pageFill";

const IPHONE = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 CriOS/131.0 Mobile/15E148";

function stubPage(ua: string, standalone = false) {
  const store = new Map<string, string>();
  vi.stubGlobal("navigator", { userAgent: ua, maxTouchPoints: 5, standalone });
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
  });
  vi.stubGlobal("window", { scrollTo: () => {}, matchMedia: () => ({ matches: false }), localStorage: globalThis.localStorage });
  return store;
}

afterEach(() => vi.unstubAllGlobals());

describe("Full screen without the Fullscreen API", () => {
  it("shows the button in a browser without the API", () => {
    expect(showsFullButton(false, false, false)).toBe(true);
    expect(showsFullButton(false, false, true)).toBe(true);
    expect(showsFullButton(true, true, false)).toBe(true);
    // Already in the full layout with nothing the browser can undo.
    expect(showsFullButton(false, true, false)).toBe(false);
  });

  it("asks for the Home Screen tip once on an iPhone", () => {
    const store = stubPage(IPHONE);
    expect(fillWithoutApi()).toBe(true);
    store.set(HOME_TIP_KEY, "1"); // what Got it saves
    expect(fillWithoutApi()).toBe(false);
  });

  it("no tip from the Home Screen or off Apple", () => {
    stubPage(IPHONE, true);
    expect(fillWithoutApi()).toBe(false);
    stubPage("Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Firefox/130.0");
    expect(fillWithoutApi()).toBe(false);
  });

  it("the tip says how", () => {
    const html = renderToStaticMarkup(createElement(HomeScreenTip, { onClose: () => {} }));
    expect(html).toContain("Add to Home Screen");
    expect(html).toContain('data-testid="home-screen-tip-close"');
  });
});

describe("A phone turned sideways in a browser", () => {
  it("goes full on a turn sideways and leaves on a turn upright", () => {
    expect(turnStep(true, false)).toBe("enter");
    expect(turnStep(false, true)).toBe("leave");
    // Already that way: nothing (left full screen sideways stays left until the next turn).
    expect(turnStep(true, true)).toBeNull();
    expect(turnStep(false, false)).toBeNull();
  });

  it("reads sideways from the screen, the older orientation, else the window's shape", () => {
    vi.stubGlobal("window", { screen: { orientation: { type: "landscape-primary" } }, innerWidth: 400, innerHeight: 800 });
    expect(sidewaysNow()).toBe(true);
    vi.stubGlobal("window", { screen: { orientation: { type: "portrait-primary" } }, innerWidth: 800, innerHeight: 400 });
    expect(sidewaysNow()).toBe(false);
    vi.stubGlobal("window", { screen: {}, orientation: -90, innerWidth: 400, innerHeight: 800 });
    expect(sidewaysNow()).toBe(true);
    vi.stubGlobal("window", { screen: {}, innerWidth: 800, innerHeight: 400 });
    expect(sidewaysNow()).toBe(true);
    vi.stubGlobal("window", { screen: {}, innerWidth: 400, innerHeight: 800 });
    expect(sidewaysNow()).toBe(false);
  });
});
