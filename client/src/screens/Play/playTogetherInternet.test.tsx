import type { ReactElement } from "react";
import { renderToStaticMarkup as ssr } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

// The Internet hint's Support link navigates: render inside a router.
const renderToStaticMarkup = (el: ReactElement) => ssr(<MemoryRouter>{el}</MemoryRouter>);

import { installWindowStub } from "../../test-utils/storage";
import { NetworkChoice, savedNetwork, saveNetwork } from "./NetworkChoice";
import { guestWayText, netInviteNote, netPeersView, PlayTogetherPanel } from "./PlayTogether";
import { hubUrlOk, invitesBody, joinApi, parseStun, readJoinState, shareOf } from "./playTogetherState";

// Internet invites (join_net): what the engine says (join.rs state_json, hub.rs Room::json),
// field for field.
const KEY = "0f".repeat(16);
const NET_URL = `https://ps5upload.phantomptr.com/join/#room=r1&k=${KEY}&fp=AA:BB&hub=hub.phantomptr.com&lan=http%3A%2F%2F192.168.1.9%3A19113%2Fjoin`;
const engine = {
  open: true,
  console: "192.168.1.100",
  invite: {
    key: KEY,
    code: "4821",
    url: `http://192.168.1.9:19113/join#i=${KEY}`,
    urls: [`http://192.168.1.9:19113/join#i=${KEY}`],
    expires_in_s: 1800,
    share: { url: NET_URL, works: "anywhere", why: null },
    internet: { url: NET_URL, room: "r1", hub: "hub.phantomptr.com", fp: "AA:BB", state: "hosted", error: null, offline: false, router: null, peers: [] },
  },
  guests: [
    { id: 5, name: "Ann", player: 2, state: "playing", connected: true, rtt_ms: 48, via: "internet", path: "direct", ice_rtt_ms: 40, link: { path: "lan", rtt_ms: 4 } },
    { id: 6, name: "Ben", player: 3, state: "playing", connected: true, rtt_ms: 9, via: "lan", path: null, ice_rtt_ms: null, link: { path: "lan", rtt_ms: 9 } },
    { id: 7, name: "Cy", player: 4, state: "playing", connected: true, rtt_ms: 61, via: "internet", path: "direct", ice_rtt_ms: 55, link: { path: "internet", rtt_ms: 55 } },
  ],
  pending: [],
  settings: {
    block_system_buttons: true,
    max_players: 3,
    stream_seat: false,
    internet_invites: true,
    invites: "anywhere",
    hub_url: "wss://hub.phantomptr.com/ws",
    stun: ["stun.cloudflare.com:3478", "stun.l.google.com:19302"],
    upnp: true,
  },
  internet: {
    room: "r1",
    peers: [
      { gid: "g1", path: "direct", ice: "connected", rtt_ms: 40, since_s: 20, why: null },
      { gid: "g2", path: "connecting", ice: "checking", rtt_ms: null, since_s: 2, why: null },
      { gid: "g3", path: "failed", ice: "failed", rtt_ms: null, since_s: 16, why: "no_direct" },
    ],
  },
};

// English, as tr falls back without a language loaded.
const tr = ((_k: string, vars?: Record<string, string | number> | string, fallback?: string) =>
  typeof vars === "string" ? vars : (fallback ?? _k)) as Parameters<typeof netInviteNote>[0];

afterEach(() => vi.unstubAllGlobals());

