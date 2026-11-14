import type { ReactElement } from "react";
import { renderToStaticMarkup as ssr } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

// The Internet hint's Support link navigates: render inside a router.
const renderToStaticMarkup = (el: ReactElement) => ssr(<MemoryRouter>{el}</MemoryRouter>);

import { installWindowStub } from "../../test-utils/storage";
import { allowTap, askText, PadAsks, padInternet, PhonePadsPanel, playsAsHost, readPadLink, watchUrl } from "./PhonePads";

// The phone link over the internet (pad_link.rs status, POST /api/ps5/rp/pad-link/internet),
// field for field as the engine says it.
const KEY = "0f".repeat(16);
const NET_KEY = "a1".repeat(16);
const LAN_URL = `http://192.168.1.9:19113/pad#t=${KEY}`;
const NET_URL = `https://ps5upload.phantomptr.com/pad/#room=r1&k=${NET_KEY}&fp=AA:BB&hub=hub.phantomptr.com`;
const engine = {
  open: true,
  internet_invites: true,
  internet: {
    url: NET_URL,
    room: "r1",
    hub: "hub.phantomptr.com",
    fp: "AA:BB",
    state: "hosted",
    error: null,
    router: null,
    peers: [
      { gid: "g1", path: "direct", ice: "connected", rtt_ms: 80, since_s: 20, why: null },
      { gid: "g2", path: "connecting", ice: "checking", rtt_ms: null, since_s: 2, why: null },
      { gid: "g3", path: "failed", ice: "failed", rtt_ms: null, since_s: 16, why: "timeout" },
    ],
  },
  url: LAN_URL,
  tailscale: null,
  urls: [LAN_URL],
  pad_ws: "ws://192.168.1.9:19113/api/pad/ws",
  pads: [
    { id: 1, name: "android", since_ms: 0, watch: false, control: false, via: "internet", rtt_ms: 80 },
    { id: 2, name: "iphone", since_ms: 0, watch: true, control: false, via: "internet", rtt_ms: 120 },
    { id: 3, name: "ipad", since_ms: 0, watch: true, control: false, via: "lan", rtt_ms: null },
  ],
  pending: [
    { id: 7, name: "Pixel", via: "internet", watch: true },
    { id: 8, name: "android", via: "internet", watch: false },
  ],
};

afterEach(() => vi.unstubAllGlobals());

const panel = (v: unknown = engine) =>
  renderToStaticMarkup(<PhonePadsPanel consoleHost="192.168.1.100" link={readPadLink(v)} onLink={() => {}} onClose={() => {}} />);

describe("reading the phone link's internet half", () => {
  it("reads the link, the switch, and each phone's way in", () => {
    const s = readPadLink(engine);
    expect(s.internetInvites).toBe(true);
    expect(s.internet).toEqual({ url: NET_URL, hubState: "hosted", hubError: null, offline: false, router: null });
    expect(s.pads.map((p) => p.via ?? "lan")).toEqual(["internet", "internet", "lan"]);
  });

  it("an older engine: no internet half at all", () => {
    const s = readPadLink({ ...engine, internet_invites: undefined, internet: undefined });
    expect(s.internetInvites).toBeNull();
    expect(s.internet).toBeNull();
  });

  it("the watch link of an internet link keeps its fragment", () => {
    expect(watchUrl(NET_URL)).toBe(`${NET_URL}&w=1`);
    expect(new URLSearchParams(watchUrl(NET_URL).split("#")[1]).get("w")).toBe("1");
  });
});

describe("the internet link call", () => {
  function fake(status: number, json: object) {
    const calls: Array<{ url: string; body: unknown }> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        calls.push({ url, body: init?.body ? JSON.parse(String(init.body)) : null });
        return new Response(JSON.stringify(json), { status });
      }),
    );
    return calls;
  }

  it("asks the engine (New link: a new one), then reads the whole state", async () => {
    const calls = fake(200, engine);
    const r = await padInternet("192.168.1.100", true);
    expect(calls[0].url).toMatch(/\/api\/ps5\/rp\/pad-link\/internet$/);
    expect(calls[0].body).toEqual({ console: "192.168.1.100", new: true });
    expect(r.state?.internet?.url).toBe(NET_URL);
    expect(r.error).toBeNull();
  });

  it("says why the engine refused (internet invites off, no usable hub)", async () => {
    fake(409, { error: "internet_off" });
    expect(await padInternet(null)).toEqual({ state: null, error: "internet_off" });
    fake(409, { error: "hub_url" });
    expect((await padInternet(null)).error).toBe("hub_url");
  });
});