describe("reading the engine's internet invites", () => {
  it("reads the link, the hub, each guest's way in and the peers", () => {
    const s = readJoinState(engine);
    expect(s.invite?.internet).toEqual({ url: NET_URL, hubState: "hosted", hubError: null, offline: false });
    expect(s.invite?.share).toEqual({ url: NET_URL, works: "anywhere", why: null });
    expect(s.guests.map((g) => [g.name, g.via, g.path, g.netPath, g.rttMs])).toEqual([
      ["Ann", "internet", "direct", "lan", 48],
      ["Ben", "lan", null, "lan", 9],
      ["Cy", "internet", "direct", "internet", 61],
    ]);
    expect(s.netPeers.map((p) => p.path)).toEqual(["direct", "connecting", "failed"]);
    expect(s.settings).toMatchObject({ internetInvites: true, invites: "anywhere", hubUrl: "wss://hub.phantomptr.com/ws", upnp: true });
    expect(s.settings.stun).toHaveLength(2);
  });

  it("an internet guest with no own round trip yet shows ICE's", () => {
    const s = readJoinState({ guests: [{ id: 1, player: 2, via: "internet", path: "direct", rtt_ms: null, ice_rtt_ms: 31 }] });
    expect(s.guests[0].rttMs).toBe(31);
  });

  it("an older engine: no internet choice, every guest on the home network", () => {
    const s = readJoinState({ open: true, settings: { block_system_buttons: true }, guests: [{ id: 1, player: 2 }] });
    expect(s.settings.internetInvites).toBeNull();
    expect(s.settings.invites).toBeNull();
    expect(s.guests[0].via).toBe("lan");
    expect(s.guests[0].netPath).toBeNull();
    expect(s.netPeers).toEqual([]);
    expect(readJoinState({ invite: { url: "http://x/join#i=k" } }).invite?.internet).toBeNull();
  });
});

describe("the internet invite call", () => {
  function fake(status: number, json: object) {
    const calls: Array<{ url: string; body: unknown }> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        calls.push({ url, body: init?.body ? JSON.parse(String(init.body)) : null });
        return url.includes("/api/join?")
          ? new Response(JSON.stringify(engine), { status: 200 })
          : new Response(JSON.stringify(json), { status });
      }),
    );
    return calls;
  }

  it("asks with `internet`, then reads the whole state", async () => {
    const calls = fake(200, { key: KEY });
    const r = await joinApi.inviteInternet("192.168.1.100", true);
    expect(calls[0].body).toEqual({ addr: "192.168.1.100", new: true, internet: true });
    expect(r.state?.invite?.internet?.url).toBe(NET_URL);
  });

  it("says why the engine refused (off in the settings, no usable hub)", async () => {
    fake(409, { error: "internet_off" });
    expect(await joinApi.inviteInternet("192.168.1.100")).toEqual({ error: "internet_off" });
    fake(409, { error: "hub_url" });
    expect(await joinApi.inviteInternet("192.168.1.100")).toEqual({ error: "hub_url" });
  });

  it("an engine from before the one link: internet_invites says where invites work, the link is the internet one when made", () => {
    const s = readJoinState({ ...engine, settings: { ...engine.settings, invites: undefined, internet_invites: false } });
    expect(s.settings.invites).toBe("wifi");
    const old = readJoinState({ ...engine, invite: { ...engine.invite, share: undefined } });
    expect(old.invite?.share).toBeNull();
    expect(shareOf(old.invite!)).toEqual({ url: NET_URL, works: "anywhere", why: null });
    const lan = readJoinState({ ...engine, invite: { ...engine.invite, share: undefined, internet: null } });
    expect(shareOf(lan.invite!)).toEqual({ url: engine.invite.url, works: "wifi", why: "local" });
  });

  it("the one link asks with `share`, never `internet`", async () => {
    const calls = fake(200, { key: KEY });
    const r = await joinApi.inviteShare("192.168.1.100", true);
    expect(calls[0].body).toEqual({ addr: "192.168.1.100", new: true, share: true });
    expect(r?.invite?.share?.url).toBe(NET_URL);
  });

  it("an engine from before the one link (no `share` in its answer): asked for its internet link", async () => {
    const old = { ...engine, invite: { ...engine.invite, share: undefined, internet: null } };
    const calls: unknown[] = [];
    let asked = false;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        const body = init?.body ? (JSON.parse(String(init.body)) as Record<string, unknown>) : null;
        if (body) calls.push(body);
        if (body?.internet) asked = true;
        const whole = asked ? { ...old, invite: { ...old.invite, internet: engine.invite.internet } } : old;
        return new Response(JSON.stringify(url.includes("/api/join?") ? whole : {}), { status: 200 });
      }),
    );
    const s = await joinApi.inviteShare("192.168.1.100");
    expect(calls).toEqual([
      { addr: "192.168.1.100", share: true },
      { addr: "192.168.1.100", internet: true },
    ]);
    expect(shareOf(s!.invite!)).toEqual({ url: NET_URL, works: "anywhere", why: null });
  });

  it("the home network's invite never asks for internet", async () => {
    const calls = fake(200, { key: KEY });
    await joinApi.invite("192.168.1.100");
    expect(calls[0].body).toEqual({ addr: "192.168.1.100" });
  });

  it("Invites work goes as the word and as the older on/off (an engine from before the word)", async () => {
    expect(invitesBody("wifi")).toEqual({ invites: "wifi", internet_invites: false });
    const calls = fake(200, {});
    await joinApi.netSettings("192.168.1.100", invitesBody("anywhere"));
    expect(calls[0].body).toEqual({ addr: "192.168.1.100", invites: "anywhere", internet_invites: true });
  });

  it("saves the settings and says which one the engine refused", async () => {
    const calls = fake(400, { error: "stun" });
    expect(await joinApi.netSettings("192.168.1.100", { stun: ["x"] })).toEqual({ error: "stun" });
    expect(calls[0].url).toMatch(/\/api\/join\/settings$/);
  });
});

describe("the settings' checks (as the engine's)", () => {
  it("hub: wss://, ws:// only to this computer", () => {
    expect(hubUrlOk("wss://hub.phantomptr.com/ws")).toBe(true);
    expect(hubUrlOk("ws://127.0.0.1:8787/ws")).toBe(true);
    expect(hubUrlOk("ws://localhost:8787/ws")).toBe(true);
    expect(hubUrlOk("ws://[::1]:8787/ws")).toBe(true);
    expect(hubUrlOk("ws://hub.example.com/ws")).toBe(false);
    expect(hubUrlOk("https://hub.example.com/ws")).toBe(false);
    expect(hubUrlOk("hub.example.com")).toBe(false);
  });

  it("STUN: host:port per line, stun: dropped, at most 8", () => {
    expect(parseStun("stun:stun.cloudflare.com:3478\n\n stun.l.google.com:19302 ")).toEqual([
      "stun.cloudflare.com:3478",
      "stun.l.google.com:19302",
    ]);
    expect(parseStun("")).toEqual([]);
    expect(parseStun("stun.example.com")).toBeNull();
    expect(parseStun("bad host:3478")).toBeNull();
    expect(parseStun("a.example:0")).toBeNull();
    expect(parseStun(Array.from({ length: 9 }, (_, i) => `s${i}.example:3478`).join("\n"))).toBeNull();
  });
});

describe("what the host is told", () => {
  it("internet invites off, a bad hub address, other refusals", () => {
    expect(netInviteNote(tr, "internet_off", null)?.id).toBe("pt-net-off");
    expect(netInviteNote(tr, "internet_off", null)?.text).toContain("Settings › App › Play Together");
    expect(netInviteNote(tr, "hub_url", null)?.id).toBe("pt-net-hub-url");
    expect(netInviteNote(tr, "no_certificate", null)?.text).toContain("(no_certificate)");
  });

  it("the hub unreachable, from the engine's state", () => {
    const down = netInviteNote(tr, null, { url: NET_URL, hubState: "reconnecting", hubError: "connect: refused", offline: true });
    expect(down?.id).toBe("pt-net-hub-down");
    expect(down?.text).toContain("(connect: refused)");
    expect(netInviteNote(tr, null, { url: NET_URL, hubState: "connecting", hubError: null, offline: false })?.tone).toBe("muted");
    expect(netInviteNote(tr, null, { url: NET_URL, hubState: "hosted", hubError: null, offline: false })).toBeNull();
  });

  it("peers: those connecting, one failure notice until dismissed", () => {
    const peers = readJoinState(engine).netPeers;
    const v = netPeersView(peers, new Set());
    expect(v.connecting.map((p) => p.gid)).toEqual(["g2"]);
    expect(v.failed.map((p) => p.gid)).toEqual(["g3"]);
    expect(netPeersView(peers, new Set(["g3"])).failed).toEqual([]);
  });
});