describe("the panel's network choice", () => {
  it("Internet picked: the internet link and its QR code, Copy and New link, the hint and the donation line", () => {
    installWindowStub({ "ps5upload.pt_network": "internet" });
    const out = panel();
    expect(out).toContain(`data-testid="rp-pad-url">${NET_URL.replace(/&/g, "&amp;")}<`);
    expect(out).toContain('data-testid="rp-pad-qr"');
    expect(out).toContain('data-testid="rp-pad-copy"');
    expect(out).toContain('data-testid="rp-pad-new-link"');
    expect(out).toContain('data-testid="rp-pad-addresses-internet-hint"');
    expect(out).toContain('data-testid="rp-pad-addresses-internet-donate"');
    // Nothing that is only true on the home network.
    expect(out).not.toContain('data-testid="rp-pad-android-lan"');
    expect(out).not.toContain("In the PS5Upload app on a phone");
  });

  it("This network picked: the LAN link as before, Internet offered beside it", () => {
    installWindowStub({ "ps5upload.pt_network": "home" });
    const out = panel();
    expect(out).toContain(`data-testid="rp-pad-url">${LAN_URL}<`);
    expect(out).toContain('data-testid="rp-pad-addresses-home"');
    expect(out).toContain('data-testid="rp-pad-addresses-internet"');
    expect(out).toContain('data-testid="rp-pad-android-lan"');
    expect(out).not.toContain('data-testid="rp-pad-copy"');
  });

  it("internet invites off: the choice stays, the link waits for the engine's answer", () => {
    installWindowStub({ "ps5upload.pt_network": "internet" });
    const out = panel({ ...engine, internet_invites: false, internet: null });
    expect(out).toContain('data-testid="rp-pad-addresses-internet"');
    expect(out).not.toContain('data-testid="rp-pad-url"');
    expect(out).not.toContain('data-testid="rp-pad-no-address"');
  });

  it("an older engine offers no Internet, whatever was picked last", () => {
    installWindowStub({ "ps5upload.pt_network": "internet" });
    const out = panel({ ...engine, internet_invites: undefined, internet: undefined });
    expect(out).not.toContain("rp-pad-addresses-internet");
    expect(out).toContain(`data-testid="rp-pad-url">${LAN_URL}<`);
  });

  it("internet phones waiting for Allow are listed with Allow and No; connecting and failed ones are said", () => {
    installWindowStub();
    const out = panel();
    expect(out).toContain('data-testid="rp-pad-pending"');
    expect(out.match(/data-testid="rp-pad-pending-allow"/g)).toHaveLength(2);
    expect(out).toContain("Pixel wants to watch · Internet");
    expect(out).toContain("Android phone wants to control your PS5 as you: your profile, saves and trophies");
    expect(out.match(/data-testid="rp-pad-net-peer"/g)).toHaveLength(1);
    expect(out).toContain('data-testid="rp-pad-net-failed"');
    expect(out).toContain("Couldn&#x27;t connect from their network");
  });

  it("the Allow over the picture says what an internet phone wants", () => {
    installWindowStub();
    const s = readPadLink(engine);
    expect(s.pending).toEqual([
      { id: 7, name: "Pixel", watch: true, via: "internet" },
      { id: 8, name: "android", via: "internet" },
    ]);
    const out = renderToStaticMarkup(<PadAsks pending={s.pending} />);
    expect(out).toContain('data-testid="rp-pad-ask"');
    expect(out).toContain("Pixel wants to watch · Internet");
    expect(out).toContain('data-testid="rp-pad-allow"');
  });

  it("an internet phone that would play says it plays as the host, and Allow takes two taps", () => {
    installWindowStub();
    const tr = ((_k: string, _v: unknown, fallback: string) => fallback) as Parameters<typeof askText>[0];
    const net = { id: 8, name: "Pixel", via: "internet" as const };
    const watcher = { id: 7, name: "Pixel", via: "internet" as const, watch: true };
    const lan = { id: 9, name: "Pixel" };
    expect(playsAsHost(net)).toBe(true);
    expect(askText(tr, net)).toBe("Pixel wants to control your PS5 as you: your profile, saves and trophies");
    expect(allowTap(net, false)).toBe("arm");
    expect(allowTap(net, true)).toBe("allow");
    // A watcher from afar and a phone on the LAN are allowed in one tap, as before.
    expect(playsAsHost(watcher) || playsAsHost(lan)).toBe(false);
    expect(allowTap(watcher, false)).toBe("allow");
    expect(allowTap(lan, false)).toBe("allow");
    expect(askText(tr, lan)).toBe("Pixel wants to be a controller");
    const out = renderToStaticMarkup(<PadAsks pending={[net]} />);
    expect(out).toContain("wants to control your PS5 as you");
    expect(out).toContain('data-testid="rp-pad-allow"');
    expect(out).not.toContain("data-armed");
  });

  it("phones from the internet are marked, and a watcher there is never handed the controller", () => {
    installWindowStub();
    const out = panel();
    expect(out.match(/data-testid="rp-pad-via-internet"/g)).toHaveLength(2);
    // Only the LAN watcher has the button.
    expect(out.match(/data-testid="rp-watch-control"/g)).toHaveLength(1);
  });
});