describe("the network choice", () => {
  it("offers Internet beside this network, with its one-line explanation when picked", () => {
    const props = {
      urls: engine.invite.urls,
      tailscale: null,
      current: engine.invite.url,
      onPick: () => {},
      otherNetwork: "",
      testId: "pt-addresses",
      addressTestId: "pt-address",
    };
    const off = renderToStaticMarkup(<NetworkChoice {...props} internet={{ on: false, onPick: () => {} }} />);
    expect(off).toContain('data-testid="pt-addresses-internet"');
    expect(off).toMatch(/aria-pressed="true"[^>]*data-testid="pt-addresses-home"/);
    expect(off).not.toContain("Works from anywhere");
    // The support line is for Internet only.
    expect(off).not.toContain("pt-addresses-internet-support");
    const on = renderToStaticMarkup(<NetworkChoice {...props} internet={{ on: true, onPick: () => {} }} />);
    expect(on).toMatch(/aria-pressed="true"[^>]*data-testid="pt-addresses-internet"/);
    expect(on).toContain("Works from anywhere: your friend&#x27;s browser connects straight to this computer.");
    expect(on).toContain('data-testid="pt-addresses-internet-support"');
    expect(on).toContain("a donation keeps it going");
    expect(on).toMatch(/data-testid="pt-addresses-internet-donate">Support PS5Upload</);
    // Never an amount.
    expect(on).not.toMatch(/[$€£]\s?\d/);
    // No internet offered and one address: nothing to choose.
    expect(renderToStaticMarkup(<NetworkChoice {...props} />)).toBe("");
  });

  it("remembers the last choice on this device", () => {
    installWindowStub();
    expect(savedNetwork()).toBe("home");
    saveNetwork("internet");
    expect(savedNetwork()).toBe("internet");
  });
});

describe("the panel", () => {
  const panel = (state = readJoinState(engine)) =>
    renderToStaticMarkup(
      <PlayTogetherPanel addr="192.168.1.100" state={state} onState={() => {}} onClose={() => {}} onPhones={() => {}} />,
    );
  const esc = (u: string) => u.replace(/&/g, "&amp;");

  it("one link: the internet one with its QR code, no Local or Internet choice", () => {
    // A choice remembered from before one invite changes nothing.
    for (const saved of ["internet", "home"]) {
      installWindowStub({ "ps5upload.pt_network": saved });
      const out = panel();
      expect(out).toContain(`data-testid="pt-url">${esc(NET_URL)}<`);
      expect(out).toContain('data-testid="pt-qr"');
      expect(out).toContain('data-testid="pt-works-anywhere"');
      // #720: said plainly that Tailscale is nothing to set up for it.
      expect(out).toContain('data-testid="pt-no-tailscale"');
      expect(out).toContain('data-testid="pt-addresses-internet-hint"');
      expect(out).not.toContain('data-testid="pt-addresses-home"');
      expect(out).not.toContain('data-testid="pt-addresses-internet"');
      expect(out).not.toContain("pt-wifi-only");
      // The short code still works for a friend on the same network (Play › Join a friend).
      expect(out).toContain('data-testid="pt-code"');
    }
  });

  it("each guest's connection check, and a lower rate for everyone once one stays under it", () => {
    installWindowStub();
    const track = (o: Record<string, unknown>) => ({
      frames_sent: 1,
      bwe_kbps: 30000,
      stream_kbps: 15000,
      slow: false,
      measuring: false,
      under_s: 0,
      fit_kbps: 25000,
      loss: 0,
      rtcp_rtt_ms: 20,
      ...o,
    });
    const s = readJoinState({
      ...engine,
      guests: [
        { ...engine.guests[0], link: { path: "lan", rtt_ms: 4, track: track({}) } },
        { ...engine.guests[2], link: { path: "internet", rtt_ms: 55, track: track({ measuring: true }) } },
      ],
    });
    let out = panel(s);
    expect(out).toMatch(/data-testid="pt-player-link" data-grade="good">Good connection, about 30 Mbps</);
    expect(out).toContain('data-grade="measuring"');
    expect(out).not.toContain("pt-lower");
    // Cy's estimate has stayed under the rate for 12 s: 4.5 Mbps fits with room (offered, not done).
    out = panel(
      readJoinState({
        ...engine,
        guests: [{ ...engine.guests[2], link: { path: "internet", rtt_ms: 55, track: track({ bwe_kbps: 6000, fit_kbps: 5000, under_s: 12 }) } }],
      }),
    );
    expect(out).toContain('data-testid="pt-lower" data-to="4500"');
    expect(out).toContain("Lower the stream to 4.5 Mbps so everyone keeps up?");
    expect(out).not.toContain('data-testid="pt-slow"');
  });

  it("no internet right now: the Wi-Fi's link, said to work on the Wi-Fi only", () => {
    installWindowStub();
    const offline = readJoinState({
      ...engine,
      invite: {
        ...engine.invite,
        share: { url: engine.invite.url, works: "wifi", why: "offline" },
        internet: { ...engine.invite.internet, state: "reconnecting", error: "connect: refused", offline: true },
      },
    });
    const out = panel(offline);
    expect(out).toContain(`data-testid="pt-url">${engine.invite.url}<`);
    expect(out).toMatch(/data-testid="pt-wifi-only" data-why="offline">Works on your Wi-Fi only \(no internet right now\)\.</);
    expect(out).not.toContain("pt-works-anywhere");
    expect(out).not.toContain("pt-no-tailscale");
    // The hub's own warning is not repeated.
    expect(out).not.toContain("pt-net-hub-down");
  });

  it("invites only on the Wi-Fi: the LAN link and the way to change it", () => {
    installWindowStub();
    const wifi = readJoinState({
      ...engine,
      settings: { ...engine.settings, internet_invites: false, invites: "wifi" },
      invite: { ...engine.invite, share: { url: engine.invite.url, works: "wifi", why: "setting" }, internet: null },
    });
    const out = panel(wifi);
    expect(out).toContain(`data-testid="pt-url">${engine.invite.url}<`);
    expect(out).toMatch(/data-testid="pt-wifi-only" data-why="setting"/);
    expect(out).toContain("Settings › App › Play Together");
    expect(out).not.toContain("pt-addresses-internet");
  });

  it("each friend: how they are connected and the delay; connecting peers; one dismissible failure", () => {
    installWindowStub();
    const out = panel();
    expect(out).toMatch(/data-testid="pt-player-via" data-path="direct" data-way="lan">Home network</);
    expect(out).toMatch(/data-testid="pt-player-via" data-path="" data-way="lan">Home network</);
    expect(out).toMatch(/data-testid="pt-player-via" data-path="direct" data-way="internet">Internet direct</);
    expect(out).toContain(">48 ms<");
    expect(out).toContain(">61 ms<");
    expect(out.match(/data-testid="pt-net-peer"/g)).toHaveLength(1);
    expect(out).toContain("Connecting…");
    expect(out.match(/data-testid="pt-net-peer-failed"/g)).toHaveLength(1);
    expect(out).toContain("Couldn&#x27;t connect from their network");
    expect(out).toContain('data-testid="pt-net-peer-failed-dismiss"');
  });

  it("how a friend is connected, in words (an older engine: Internet · Direct, nothing for the LAN)", () => {
    const g = readJoinState(engine).guests[0];
    expect(guestWayText(tr, { ...g, netPath: "tailscale" })).toBe("Tailscale");
    expect(guestWayText(tr, { ...g, netPath: null })).toBe("Internet · Direct");
    expect(guestWayText(tr, { ...g, netPath: null, path: "connecting" })).toBe("Connecting…");
    expect(guestWayText(tr, { ...g, via: "lan", path: null, netPath: null })).toBeNull();
  });

  it("an older engine offers no Internet", () => {
    installWindowStub({ "ps5upload.pt_network": "internet" });
    const old = readJoinState({ ...engine, settings: { block_system_buttons: true, max_players: 3 }, internet: null, invite: { ...engine.invite, share: undefined, internet: null } });
    const out = panel(old);
    expect(out).not.toContain("pt-addresses-internet");
    expect(out).toContain(`data-testid="pt-url">${engine.invite.url}<`);
    expect(out).toContain('data-testid="pt-tailscale"');
  });
});
